//! People, business components and ticket fields (spec "Ticket fields", "API and MCP", "Migration").
//!
//! GET  /api/people                 people with ticket counts (total and open or doing)
//! POST /api/people                 `{handle, name, email?, aliases?, is_agent?}` -> 201
//! GET  /api/components             components with a project and a ticket count
//! POST /api/components             `{slug, title, description?, project?}` -> 201
//! POST /api/tickets/{code}/fields  `{assignee?: handle|null, components?: slug[]}` -> ticket fields
//!
//! Assignee and components live as graph edges `assigned_to` (one live) and `in_component`; tickets has no columns for them.
//! A new assignee closes the previous one with `superseded` in the same transaction (trigger `edge_c_supersede`,
//! 0012_people_components.sql). `components` sets the whole set: extra live edges are closed (`removed`),
//! missing ones are added. An agent cannot remove an assignee or a component set by a human: 403
//! `close_requires_human` from the database. A field update takes the ticket row lock (like PATCH) and moves
//! `updated_at`, so two concurrent updates run one after the other instead of mixing sets. A retired
//! person or component stays while it is already on the ticket (the form sends it back) and can be removed; it cannot be
//! set again (409 `retired`). Only a human adds people and components (`human`, hub-web form with a session): otherwise
//! 403 `requires_human`. JSON error responses: 404 `unknown_person`, `unknown_component`, 409 `exists`,
//! `email_taken`, `retired`.
//!
//! The `people.toml` and `components.toml` seeds from the `HUB_SEED_DIR` directory (seed.rs) are applied at startup after the
//! old Roadmap snapshot ([`seed`]); the import is idempotent, the import functions in the database set the actor (`seed:people`,
//! `seed:components`).

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::Instrument;

use crate::{
    AppError, bad,
    graph::db_error,
    seed::Seeds,
    telemetry,
    tx::{self, Actor},
    valid_project,
};

/// Maximum number of components on a ticket in one update.
pub const MAX_COMPONENTS: usize = 20;

/// Value of the `assignee=none` filter (no assignee): a handle with this name is forbidden in the database too.
pub const NO_ASSIGNEE: &str = "none";

/// Person handle: `^[a-z][a-z0-9-]{0,39}$` and not [`NO_ASSIGNEE`], like the CHECK in graph.person.
pub fn valid_handle(h: &str) -> bool {
    let b = h.as_bytes();
    h != NO_ASSIGNEE
        && (1..=40).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Component slug: `^[a-z0-9][a-z0-9-]{0,39}$`, like the CHECK in graph.component.
pub fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Address: one `@`, no spaces, up to 320 bytes (like the database CHECK; delivery is not checked).
fn valid_email(e: &str) -> bool {
    let mut parts = e.split('@');
    let ok =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && !p.chars().any(char::is_whitespace));
    e.len() <= 320 && ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// Text for a name or title: 1..=max characters without control characters.
fn valid_text(s: &str, max: usize) -> bool {
    let n = s.chars().count();
    (1..=max).contains(&n) && !s.chars().any(char::is_control)
}

fn err(status: StatusCode, body: serde_json::Value) -> AppError {
    AppError::Json(status, body)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Person {
    pub handle: String,
    pub name: String,
    pub email: Option<String>,
    pub aliases: Vec<String>,
    pub is_agent: bool,
    pub retired: bool,
    /// Live confirmed assignments: total and among open or doing.
    pub tickets: i64,
    pub active: i64,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Component {
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    /// Project from the live component_of edge.
    pub project: Option<String>,
    pub retired: bool,
    pub tickets: i64,
    pub active: i64,
}

#[derive(Deserialize, Serialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NewPerson {
    pub handle: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub is_agent: bool,
}

#[derive(Deserialize, Serialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NewComponent {
    pub slug: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Seed only: tags by which tickets get the component on import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl NewPerson {
    pub fn check(&mut self) -> Result<(), &'static str> {
        self.handle = self.handle.trim().to_owned();
        self.name = self.name.trim().to_owned();
        self.email = self
            .email
            .take()
            .map(|e| e.trim().to_owned())
            .filter(|e| !e.is_empty());
        self.aliases = self.aliases.iter().map(|a| a.trim().to_owned()).collect();
        if !valid_handle(&self.handle) {
            return Err("handle: ^[a-z][a-z0-9-]{0,39}$, not none");
        }
        if !valid_text(&self.name, 200) {
            return Err("name: 1 to 200 characters");
        }
        if self.email.as_deref().is_some_and(|e| !valid_email(e)) {
            return Err("email: name@domain");
        }
        if self.aliases.len() > 50 || self.aliases.iter().any(|a| !valid_text(a, 200)) {
            return Err("aliases: up to 50, each 1 to 200 characters");
        }
        Ok(())
    }
}

impl NewComponent {
    pub fn check(&mut self) -> Result<(), &'static str> {
        self.slug = self.slug.trim().to_owned();
        self.title = self.title.trim().to_owned();
        self.description = self
            .description
            .take()
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty());
        self.project = self
            .project
            .take()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty());
        if !valid_slug(&self.slug) {
            return Err("slug: ^[a-z0-9][a-z0-9-]{0,39}$");
        }
        if !valid_text(&self.title, 200) {
            return Err("title: 1 to 200 characters");
        }
        if self
            .description
            .as_deref()
            .is_some_and(|d| d.chars().count() > 2000)
        {
            return Err("description: up to 2000 characters");
        }
        if self.project.as_deref().is_some_and(|p| !valid_project(p)) {
            return Err("project: lowercase latin letters, digits and dashes, up to 40");
        }
        if self.tags.len() > 30
            || self
                .tags
                .iter()
                .any(|t| t.trim().is_empty() || t.len() > 60)
        {
            return Err("tags: up to 30, each 1 to 60 bytes");
        }
        Ok(())
    }
}

