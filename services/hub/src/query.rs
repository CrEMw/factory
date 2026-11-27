//! Lists for pages and MCP: server-side filters, pages, counters.
//!
//! GET /api/ticket-page:  a page of tickets with filters: { total, items }; with ready=true also "ready": true,
//!                        with assignee or component also those fields (MCP tells the new hub from the old one by them)
//! GET /api/ticket-stats — counters by status, priority and top tags (without ticket rows)
//! GET /api/document-page — a page of documents: { total, roles, repos, items }; repo and include_retired
//!
//! The old /api/tickets and /api/documents stay as they were (full lists).

use anyhow::Context;
use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, QueryBuilder};
use tracing::Instrument;

use crate::{
    AppError, bad,
    config::local_date,
    non_empty,
    people::{valid_handle, valid_slug},
    telemetry, valid_priority, valid_project, valid_status,
};

pub const MAX_LIMIT: i64 = 500;
pub const DEFAULT_LIMIT: i64 = 100;

#[derive(Deserialize, Default)]
pub struct PageQuery {
    pub project: Option<String>,
    /// Comma separated: open,doing,done,cancelled; all or empty means any.
    pub status: Option<String>,
    /// Comma separated: p0..p3 and none (no priority).
    pub priority: Option<String>,
    pub tag: Option<String>,
    /// Text: an exact code, a summary substring or an exact tag.
    pub q: Option<String>,
    /// seq (newest first), updated (recently changed), prio (p0 → p3, then newest).
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// true: only tickets ready for work (without "not before" until stage 9): open, no confirmed
    /// live part_of subtasks and blocks blockers in status open or doing, wherever they are in the graph.
    pub ready: Option<String>,
    /// Assignee handle or none: no assignee.
    pub assignee: Option<String>,
    /// Component slug.
    pub component: Option<String>,
}

/// Assignee in a filter: a person by handle or "no assignee".
#[derive(Debug, PartialEq, Clone)]
pub enum Assignee {
    Person(String),
    Nobody,
}

/// Checked filters: built once and turned into a where clause.
#[derive(Debug, PartialEq, Default)]
pub struct Filters {
    pub project: Option<String>,
    pub statuses: Vec<String>,
    pub priorities: Vec<String>,
    pub no_priority: bool,
    pub tag: Option<String>,
    pub q: Option<String>,
    pub ready: bool,
    pub assignee: Option<Assignee>,
    pub component: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Sort {
    Seq,
    Updated,
    Prio,
}

impl Sort {
    fn order_by(self) -> &'static str {
        match self {
            Sort::Seq => " order by seq desc",
            Sort::Updated => " order by updated_at desc, seq desc",
            Sort::Prio => " order by priority asc nulls last, seq desc",
        }
    }
}

fn list(v: Option<&str>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

pub fn parse(q: &PageQuery) -> Result<(Filters, Sort, i64, i64), AppError> {
    let project = non_empty(q.project.as_deref()).map(str::to_owned);
    if project.as_deref().is_some_and(|p| !valid_project(p)) {
        return Err(bad("bad project name"));
    }
    let mut statuses = list(q.status.as_deref());
    if statuses.iter().any(|s| s == "all") {
        statuses.clear();
    } else if statuses.iter().any(|s| !valid_status(s)) {
        return Err(bad(
            "status: open, doing, done, cancelled or all, comma separated",
        ));
    }
    let mut no_priority = false;
    let mut priorities = Vec::new();
    for p in list(q.priority.as_deref()) {
        if p == "none" {
            no_priority = true;
        } else if valid_priority(&p) {
            priorities.push(p);
        } else {
            return Err(bad("priority: p0, p1, p2, p3 or none, comma separated"));
        }
    }
    let tag = non_empty(q.tag.as_deref()).map(str::to_owned);
    let text = non_empty(q.q.as_deref()).map(str::to_owned);
    if text.as_deref().is_some_and(|t| t.len() > 200) {
        return Err(bad("q: up to 200 bytes"));
    }
    let sort = match non_empty(q.sort.as_deref()) {
        None | Some("seq") => Sort::Seq,
        Some("updated") => Sort::Updated,
        Some("prio") => Sort::Prio,
        Some(_) => return Err(bad("sort: seq, updated or prio")),
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad("limit: 1 to 500"));
    }
    let offset = q.offset.unwrap_or(0);
    if offset < 0 {
        return Err(bad("offset: 0 or more"));
    }
    let ready = match non_empty(q.ready.as_deref()) {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        Some(_) => return Err(bad("ready: true or false")),
    };
    let assignee = match non_empty(q.assignee.as_deref()).map(str::to_lowercase) {
        None => None,
        Some(h) if h == "none" => Some(Assignee::Nobody),
        Some(h) if valid_handle(&h) => Some(Assignee::Person(h)),
        Some(_) => return Err(bad("assignee: handle or none")),
    };
    let component = non_empty(q.component.as_deref()).map(str::to_lowercase);
    if component.as_deref().is_some_and(|c| !valid_slug(c)) {
        return Err(bad("component: slug ^[a-z0-9][a-z0-9-]{0,39}$"));
    }
    Ok((
        Filters {
            project,
            statuses,
            priorities,
            no_priority,
            tag,
            q: text,
            ready,
            assignee,
            component,
        },
        sort,
        limit,
        offset,
    ))
}

