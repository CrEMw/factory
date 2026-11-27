//! Graph edges (the edge layer under the "Graph" view of graph_view.rs; spec "Nodes and edges", "API and MCP").
//!
//! POST /api/edges                 `{src:{kind,key}, rel, dst:{kind,key}, state?, evidence?}` -> 201 edge
//! POST /api/edges/{id}/confirm    proposed -> confirmed, humans only (the database checks the actor kind)
//! POST /api/edges/{id}/close      `{reason: removed|rejected}`; an edge confirmed by a human is closed only by a human
//! GET  /api/tickets/{code}/links  live edges of a ticket with the nodes at their ends, the number of open blockers and the documents
//!                                 that describe or close the ticket (ticket_documents)
//!
//! Ticket node key in the API: the ticket code (in the database a ticket node holds the seq). Other kinds: the node key as is
//! (project: slug, document: `repo:path`). The database holds the invariants (0010_graph.sql); here its
//! errors are only translated into responses: 409 `{"error":"cycle","path":[codes]}`, 409 `single_violation` and `exists`, 422
//! `unknown_shape`, `self_link`, `bad_evidence`, 403 `confirm_requires_human` and `close_requires_human`,
//! 404 `unknown_node`.

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction, postgres::PgDatabaseError};
use tracing::Instrument;

use crate::{
    AppError, bad, telemetry,
    tx::{self, Actor},
};

#[derive(Deserialize, Debug)]
pub struct NodeRef {
    pub kind: String,
    pub key: String,
}

#[derive(Deserialize, Debug)]
pub struct NewEdge {
    pub src: NodeRef,
    pub rel: String,
    pub dst: NodeRef,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub evidence: Option<Value>,
}

#[derive(Deserialize)]
pub struct CloseEdge {
    pub reason: String,
}

/// Edge in a response: the ends are labelled with a ticket code or a node key.
#[derive(Serialize, sqlx::FromRow)]
pub struct Edge {
    pub id: i64,
    pub rel: String,
    pub state: String,
    pub src_kind: String,
    pub src: String,
    pub dst_kind: String,
    pub dst: String,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub confirmed_by: Option<String>,
    pub confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Kind of the confirmer: human, agent, seed, service:<name>.
    pub confirmed_kind: Option<String>,
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
    pub close_reason: Option<String>,
}

const EDGE_SELECT: &str = "select e.id, e.rel, e.state, e.src_kind, graph.node_label(e.src) as src, e.dst_kind, \
    graph.node_label(e.dst) as dst, e.created_by, e.created_at, e.confirmed_by, e.confirmed_at, e.confirmed_kind, e.valid_to, \
    e.close_reason from graph.edge e where e.account = (select public.instance_account()) and e.id = $1";

/// A ticket's edge from the ticket's side: the direction and the node at the other end.
#[derive(Serialize, sqlx::FromRow)]
pub struct Link {
    pub id: i64,
    pub rel: String,
    pub state: String,
    /// out: the ticket is src (it blocks, it is a part); in: the ticket is dst.
    pub dir: String,
    pub kind: String,
    pub key: String,
    pub summary: Option<String>,
    pub status: Option<String>,
    pub project: Option<String>,
    pub created_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A document that describes the ticket (describes) or closes it (closes): a link from git, not a graph edge.
#[derive(Serialize, sqlx::FromRow)]
pub struct DocumentLink {
    pub rel: String,
    pub repo: String,
    pub path: String,
    pub title: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct Links {
    pub code: String,
    /// Confirmed blockers (in blocks) that are still open or doing.
    pub open_blockers: i64,
    /// Confirmed subtasks (in part_of) that are still open or doing.
    pub open_children: i64,
    pub links: Vec<Link>,
    pub documents: Vec<DocumentLink>,
}

/// Node kind and rel: lowercase Latin letters and underscores, as in the database.
fn valid_word(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=40).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || *c == b'_')
}