/// Ticket fields in an update: `assignee` absent (leave as is), null (clear) or a handle; `components` is the whole set.
#[derive(Deserialize, Default, Debug, PartialEq)]
pub struct Fields {
    #[serde(default, deserialize_with = "crate::some_or_null")]
    pub assignee: Option<Option<String>>,
    #[serde(default)]
    pub components: Option<Vec<String>>,
}

impl Fields {
    pub fn is_empty(&self) -> bool {
        self.assignee.is_none() && self.components.is_none()
    }

    /// Check before the database: handle and slug by the database rules, components without duplicates and no more than [`MAX_COMPONENTS`].
    /// An empty handle equals null (clear the assignee).
    pub fn check(&mut self) -> Result<(), AppError> {
        if let Some(a) = &mut self.assignee {
            *a = a
                .take()
                .map(|h| h.trim().to_lowercase())
                .filter(|h| !h.is_empty());
            if a.as_deref().is_some_and(|h| !valid_handle(h)) {
                return Err(bad(
                    "assignee: handle ^[a-z][a-z0-9-]{0,39}$ (not none) or null",
                ));
            }
        }
        if let Some(list) = &mut self.components {
            let mut out: Vec<String> = Vec::with_capacity(list.len());
            for s in list.iter() {
                let s = s.trim().to_lowercase();
                if !valid_slug(&s) {
                    return Err(bad("components: slugs ^[a-z0-9][a-z0-9-]{0,39}$"));
                }
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            if out.len() > MAX_COMPONENTS {
                return Err(bad("components: up to 20"));
            }
            out.sort();
            *list = out;
        }
        Ok(())
    }
}

/// Assignee and components of a ticket: in ticket and field update responses.
#[derive(Serialize, Debug, Default, PartialEq)]
pub struct TicketFields {
    pub assignee: Option<String>,
    pub components: Vec<String>,
}

/// Current ticket fields by seq from live confirmed edges.
pub async fn ticket_fields<'e, E>(db: E, seq: i64) -> Result<TicketFields, AppError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let (assignee, components): (Option<String>, Vec<String>) = sqlx::query_as(
        "select (select gp.handle from graph.ticket gt \
                 join graph.edge e on e.src = gt.node_id and e.rel = 'assigned_to' \
                  and e.state = 'confirmed' and e.valid_to is null \
                 join graph.person gp on gp.node_id = e.dst \
                 where gt.ticket_seq = $1 limit 1), \
                coalesce((select array_agg(gc.slug order by gc.slug) from graph.ticket gt \
                 join graph.edge e on e.src = gt.node_id and e.rel = 'in_component' \
                  and e.state = 'confirmed' and e.valid_to is null \
                 join graph.component gc on gc.node_id = e.dst \
                 where gt.ticket_seq = $1), '{}')",
    )
    .bind(seq)
    .fetch_one(db)
    .instrument(telemetry::db_span("select ticket fields"))
    .await
    .context("select ticket fields")?;
    Ok(TicketFields {
        assignee,
        components,
    })
}