/// where conditions from the filters; `skip_status` is for the per-status counters.
fn push_where(b: &mut QueryBuilder<'_, Postgres>, f: &Filters, skip_status: bool) {
    b.push(" where true");
    if let Some(p) = &f.project {
        b.push(" and project = ").push_bind(p.clone());
    }
    if !skip_status && !f.statuses.is_empty() {
        b.push(" and status = any(")
            .push_bind(f.statuses.clone())
            .push(")");
    }
    match (f.priorities.is_empty(), f.no_priority) {
        (true, false) => {}
        (false, false) => {
            b.push(" and priority = any(")
                .push_bind(f.priorities.clone())
                .push(")");
        }
        (true, true) => {
            b.push(" and priority is null");
        }
        (false, true) => {
            b.push(" and (priority is null or priority = any(")
                .push_bind(f.priorities.clone())
                .push("))");
        }
    }
    if let Some(t) = &f.tag {
        b.push(" and ").push_bind(t.clone()).push(" = any(tags)");
    }
    if let Some(q) = &f.q {
        // The code is compared exactly (lowercase), the summary as a case-insensitive substring, the tag exactly.
        let like = format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        b.push(" and (code = ")
            .push_bind(q.to_lowercase())
            .push(" or summary ilike ")
            .push_bind(like)
            .push(" or ")
            .push_bind(q.clone())
            .push(" = any(tags))");
    }
    if f.ready {
        b.push(READY);
    }
    match &f.assignee {
        None => {}
        Some(Assignee::Person(h)) => {
            b.push(ASSIGNED)
                .push(" and gp.handle = ")
                .push_bind(h.clone())
                .push(")");
        }
        Some(Assignee::Nobody) => {
            b.push(" and not").push(&ASSIGNED[" and".len()..]).push(")");
        }
    }
    if let Some(c) = &f.component {
        b.push(IN_COMPONENT).push_bind(c.clone()).push(")");
    }
}

/// The ticket's live confirmed assignee; push_where appends the condition on gp and closes the bracket itself.
const ASSIGNED: &str = " and exists (select 1 from graph.ticket gt \
    join graph.edge e on e.src = gt.node_id and e.rel = 'assigned_to' \
     and e.state = 'confirmed' and e.valid_to is null \
    join graph.person gp on gp.node_id = e.dst \
    where gt.ticket_seq = tickets.seq";

/// Ticket in a component by slug: a live confirmed in_component edge.
const IN_COMPONENT: &str = " and exists (select 1 from graph.ticket gt \
    join graph.edge e on e.src = gt.node_id and e.rel = 'in_component' \
     and e.state = 'confirmed' and e.valid_to is null \
    join graph.component gc on gc.node_id = e.dst \
    where gt.ticket_seq = tickets.seq and gc.slug = ";