/// Node key: up to 300 bytes, for a document (`repo:path`, the path in documents is unlimited) up to 2000.
fn valid_key(kind: &str, s: &str) -> bool {
    let max = if kind == "document" { 2000 } else { 300 };
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn err(status: StatusCode, body: Value) -> AppError {
    AppError::Json(status, body)
}

/// Body check before the database. A ticket key is lowercased (codes are lowercase).
pub fn check(new: &mut NewEdge) -> Result<(), AppError> {
    for n in [&mut new.src, &mut new.dst] {
        n.kind = n.kind.trim().to_owned();
        n.key = n.key.trim().to_owned();
        if !valid_word(&n.kind) || !valid_key(&n.kind, &n.key) {
            return Err(bad(
                "node: {kind: lowercase word, key: 1 to 300 bytes, document 1 to 2000}",
            ));
        }
        if n.kind == "ticket" {
            n.key = n.key.to_lowercase();
        }
    }
    new.rel = new.rel.trim().to_owned();
    if !valid_word(&new.rel) {
        return Err(bad("rel: lowercase latin letters and _, 2 to 40"));
    }
    if !matches!(new.state.as_deref(), None | Some("proposed" | "confirmed")) {
        return Err(bad("state: proposed or confirmed"));
    }
    if new
        .evidence
        .as_ref()
        .is_some_and(|e| !e.is_object() && !e.is_null())
    {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "bad_evidence"}),
        ));
    }
    if new.src.kind == new.dst.kind && new.src.key == new.dst.key {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "self_link"}),
        ));
    }
    Ok(())
}

/// Graph trigger and index errors into API responses; everything else: 500 with context.
pub fn db_error(e: sqlx::Error, what: &'static str) -> AppError {
    if let sqlx::Error::Database(d) = &e {
        let code = d.code();
        let simple = |status: StatusCode, error: &str| Some(err(status, json!({ "error": error })));
        let mapped = match code.as_deref() {
            Some("GRCYC") => {
                let path: Vec<String> = d
                    .try_downcast_ref::<PgDatabaseError>()
                    .and_then(|p| p.detail())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                Some(err(
                    StatusCode::CONFLICT,
                    json!({"error": "cycle", "path": path}),
                ))
            }
            Some("GRHUM") => simple(StatusCode::FORBIDDEN, "confirm_requires_human"),
            Some("GRHCL") => simple(StatusCode::FORBIDDEN, "close_requires_human"),
            Some("GRRET") => simple(StatusCode::CONFLICT, "retired"),
            Some("GRSHP") => simple(StatusCode::UNPROCESSABLE_ENTITY, "unknown_shape"),
            Some("GRSLF") => simple(StatusCode::UNPROCESSABLE_ENTITY, "self_link"),
            Some("GREVD") => simple(StatusCode::UNPROCESSABLE_ENTITY, "bad_evidence"),
            Some("GRFRB") => simple(StatusCode::CONFLICT, "forbidden"),
            Some("23505") => match d.constraint() {
                Some("edge_single") => simple(StatusCode::CONFLICT, "single_violation"),
                Some("edge_live_unique") => simple(StatusCode::CONFLICT, "exists"),
                _ => None,
            },
            _ => None,
        };
        if let Some(m) = mapped {
            return m;
        }
    }
    anyhow::Error::new(e).context(what).into()
}

async fn node_id(tx: &mut Transaction<'_, Postgres>, n: &NodeRef) -> Result<Option<i64>, AppError> {
    let id: Option<(i64,)> = sqlx::query_as(
        "select n.id from graph.node n where n.account = (select public.instance_account()) and n.kind = $1 and n.key = \
         case when $1 = 'ticket' then (select t.seq::text from tickets t where t.code = $2) else $2 end",
    )
    .bind(&n.kind)
    .bind(&n.key)
    .fetch_optional(&mut **tx)
    .instrument(telemetry::db_span("select node"))
    .await
    .context("select node")?;
    Ok(id.map(|r| r.0))
}

async fn edge(tx: &mut Transaction<'_, Postgres>, id: i64) -> Result<Option<Edge>, AppError> {
    Ok(sqlx::query_as(EDGE_SELECT)
        .bind(id)
        .fetch_optional(&mut **tx)
        .instrument(telemetry::db_span("select edge"))
        .await
        .context("select edge")?)
}