/// Person or component node by key: id and whether it is retired.
async fn attr_node(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    key: &str,
) -> Result<Option<(i64, bool)>, AppError> {
    let table = if kind == "person" {
        "graph.person a on a.node_id = n.id and a.handle = $2"
    } else {
        "graph.component a on a.node_id = n.id and a.slug = $2"
    };
    Ok(sqlx::query_as(&format!(
        "select n.id, n.retired_at is not null from graph.node n join {table} \
         where n.account = (select public.instance_account()) and n.kind = $1 and n.key = $2"
    ))
    .bind(kind)
    .bind(key)
    .fetch_optional(&mut **tx)
    .instrument(telemetry::db_span("select attr node"))
    .await
    .context("select attr node")?)
}

async fn live_edges(
    tx: &mut Transaction<'_, Postgres>,
    src: i64,
    rel: &str,
) -> Result<Vec<(i64, i64)>, AppError> {
    Ok(sqlx::query_as(
        "select id, dst from graph.edge where account = (select public.instance_account()) and src = $1 and rel = $2 \
         and state = 'confirmed' and valid_to is null order by id",
    )
    .bind(src)
    .bind(rel)
    .fetch_all(&mut **tx)
    .instrument(telemetry::db_span("select live edges"))
    .await
    .context("select live edges")?)
}

async fn insert_edge(
    tx: &mut Transaction<'_, Postgres>,
    src: i64,
    rel: &str,
    dst: i64,
    dst_kind: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "insert into graph.edge (src, src_kind, rel, dst, dst_kind) values ($1, 'ticket', $2, $3, $4)",
    )
    .bind(src)
    .bind(rel)
    .bind(dst)
    .bind(dst_kind)
    .execute(&mut **tx)
    .instrument(telemetry::db_span("insert field edge"))
    .await
    .map_err(|e| db_error(e, "insert field edge"))?;
    Ok(())
}

async fn close_edge(tx: &mut Transaction<'_, Postgres>, id: i64) -> Result<(), AppError> {
    sqlx::query(
        "update graph.edge set valid_to = now(), close_reason = 'removed' where id = $1 and valid_to is null",
    )
    .bind(id)
    .execute(&mut **tx)
    .instrument(telemetry::db_span("close field edge"))
    .await
    .map_err(|e| db_error(e, "close field edge"))?;
    Ok(())
}

/// Person or component node for a field: 404 unknown, 409 retired, unless it is already among
/// the live edges `live` (a retired node already in place stays: the form sends it back with the others).
async fn field_node(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    key: &str,
    live: &[(i64, i64)],
) -> Result<i64, AppError> {
    let unknown = if kind == "person" {
        "unknown_person"
    } else {
        "unknown_component"
    };
    match attr_node(tx, kind, key).await? {
        None => Err(err(
            StatusCode::NOT_FOUND,
            json!({"error": unknown, "key": key}),
        )),
        Some((id, true)) if live.iter().any(|(_, dst)| *dst == id) => Ok(id),
        Some((_, true)) => Err(err(
            StatusCode::CONFLICT,
            json!({"error": "retired", "key": key}),
        )),
        Some((id, false)) => Ok(id),
    }
}