/// Assignee and components of a list row from edges in one pass (spec "Ticket fields").
const FIELDS_JOIN: &str = " left join lateral (select gp.handle as assignee from graph.ticket gt \
    join graph.edge e on e.src = gt.node_id and e.rel = 'assigned_to' \
     and e.state = 'confirmed' and e.valid_to is null \
    join graph.person gp on gp.node_id = e.dst \
    where gt.ticket_seq = tickets.seq limit 1) fa on true \
    left join lateral (select array_agg(gc.slug order by gc.slug) as components from graph.ticket gt \
    join graph.edge e on e.src = gt.node_id and e.rel = 'in_component' \
     and e.state = 'confirmed' and e.valid_to is null \
    join graph.component gc on gc.node_id = e.dst \
    where gt.ticket_seq = tickets.seq) fc on true";

/// Ready for work: open; no confirmed live subtask (part_of into this ticket) and no confirmed
/// live blocker (blocks into this ticket) is open or doing; the ticket itself is not a duplicate (no live confirmed duplicates
/// edge from it). Blockers are taken from the whole graph, not the project. The set of "src before dst" rels (blocks, part_of) is the same
/// as in the cycle check in 0010_graph.sql; tools/test-graph.mjs checks the condition. Since stage 9 also
/// `start_not_before` of the live estimate is no later than today in installation local time (zone from public.instance,
/// step 2.5). The condition is also used by `/api/next` (plan.rs).
pub(crate) const READY: &str = concat!(
    " and status = 'open' and not exists (\
    select 1 from graph.ticket gt \
    join graph.edge e on e.dst = gt.node_id and e.rel in ('part_of', 'blocks') \
     and e.state = 'confirmed' and e.valid_to is null \
    join graph.ticket ot on ot.node_id = e.src \
    join tickets o on o.seq = ot.ticket_seq \
    where gt.ticket_seq = tickets.seq and o.status in ('open', 'doing')) \
    and not exists (select 1 from graph.ticket dt \
    join graph.edge d on d.src = dt.node_id and d.rel = 'duplicates' \
     and d.state = 'confirmed' and d.valid_to is null \
    where dt.ticket_seq = tickets.seq) \
    and not exists (select 1 from plan.ticket_estimate snb where snb.account = (select public.instance_account()) \
     and snb.ticket_seq = tickets.seq and snb.valid_to is null \
     and snb.start_not_before > ",
    local_date!("now()"),
    ")"
);

#[derive(Serialize, sqlx::FromRow)]
pub struct TicketItem {
    pub code: String,
    pub project: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Assignee handle.
    pub assignee: Option<String>,
    /// Component slugs.
    pub components: Vec<String>,
}

#[derive(Serialize)]
pub struct Page<T> {
    pub total: i64,
    pub items: Vec<T>,
    /// The ready filter was applied: MCP tells the new hub from the old one, which silently skips ready, by this field.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ready: bool,
    /// The applied assignee (handle or none) and component filter: likewise for MCP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
}

pub async fn ticket_page(db: &PgPool, q: &PageQuery) -> Result<Page<TicketItem>, AppError> {
    let (f, sort, limit, offset) = parse(q)?;
    let mut count = QueryBuilder::new("select count(*) from tickets");
    push_where(&mut count, &f, false);
    let (total,): (i64,) = count
        .build_query_as()
        .fetch_one(db)
        .instrument(telemetry::db_span("count tickets"))
        .await
        .context("count tickets")?;
    let mut rows = QueryBuilder::new(
        "select code, project, summary, status, priority, tags, updated_at, fa.assignee, \
         coalesce(fc.components, '{}') as components from tickets",
    );
    rows.push(FIELDS_JOIN);
    push_where(&mut rows, &f, false);
    rows.push(sort.order_by())
        .push(" limit ")
        .push_bind(limit)
        .push(" offset ")
        .push_bind(offset);
    let items = rows
        .build_query_as()
        .fetch_all(db)
        .instrument(telemetry::db_span("select ticket page"))
        .await
        .context("select ticket page")?;
    Ok(Page {
        total,
        items,
        ready: f.ready,
        assignee: f.assignee.map(|a| match a {
            Assignee::Person(h) => h,
            Assignee::Nobody => "none".to_owned(),
        }),
        component: f.component,
    })
}