pub async fn create_edge(
    State(db): State<PgPool>,
    actor: Actor,
    Json(mut new): Json<NewEdge>,
) -> Result<(StatusCode, Json<Edge>), AppError> {
    check(&mut new)?;
    let mut tx = tx::begin(&db, &actor).await?;
    let (shape,): (bool,) = sqlx::query_as(
        "select exists (select 1 from graph.rel_shape where account = (select public.instance_account()) and rel = $1 \
         and src_kind = $2 and dst_kind = $3)",
    )
    .bind(&new.rel)
    .bind(&new.src.kind)
    .bind(&new.dst.kind)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("select rel shape"))
    .await
    .context("select rel shape")?;
    if !shape {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "unknown_shape", "src_kind": new.src.kind, "rel": new.rel, "dst_kind": new.dst.kind}),
        ));
    }
    let mut ids = [0i64; 2];
    for (slot, n) in ids.iter_mut().zip([&new.src, &new.dst]) {
        *slot = node_id(&mut tx, n).await?.ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                json!({"error": "unknown_node", "kind": n.kind, "key": n.key}),
            )
        })?;
    }
    let evidence = new
        .evidence
        .as_ref()
        .filter(|e| !e.is_null())
        .map(Value::to_string);
    let (id,): (i64,) = sqlx::query_as(
        "insert into graph.edge (src, src_kind, rel, dst, dst_kind, state, evidence) \
         values ($1, $2, $3, $4, $5, $6, $7::jsonb) returning id",
    )
    .bind(ids[0])
    .bind(&new.src.kind)
    .bind(&new.rel)
    .bind(ids[1])
    .bind(&new.dst.kind)
    .bind(new.state.as_deref().unwrap_or("confirmed"))
    .bind(evidence)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("insert edge"))
    .await
    .map_err(|e| db_error(e, "insert edge"))?;
    let out = edge(&mut tx, id).await?.context("edge after insert")?;
    tx.commit().await.context("commit edge")?;
    Ok((StatusCode::CREATED, Json(out)))
}

pub async fn confirm_edge(
    State(db): State<PgPool>,
    actor: Actor,
    Path(id): Path<i64>,
) -> Result<Json<Edge>, AppError> {
    // The database checks the same; here the response comes without a transaction for the common case (MCP).
    if !actor.is_human() {
        return Err(err(
            StatusCode::FORBIDDEN,
            json!({"error": "confirm_requires_human"}),
        ));
    }
    let mut tx = tx::begin(&db, &actor).await?;
    let row: Option<(i64,)> = sqlx::query_as(
        "update graph.edge set state = 'confirmed', confirmed_at = now(), confirmed_by = $2 \
         where account = (select public.instance_account()) and id = $1 and state = 'proposed' returning id",
    )
    .bind(id)
    .bind(&actor.id)
    .fetch_optional(&mut *tx)
    .instrument(telemetry::db_span("confirm edge"))
    .await
    .map_err(|e| db_error(e, "confirm edge"))?;
    let out = edge(&mut tx, id)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, json!({"error": "unknown_edge"})))?;
    if row.is_none() && out.valid_to.is_some() {
        return Err(err(StatusCode::CONFLICT, json!({"error": "closed"})));
    }
    tx.commit().await.context("commit confirm")?;
    Ok(Json(out))
}

pub async fn close_edge(
    State(db): State<PgPool>,
    actor: Actor,
    Path(id): Path<i64>,
    Json(close): Json<CloseEdge>,
) -> Result<Json<Edge>, AppError> {
    let reason = close.reason.trim();
    if !matches!(reason, "removed" | "rejected") {
        return Err(bad("reason: removed or rejected"));
    }
    let mut tx = tx::begin(&db, &actor).await?;
    let row: Option<(i64,)> = sqlx::query_as(
        "update graph.edge set valid_to = now(), close_reason = $2, closed_by = $3 \
         where account = (select public.instance_account()) and id = $1 and valid_to is null returning id",
    )
    .bind(id)
    .bind(reason)
    .bind(&actor.id)
    .fetch_optional(&mut *tx)
    .instrument(telemetry::db_span("close edge"))
    .await
    .map_err(|e| db_error(e, "close edge"))?;
    let out = edge(&mut tx, id)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, json!({"error": "unknown_edge"})))?;
    if row.is_none() {
        return Err(err(StatusCode::CONFLICT, json!({"error": "closed"})));
    }
    tx.commit().await.context("commit close")?;
    Ok(Json(out))
}