/// Apply the fields to a ticket inside a write transaction (the actor is already set). Fields are checked by [`Fields::check`].
pub async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    seq: i64,
    f: &Fields,
) -> Result<(), AppError> {
    if f.is_empty() {
        return Ok(());
    }
    // Ticket row lock: field updates of one ticket (fields, PATCH, concurrent requests) run in turn,
    // and the difference with live edges is computed from the committed set, not a stale one.
    let node: Option<(i64,)> = sqlx::query_as(
        "select gt.node_id from graph.ticket gt join tickets t on t.seq = gt.ticket_seq \
         where gt.account = (select public.instance_account()) and gt.ticket_seq = $1 for update of t",
    )
    .bind(seq)
    .fetch_optional(&mut **tx)
    .instrument(telemetry::db_span("lock ticket node"))
    .await
    .context("select ticket node")?;
    let (ticket,) = node.context("ticket without a graph node")?;
    if let Some(assignee) = &f.assignee {
        let live = live_edges(tx, ticket, "assigned_to").await?;
        match assignee {
            None => {
                for (id, _) in live {
                    close_edge(tx, id).await?;
                }
            }
            Some(handle) => {
                let person = field_node(tx, "person", handle, &live).await?;
                // Same assignee: nothing changes. A different one: the trigger closes the previous one with superseded.
                if !live.iter().any(|(_, dst)| *dst == person) {
                    insert_edge(tx, ticket, "assigned_to", person, "person").await?;
                }
            }
        }
    }
    if let Some(slugs) = &f.components {
        let live = live_edges(tx, ticket, "in_component").await?;
        let mut want = Vec::with_capacity(slugs.len());
        for s in slugs {
            want.push(field_node(tx, "component", s, &live).await?);
        }
        for (id, dst) in &live {
            if !want.contains(dst) {
                close_edge(tx, *id).await?;
            }
        }
        for c in want {
            if !live.iter().any(|(_, dst)| *dst == c) {
                insert_edge(tx, ticket, "in_component", c, "component").await?;
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub struct FieldsOut {
    pub code: String,
    #[serde(flatten)]
    pub fields: TicketFields,
}

pub async fn set_fields(
    State(db): State<PgPool>,
    actor: Actor,
    Path(code): Path<String>,
    Json(mut f): Json<Fields>,
) -> Result<Json<FieldsOut>, AppError> {
    f.check()?;
    if f.is_empty() {
        return Err(bad("fields: assignee or components"));
    }
    let mut tx = tx::begin(&db, &actor).await?;
    // Like PATCH: a field update moves updated_at (sort by updated) and immediately takes the ticket row lock.
    let seq: Option<(i64,)> =
        sqlx::query_as("update tickets set updated_at = now() where code = $1 returning seq")
            .bind(&code)
            .fetch_optional(&mut *tx)
            .instrument(telemetry::db_span("touch ticket"))
            .await
            .context("touch ticket")?;
    let Some((seq,)) = seq else {
        return Err(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"));
    };
    apply(&mut tx, seq, &f).await?;
    let fields = ticket_fields(&mut *tx, seq).await?;
    tx.commit().await.context("commit ticket fields")?;
    Ok(Json(FieldsOut { code, fields }))
}

pub async fn list_people(State(db): State<PgPool>) -> Result<Json<Vec<Person>>, AppError> {
    let rows = sqlx::query_as(
        "select gp.handle, gp.name, gp.email, gp.aliases, gp.is_agent, n.retired_at is not null as retired, \
                count(t.seq) as tickets, count(t.seq) filter (where t.status in ('open', 'doing')) as active \
         from graph.person gp \
         join graph.node n on n.id = gp.node_id \
         left join graph.edge e on e.dst = gp.node_id and e.rel = 'assigned_to' \
          and e.state = 'confirmed' and e.valid_to is null \
         left join graph.ticket gt on gt.node_id = e.src \
         left join tickets t on t.seq = gt.ticket_seq \
         where gp.account = (select public.instance_account()) \
         group by gp.node_id, n.retired_at order by gp.handle",
    )
    .fetch_all(&db)
    .instrument(telemetry::db_span("select people"))
    .await
    .context("select people")?;
    Ok(Json(rows))
}

pub async fn list_components(State(db): State<PgPool>) -> Result<Json<Vec<Component>>, AppError> {
    let rows = sqlx::query_as(
        "select gc.slug, gc.title, gc.description, pr.key as project, n.retired_at is not null as retired, \
                count(t.seq) as tickets, count(t.seq) filter (where t.status in ('open', 'doing')) as active \
         from graph.component gc \
         join graph.node n on n.id = gc.node_id \
         left join lateral (select p.key from graph.edge ce join graph.node p on p.id = ce.dst \
          where ce.src = gc.node_id and ce.rel = 'component_of' and ce.state = 'confirmed' \
           and ce.valid_to is null limit 1) pr on true \
         left join graph.edge e on e.dst = gc.node_id and e.rel = 'in_component' \
          and e.state = 'confirmed' and e.valid_to is null \
         left join graph.ticket gt on gt.node_id = e.src \
         left join tickets t on t.seq = gt.ticket_seq \
         where gc.account = (select public.instance_account()) \
         group by gc.node_id, n.retired_at, pr.key order by pr.key nulls last, gc.slug",
    )
    .fetch_all(&db)
    .instrument(telemetry::db_span("select components"))
    .await
    .context("select components")?;
    Ok(Json(rows))
}

/// Only a human adds people and components (spec "Actor and trust"): agents and services get 403.
fn require_human(actor: &Actor) -> Result<(), AppError> {
    if actor.is_human() {
        Ok(())
    } else {
        Err(err(
            StatusCode::FORBIDDEN,
            json!({"error": "requires_human"}),
        ))
    }
}

/// A new node: 409 `exists` if the key is already taken.
async fn new_node(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    key: &str,
) -> Result<i64, AppError> {
    let id: Option<(i64,)> = sqlx::query_as(
        "insert into graph.node (kind, key) values ($1, $2) on conflict (account, kind, key) do nothing returning id",
    )
    .bind(kind)
    .bind(key)
    .fetch_optional(&mut **tx)
    .instrument(telemetry::db_span("insert node"))
    .await
    .map_err(|e| db_error(e, "insert node"))?;
    id.map(|r| r.0)
        .ok_or_else(|| err(StatusCode::CONFLICT, json!({"error": "exists", "key": key})))
}

pub async fn create_person(
    State(db): State<PgPool>,
    actor: Actor,
    Json(mut p): Json<NewPerson>,
) -> Result<(StatusCode, Json<NewPerson>), AppError> {
    p.check().map_err(bad)?;
    require_human(&actor)?;
    let mut tx = tx::begin(&db, &actor).await?;
    let id = new_node(&mut tx, "person", &p.handle).await?;
    sqlx::query(
        "insert into graph.person (node_id, handle, name, email, aliases, is_agent) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(&p.handle)
    .bind(&p.name)
    .bind(&p.email)
    .bind(&p.aliases)
    .bind(p.is_agent)
    .execute(&mut *tx)
    .instrument(telemetry::db_span("insert person"))
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.constraint() == Some("person_email_unique") => {
            err(StatusCode::CONFLICT, json!({"error": "email_taken"}))
        }
        _ => db_error(e, "insert person"),
    })?;
    tx.commit().await.context("commit person")?;
    Ok((StatusCode::CREATED, Json(p)))
}

pub async fn create_component(
    State(db): State<PgPool>,
    actor: Actor,
    Json(mut c): Json<NewComponent>,
) -> Result<(StatusCode, Json<NewComponent>), AppError> {
    c.check().map_err(bad)?;
    if !c.tags.is_empty() {
        return Err(bad("tags: only in services/hub/seed/components.toml"));
    }
    require_human(&actor)?;
    let mut tx = tx::begin(&db, &actor).await?;
    let id = new_node(&mut tx, "component", &c.slug).await?;
    sqlx::query(
        "insert into graph.component (node_id, slug, title, description) values ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(&c.slug)
    .bind(&c.title)
    .bind(&c.description)
    .execute(&mut *tx)
    .instrument(telemetry::db_span("insert component"))
    .await
    .map_err(|e| db_error(e, "insert component"))?;
    if let Some(project) = &c.project {
        sqlx::query(
            "insert into graph.node (kind, key) values ('project', $1) on conflict (account, kind, key) do nothing",
        )
        .bind(project)
        .execute(&mut *tx)
        .instrument(telemetry::db_span("insert project node"))
        .await
        .context("insert project node")?;
        sqlx::query(
            "insert into graph.edge (src, src_kind, rel, dst, dst_kind) \
             select $1, 'component', 'component_of', p.id, 'project' from graph.node p \
             where p.account = (select public.instance_account()) and p.kind = 'project' and p.key = $2",
        )
        .bind(id)
        .bind(project)
        .execute(&mut *tx)
        .instrument(telemetry::db_span("insert component_of"))
        .await
        .map_err(|e| db_error(e, "insert component_of"))?;
    }
    tx.commit().await.context("commit component")?;
    Ok((StatusCode::CREATED, Json(c)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeopleSeed {
    #[serde(default)]
    person: Vec<NewPerson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentsSeed {
    #[serde(default)]
    component: Vec<NewComponent>,
}

/// Parsing and checking the people seed: the same rules as POST, handles and addresses without duplicates.
pub fn parse_people(text: &str) -> anyhow::Result<Vec<NewPerson>> {
    let mut seed: PeopleSeed = toml::from_str(text).context("people.toml")?;
    let mut seen = std::collections::BTreeSet::new();
    for p in &mut seed.person {
        p.check()
            .map_err(|e| anyhow::anyhow!("people.toml {}: {e}", p.handle))?;
        anyhow::ensure!(
            seen.insert(p.handle.clone()),
            "people.toml: handle {} twice",
            p.handle
        );
        if let Some(e) = &p.email {
            anyhow::ensure!(
                seen.insert(format!("@{}", e.to_lowercase())),
                "people.toml: email of {} twice",
                p.handle
            );
        }
    }
    Ok(seed.person)
}

/// Parsing and checking the component seed: the same rules as POST, project required, slug without duplicates.
pub fn parse_components(text: &str) -> anyhow::Result<Vec<NewComponent>> {
    let mut seed: ComponentsSeed = toml::from_str(text).context("components.toml")?;
    let mut seen = std::collections::BTreeSet::new();
    for c in &mut seed.component {
        c.check()
            .map_err(|e| anyhow::anyhow!("components.toml {}: {e}", c.slug))?;
        anyhow::ensure!(
            c.project.is_some(),
            "components.toml {}: project is required",
            c.slug
        );
        anyhow::ensure!(
            seen.insert(c.slug.clone()),
            "components.toml: slug {} twice",
            c.slug
        );
    }
    Ok(seed.component)
}

/// People and component seeds at startup: one transaction, the import functions set the actor and the lock themselves.
/// A broken seed fails the start (like a broken Roadmap snapshot): the rollout stalls, the old pod works. No file: an empty list,
/// the import changes nothing.
pub async fn seed(db: &PgPool, seeds: &Seeds) -> anyhow::Result<()> {
    if seeds.people.is_none() && seeds.components.is_none() {
        return Ok(());
    }
    let people = serde_json::to_value(parse_people(seeds.people.as_deref().unwrap_or(""))?)?;
    let components =
        serde_json::to_value(parse_components(seeds.components.as_deref().unwrap_or(""))?)?;
    let mut tx = db.begin().await.context("begin seed")?;
    let (p,): (serde_json::Value,) = sqlx::query_as("select graph.import_people($1::jsonb)")
        .bind(&people)
        .fetch_one(&mut *tx)
        .instrument(telemetry::db_span("import people"))
        .await
        .context("import people")?;
    let (c,): (serde_json::Value,) = sqlx::query_as("select graph.import_components($1::jsonb)")
        .bind(&components)
        .fetch_one(&mut *tx)
        .instrument(telemetry::db_span("import components"))
        .await
        .context("import components")?;
    tx.commit().await.context("commit seed")?;
    tracing::info!(people = %p, components = %c, "seed people and components");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_follow_the_database_check() {
        for ok in ["alice", "bob", "a", "a1", "alice-bot", &"a".repeat(40)] {
            assert!(valid_handle(ok), "{ok}");
        }
        for bad in [
            "",
            "none",
            "Alice",
            "1alice",
            "-alice",
            "a_b",
            "ελ",
            "alice bob",
            &"a".repeat(41),
        ] {
            assert!(!valid_handle(bad), "{bad}");
        }
    }

    #[test]
    fn slugs_follow_the_database_check() {
        for ok in ["cockpit", "nu-plugin-demo", "1x", "a", &"a".repeat(40)] {
            assert!(valid_slug(ok), "{ok}");
        }
        for bad in ["", "Cockpit", "-x", "nu_plugin", "a b", &"a".repeat(41)] {
            assert!(!valid_slug(bad), "{bad}");
        }
    }

    #[test]
    fn emails() {
        assert!(valid_email("a@b.test"));
        for bad in ["", "a", "a@", "@b", "a@b@c", "a b@c"] {
            assert!(!valid_email(bad), "{bad}");
        }
    }

    #[test]
    fn fields_are_checked_and_normalised() {
        let mut f: Fields =
            serde_json::from_str(r#"{"assignee":" Alice ","components":["hub","Cockpit","hub"]}"#)
                .unwrap();
        f.check().unwrap();
        assert_eq!(f.assignee, Some(Some("alice".into())));
        assert_eq!(f.components, Some(vec!["cockpit".into(), "hub".into()]));
        let mut f: Fields = serde_json::from_str(r#"{"assignee":null}"#).unwrap();
        f.check().unwrap();
        assert_eq!(f.assignee, Some(None), "null takes the assignee off");
        let mut f: Fields = serde_json::from_str(r#"{"assignee":""}"#).unwrap();
        f.check().unwrap();
        assert_eq!(f.assignee, Some(None), "empty is null");
        let f: Fields = serde_json::from_str("{}").unwrap();
        assert!(f.is_empty());
        for body in [
            r#"{"assignee":"1alice"}"#,
            r#"{"assignee":"a_b"}"#,
            r#"{"assignee":"none"}"#,
            r#"{"components":["bad_slug"]}"#,
            r#"{"components":[""]}"#,
        ] {
            let mut f: Fields = serde_json::from_str(body).unwrap();
            assert!(f.check().is_err(), "{body}");
        }
        let many: Vec<String> = (0..21).map(|i| format!("c{i}")).collect();
        let mut f = Fields {
            components: Some(many),
            ..Fields::default()
        };
        assert!(f.check().is_err(), "more than 20 components");
    }

    #[test]
    fn new_person_and_component_are_checked() {
        let mut p: NewPerson =
            serde_json::from_str(r#"{"handle":" carol ","name":" Κάρολ ","email":" "}"#).unwrap();
        p.check().unwrap();
        assert_eq!(
            (p.handle.as_str(), p.name.as_str(), p.email),
            ("carol", "Κάρολ", None)
        );
        let mut p: NewPerson =
            serde_json::from_str(r#"{"handle":"carol","name":"A","aliases":[" Carol B. "]}"#)
                .unwrap();
        p.check().unwrap();
        assert_eq!(p.aliases, ["Carol B."], "aliases are stored trimmed");
        for body in [
            r#"{"handle":"none","name":"A"}"#,
            r#"{"handle":"Ann","name":"A"}"#,
            r#"{"handle":"carol","name":"A","aliases":[" "]}"#,
            r#"{"handle":"carol","name":" "}"#,
            r#"{"handle":"carol","name":"A","email":"nope"}"#,
        ] {
            let mut p: NewPerson = serde_json::from_str(body).unwrap();
            assert!(p.check().is_err(), "{body}");
        }
        assert!(
            serde_json::from_str::<NewPerson>(r#"{"handle":"a","name":"A","role":"x"}"#).is_err()
        );
        let mut c: NewComponent =
            serde_json::from_str(r#"{"slug":"x-1","title":"X","project":"demo"}"#).unwrap();
        c.check().unwrap();
        for body in [
            r#"{"slug":"x_1","title":"X"}"#,
            r#"{"slug":"x","title":""}"#,
            r#"{"slug":"x","title":"X","project":"Bad"}"#,
        ] {
            let mut c: NewComponent = serde_json::from_str(body).unwrap();
            assert!(c.check().is_err(), "{body}");
        }
    }

    // The seeds in the tree (the example, and an install seed when the tree has one): a broken file is caught by this test,
    // not by a pod start.
    #[test]
    fn seeds_parse_and_pass_the_checks() {
        for dir in crate::seed::tree_dirs() {
            let seeds = Seeds::read(&dir).unwrap();
            assert!(
                !parse_people(seeds.people.as_deref().unwrap())
                    .unwrap()
                    .is_empty(),
                "{}",
                dir.display()
            );
            let components = parse_components(seeds.components.as_deref().unwrap()).unwrap();
            assert!(
                components.iter().all(|c| !c.tags.is_empty()),
                "{}: every component has source tags",
                dir.display()
            );
        }
        let example = Seeds::read(&crate::seed::example_dir()).unwrap();
        let handles: Vec<String> = parse_people(example.people.as_deref().unwrap())
            .unwrap()
            .into_iter()
            .map(|p| p.handle)
            .collect();
        assert_eq!(handles, ["alice", "bob"]);
        assert_eq!(
            parse_components(example.components.as_deref().unwrap())
                .unwrap()
                .len(),
            2
        );
        assert!(parse_people("[[person]]\nhandle = \"alice\"\nname = \"a\"\n[[person]]\nhandle = \"alice\"\nname = \"b\"\n").is_err());
        assert!(parse_people("[[person]]\nhandle = \"Alice\"\nname = \"a\"\n").is_err());
        assert!(
            parse_people("[[person]]\nhandle = \"a\"\nname = \"a\"\nemail = \"X@y.z\"\n[[person]]\nhandle = \"b\"\nname = \"b\"\nemail = \"x@y.z\"\n").is_err(),
            "email case does not make it new"
        );
        assert!(
            parse_components("[[component]]\nslug = \"x\"\ntitle = \"X\"\n").is_err(),
            "project required"
        );
        assert!(
            parse_components(
                "[[component]]\nslug = \"x\"\ntitle = \"X\"\nproject = \"a\"\nowner = \"alice\"\n"
            )
            .is_err()
        );
    }
}