pub async fn get_ticket_page(
    State(db): State<PgPool>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<TicketItem>>, AppError> {
    Ok(Json(ticket_page(&db, &q).await?))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Cell {
    pub status: String,
    pub priority: Option<String>,
    pub tickets: i64,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TagCount {
    pub tag: String,
    pub tickets: i64,
}

#[derive(Serialize)]
pub struct Stats {
    /// Number of tickets per (status, priority) with all filters except status.
    pub cells: Vec<Cell>,
    /// The ten most frequent tags among active (open, doing) tickets (with the same filters).
    pub active_tags: Vec<TagCount>,
}

pub async fn ticket_stats(db: &PgPool, q: &PageQuery) -> Result<Stats, AppError> {
    let (f, ..) = parse(q)?;
    // ready means open: per-status counters would silently shrink to one cell with it.
    if f.ready {
        return Err(bad("ready: not supported in ticket stats"));
    }
    let mut cells = QueryBuilder::new("select status, priority, count(*) as tickets from tickets");
    push_where(&mut cells, &f, true);
    cells.push(" group by status, priority order by status, priority nulls last");
    let cells = cells
        .build_query_as()
        .fetch_all(db)
        .instrument(telemetry::db_span("ticket stats"))
        .await
        .context("ticket stats")?;
    let mut tags =
        QueryBuilder::new("select tag, count(*) as tickets from tickets, unnest(tags) as tag");
    push_where(&mut tags, &f, true);
    tags.push(" and status in ('open', 'doing') group by tag order by count(*) desc, tag limit 10");
    let active_tags = tags
        .build_query_as()
        .fetch_all(db)
        .instrument(telemetry::db_span("active tags"))
        .await
        .context("active tags")?;
    Ok(Stats { cells, active_tags })
}

pub async fn get_ticket_stats(
    State(db): State<PgPool>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Stats>, AppError> {
    Ok(Json(ticket_stats(&db, &q).await?))
}

#[derive(Deserialize, Default)]
pub struct DocQuery {
    pub role: Option<String>,
    pub q: Option<String>,
    /// owner/name of a registry repository; empty: all repositories.
    pub repo: Option<String>,
    /// true: retired documents too (only live ones by default).
    pub include_retired: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct DocumentItem {
    pub repo: String,
    pub path: String,
    pub role: String,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
    pub tickets: i64,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct RoleCount {
    pub role: String,
    pub documents: i64,
}

/// A registry repository and its last sync from git: revision, time and problems of individual documents.
#[derive(Serialize, sqlx::FromRow)]
pub struct RepoState {
    pub repo: String,
    pub project: String,
    pub revision: Option<String>,
    pub synced_at: Option<chrono::DateTime<chrono::Utc>>,
    pub problems: serde_json::Value,
    /// Live documents in the repository.
    pub documents: i64,
}

#[derive(Serialize)]
pub struct DocPage {
    pub total: i64,
    /// Role counters with the same filters (except the role itself).
    pub roles: Vec<RoleCount>,
    pub repos: Vec<RepoState>,
    pub items: Vec<DocumentItem>,
}

/// Repository name owner/name, like the CHECK in `public.document_repository` (0017): the owner starts with a letter or digit. One check
/// for the request filter, `HUB_DEFAULT_DOCS_REPO` and the `repositories.toml` seed (documents.rs).
pub fn valid_repo(r: &str) -> bool {
    let Some((owner, name)) = r.split_once('/') else {
        return false;
    };
    (1..=39).contains(&owner.len())
        && (1..=100).contains(&name.len())
        && owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn flag(v: Option<&str>) -> Result<bool, AppError> {
    match non_empty(v) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(bad("include_retired: true or false")),
    }
}

pub async fn get_document_page(
    State(db): State<PgPool>,
    Query(q): Query<DocQuery>,
) -> Result<Json<DocPage>, AppError> {
    let role = non_empty(q.role.as_deref()).map(str::to_owned);
    let text = non_empty(q.q.as_deref()).map(str::to_owned);
    if text.as_deref().is_some_and(|t| t.len() > 200) {
        return Err(bad("q: up to 200 bytes"));
    }
    let repo = non_empty(q.repo.as_deref()).map(str::to_owned);
    if repo.as_deref().is_some_and(|r| !valid_repo(r)) {
        return Err(bad("repo: owner/name"));
    }
    let retired = flag(q.include_retired.as_deref())?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad("limit: 1 to 500"));
    }
    let offset = q.offset.unwrap_or(0);
    if offset < 0 {
        return Err(bad("offset: 0 or more"));
    }
    let filter = |b: &mut QueryBuilder<'_, Postgres>, with_role: bool| {
        b.push(" where true");
        if with_role && let Some(r) = &role {
            b.push(" and d.role = ").push_bind(r.clone());
        }
        if let Some(r) = &repo {
            b.push(" and d.repo = ").push_bind(r.clone());
        }
        if !retired {
            b.push(" and d.retired_at is null");
        }
        if let Some(t) = &text {
            let like = format!(
                "%{}%",
                t.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            b.push(" and (d.title ilike ")
                .push_bind(like.clone())
                .push(" or d.path ilike ")
                .push_bind(like)
                .push(")");
        }
    };
    let mut count = QueryBuilder::new("select count(*) from documents d");
    filter(&mut count, true);
    let (total,): (i64,) = count
        .build_query_as()
        .fetch_one(&db)
        .instrument(telemetry::db_span("count documents"))
        .await
        .context("count documents")?;
    let mut rows = QueryBuilder::new(
        "select d.repo, d.path, d.role, d.title, d.created_at, d.retired_at, \
                (select count(*) from ticket_documents l \
                  where l.repo = d.repo and l.path = d.path and l.valid_to is null) as tickets \
         from documents d",
    );
    filter(&mut rows, true);
    rows.push(" order by d.created_at desc, d.repo, d.path limit ")
        .push_bind(limit)
        .push(" offset ")
        .push_bind(offset);
    let items = rows
        .build_query_as()
        .fetch_all(&db)
        .instrument(telemetry::db_span("select document page"))
        .await
        .context("select document page")?;
    let mut roles = QueryBuilder::new("select d.role, count(*) as documents from documents d");
    filter(&mut roles, false);
    roles.push(" group by d.role order by d.role");
    let roles = roles
        .build_query_as()
        .fetch_all(&db)
        .instrument(telemetry::db_span("document roles"))
        .await
        .context("document roles")?;
    let repos = sqlx::query_as(
        "select r.repo, r.project, s.revision, s.synced_at, coalesce(s.problems, '[]'::jsonb) as problems, \
                (select count(*) from documents d where d.repo = r.repo and d.retired_at is null) as documents \
         from document_repository r left join repository_document_sync_state s on s.repo = r.repo \
         order by r.repo",
    )
    .fetch_all(&db)
    .instrument(telemetry::db_span("document repositories"))
    .await
    .context("document repositories")?;
    Ok(Json(DocPage {
        total,
        roles,
        repos,
        items,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> PageQuery {
        let mut p = PageQuery::default();
        for kv in s.split('&').filter(|x| !x.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap();
            let raw = v;
            let v = Some(v.to_owned());
            match k {
                "project" => p.project = v,
                "status" => p.status = v,
                "priority" => p.priority = v,
                "tag" => p.tag = v,
                "q" => p.q = v,
                "sort" => p.sort = v,
                "limit" => p.limit = raw.parse().ok(),
                "offset" => p.offset = raw.parse().ok(),
                "ready" => p.ready = v,
                "assignee" => p.assignee = v,
                "component" => p.component = v,
                _ => unreachable!(),
            }
        }
        p
    }

    #[test]
    fn repo_names_and_retired_flag() {
        for ok in ["demo/docs", "demo/hub", "acme/widgets", "a/b.c_d-e"] {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in [
            "", "acme", "/x", "-a/b", "a/", "a/b/c", "a b/c", "a/b'c", "a/b c",
        ] {
            assert!(!valid_repo(bad), "{bad}");
        }
        assert!(
            !flag(None).unwrap() && !flag(Some("false")).unwrap() && flag(Some("true")).unwrap()
        );
        assert!(flag(Some("yes")).is_err());
    }

    #[test]
    fn filters_parse() {
        let (f, sort, limit, offset) =
            parse(&q("status=open,doing&priority=p0,none&sort=prio")).unwrap();
        assert_eq!(f.statuses, vec!["open", "doing"]);
        assert_eq!(f.priorities, vec!["p0"]);
        assert!(f.no_priority);
        assert_eq!((sort, limit, offset), (Sort::Prio, DEFAULT_LIMIT, 0));
        assert!(parse(&q("status=all")).unwrap().0.statuses.is_empty());
        assert_eq!(
            parse(&q("status=cancelled")).unwrap().0.statuses,
            vec!["cancelled"]
        );
        for bad in [
            "status=closed",
            "priority=p9",
            "sort=name",
            "limit=0",
            "limit=501",
            "offset=-1",
            "project=Bad",
            "ready=yes",
            "assignee=Alice%20B",
            "assignee=1ed",
            "component=a_b",
        ] {
            assert!(parse(&q(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn where_clause_binds_every_value() {
        let (f, ..) = parse(&q(
            "project=demo&status=open&priority=p1,none&tag=bug&q=50%_x",
        ))
        .unwrap();
        let mut b = QueryBuilder::<Postgres>::new("select 1 from tickets");
        push_where(&mut b, &f, false);
        let sql = b.sql();
        assert_eq!(
            sql,
            "select 1 from tickets where true and project = $1 and status = any($2) \
             and (priority is null or priority = any($3)) and $4 = any(tags) \
             and (code = $5 or summary ilike $6 or $7 = any(tags))"
        );
    }

    #[test]
    fn ready_adds_the_graph_condition() {
        let (f, ..) = parse(&q("project=acme&ready=true")).unwrap();
        assert!(f.ready);
        assert!(!parse(&q("ready=false")).unwrap().0.ready);
        let mut b = QueryBuilder::<Postgres>::new("select 1 from tickets");
        push_where(&mut b, &f, false);
        let sql = b.sql();
        assert!(
            sql.starts_with("select 1 from tickets where true and project = $1 and status = 'open' and not exists ("),
            "{sql}"
        );
        assert!(sql.contains("gt.ticket_seq = tickets.seq and o.status in ('open', 'doing'))"));
    }

    #[test]
    fn assignee_and_component_add_graph_conditions() {
        let (f, ..) = parse(&q("assignee=ALICE&component=hub")).unwrap();
        assert_eq!(f.assignee, Some(Assignee::Person("alice".into())));
        assert_eq!(f.component.as_deref(), Some("hub"));
        let mut b = QueryBuilder::<Postgres>::new("select 1 from tickets");
        push_where(&mut b, &f, false);
        let sql = b.sql();
        assert!(
            sql.contains("and exists (select 1 from graph.ticket gt"),
            "{sql}"
        );
        assert!(
            sql.contains("where gt.ticket_seq = tickets.seq and gp.handle = $1)"),
            "{sql}"
        );
        assert!(
            sql.ends_with("where gt.ticket_seq = tickets.seq and gc.slug = $2)"),
            "{sql}"
        );
        let (f, ..) = parse(&q("assignee=none")).unwrap();
        assert_eq!(f.assignee, Some(Assignee::Nobody));
        let mut b = QueryBuilder::<Postgres>::new("select 1 from tickets");
        push_where(&mut b, &f, false);
        let sql = b.sql();
        assert!(
            sql.starts_with(
                "select 1 from tickets where true and not exists (select 1 from graph.ticket gt"
            ),
            "{sql}"
        );
        assert!(sql.ends_with("where gt.ticket_seq = tickets.seq)"), "{sql}");
    }
}