pub async fn ticket_links(
    State(db): State<PgPool>,
    Path(code): Path<String>,
) -> Result<Json<Links>, AppError> {
    let node: Option<(i64, i64)> = sqlx::query_as(
        "select gt.node_id, t.seq from tickets t join graph.ticket gt on gt.ticket_seq = t.seq where t.code = $1",
    )
    .bind(&code)
    .fetch_optional(&db)
    .instrument(telemetry::db_span("select ticket node"))
    .await
    .context("select ticket node")?;
    let Some((node, seq)) = node else {
        return Err(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"));
    };
    let documents = sqlx::query_as(
        "select l.rel, d.repo, d.path, d.title, d.role from ticket_documents l \
         join documents d on d.repo = l.repo and d.path = l.path \
         where l.ticket_seq = $1 and l.valid_to is null order by l.rel, d.repo, d.path",
    )
    .bind(seq)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select ticket documents"))
    .await
    .context("select ticket documents")?;
    let links: Vec<Link> = sqlx::query_as(
        "select e.id, e.rel, e.state, case when e.src = $1 then 'out' else 'in' end as dir, \
                o.kind, graph.node_label(o.id) as key, t.summary, t.status, t.project, e.created_by, e.created_at \
         from graph.edge e \
         join graph.node o on o.id = case when e.src = $1 then e.dst else e.src end \
         left join graph.ticket gt on gt.node_id = o.id \
         left join tickets t on t.seq = gt.ticket_seq \
         where (e.src = $1 or e.dst = $1) and e.valid_to is null \
         order by e.rel, e.id",
    )
    .bind(node)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select ticket links"))
    .await
    .context("select ticket links")?;
    let open = |rel: &str| {
        links
            .iter()
            .filter(|l| {
                l.rel == rel
                    && l.dir == "in"
                    && l.state == "confirmed"
                    && matches!(l.status.as_deref(), Some("open" | "doing"))
            })
            .count() as i64
    };
    Ok(Json(Links {
        code,
        open_blockers: open("blocks"),
        open_children: open("part_of"),
        links,
        documents,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(body: &str) -> Result<NewEdge, AppError> {
        let mut e: NewEdge = serde_json::from_str(body).unwrap();
        check(&mut e).map(|_| e)
    }

    fn status(r: Result<NewEdge, AppError>) -> StatusCode {
        match r {
            Err(AppError::Client(s, _)) | Err(AppError::Json(s, _)) => s,
            Err(AppError::Internal(e)) => panic!("{e}"),
            Ok(_) => StatusCode::OK,
        }
    }

    #[test]
    fn bodies_are_checked_before_the_database() {
        let ok = edge(r#"{"src":{"kind":"ticket","key":" AB "},"rel":"blocks","dst":{"kind":"ticket","key":"c"}}"#)
            .unwrap();
        assert_eq!(ok.src.key, "ab", "ticket codes are lowercased");
        for (body, want) in [
            (
                r#"{"src":{"kind":"ticket","key":"a"},"rel":"blocks","dst":{"kind":"ticket","key":"A"}}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                r#"{"src":{"kind":"ticket","key":"a"},"rel":"Blocks","dst":{"kind":"ticket","key":"b"}}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"src":{"kind":"ticket","key":""},"rel":"blocks","dst":{"kind":"ticket","key":"b"}}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"src":{"kind":"ticket","key":"a"},"rel":"blocks","dst":{"kind":"ticket","key":"b"},"state":"done"}"#,
                StatusCode::BAD_REQUEST,
            ),
            (
                r#"{"src":{"kind":"ticket","key":"a"},"rel":"blocks","dst":{"kind":"ticket","key":"b"},"evidence":[1]}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            assert_eq!(status(edge(body)), want, "{body}");
        }
    }

    #[test]
    fn document_keys_are_longer() {
        let long = format!("acme/widgets:docs/plans/{}.md", "a".repeat(400));
        assert!(valid_key("document", &long));
        assert!(!valid_key("ticket", &long));
        assert!(!valid_key("document", &"a".repeat(2001)));
    }

    #[test]
    fn words() {
        for ok in ["blocks", "part_of", "ticket", "in_component"] {
            assert!(valid_word(ok), "{ok}");
        }
        for bad in ["", "a", "Blocks", "part-of", "1x", &"a".repeat(41)] {
            assert!(!valid_word(bad), "{bad}");
        }
    }
}
