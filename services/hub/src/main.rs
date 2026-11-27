mod adopt;
mod callers;
// Critical chain computation: parts of the module (Plan::buffer, fixture parsing) are used only by tests and the next stage (pages).
#[allow(dead_code)]
mod ccpm;
mod config;
mod documents;
mod graph;
mod graph_view;
mod logins;
mod mailbox;
mod migrations;
mod people;
#[cfg(test)]
mod pg_tests;
mod plan;
mod plan_seed;
mod query;
mod seed;
mod telemetry;
mod tx;

use std::net::SocketAddr;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::signal::unix::{SignalKind, signal};
use tracing::Instrument;

/// List row: no body, so a list of a thousand tickets stays light.
#[derive(Serialize, sqlx::FromRow)]
struct TicketRow {
    seq: i64,
    project: String,
    code: String,
    summary: String,
    status: String,
    priority: Option<String>,
    tags: Vec<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Ticket {
    seq: i64,
    project: String,
    code: String,
    summary: String,
    status: String,
    priority: Option<String>,
    tags: Vec<String>,
    body: String,
    transcript_ref: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

const TICKET_COLUMNS: &str = "seq, project, code, summary, status, priority, tags, body, transcript_ref, created_at, updated_at";

#[derive(Serialize, sqlx::FromRow)]
struct DocumentRef {
    repo: String,
    path: String,
    role: String,
    title: String,
    rel: String,
}

#[derive(Serialize)]
struct TicketDetail {
    #[serde(flatten)]
    ticket: Ticket,
    documents: Vec<DocumentRef>,
    /// Assignee and components from graph edges.
    #[serde(flatten)]
    fields: people::TicketFields,
}

/// Create and update response: the ticket with its assignee and components. MCP tells the new hub from the old one
/// by the components field: the old one silently skips assignee and components.
#[derive(Serialize)]
struct TicketOut {
    #[serde(flatten)]
    ticket: Ticket,
    #[serde(flatten)]
    fields: people::TicketFields,
}

#[derive(Deserialize)]
struct NewTicket {
    // Option: a missing field or null gives 400 with a clear message, not 422 from JSON parsing.
    #[serde(default)]
    project: Option<String>,
    /// Without a code the hub assigns one from the ticket number (like the old Roadmap: 1, 2, ..., z, 10, ...).
    #[serde(default)]
    code: Option<String>,
    summary: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    /// Assignee handle; missing or null: no assignee.
    #[serde(default)]
    assignee: Option<String>,
    /// Component slugs.
    #[serde(default)]
    components: Option<Vec<String>>,
}

/// Ticket update: only the given fields change. priority: null clears the priority.
#[derive(Deserialize, Default)]
struct TicketPatch {
    project: Option<String>,
    summary: Option<String>,
    status: Option<String>,
    body: Option<String>,
    #[serde(default, deserialize_with = "some_or_null")]
    priority: Option<Option<String>>,
    tags: Option<Vec<String>>,
    /// Assignee: no field (leave as is), null (clear) or a handle.
    #[serde(default, deserialize_with = "some_or_null")]
    assignee: Option<Option<String>>,
    /// The full set of components: extra ones are removed, missing ones are added.
    components: Option<Vec<String>>,
}

// Distinguishes "no field" (None) from "priority: null" (Some(None)).
fn some_or_null<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug)]
enum AppError {
    Client(StatusCode, &'static str),
    /// Error with a JSON body (`{"error": ...}`): for graph responses parsed by MCP and the pages.
    Json(StatusCode, serde_json::Value),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self::Internal(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::Client(status, msg) => (status, msg).into_response(),
            Self::Json(status, body) => (status, Json(body)).into_response(),
            Self::Internal(err) => {
                // {:#} on anyhow prints the whole chain of causes on one line.
                tracing::error!(error = format!("{err:#}"), "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

fn bad(msg: &'static str) -> AppError {
    AppError::Client(StatusCode::BAD_REQUEST, msg)
}

#[derive(Deserialize)]
struct TicketFilter {
    project: Option<String>,
    status: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct ProjectCount {
    project: String,
    tickets: i64,
}

/// Project name: lowercase Latin letters, digits and hyphens, up to 40 characters (same as the database check).
fn valid_project(p: &str) -> bool {
    let b = p.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn valid_status(s: &str) -> bool {
    matches!(s, "open" | "doing" | "done" | "cancelled")
}

fn valid_priority(p: &str) -> bool {
    matches!(p, "p0" | "p1" | "p2" | "p3")
}

/// Ticket code from its number: the number in base 32 with an alphabet without i, l, o, u (like the old Roadmap).
const CODE_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

fn code_for(seq: i64) -> String {
    let mut n = seq.max(0) as u64;
    let mut out = Vec::new();
    loop {
        out.push(CODE_ALPHABET[(n % 32) as usize]);
        n /= 32;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

fn valid_tags(tags: &[String]) -> bool {
    tags.len() <= 30 && tags.iter().all(|t| !t.trim().is_empty() && t.len() <= 60)
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|p| !p.is_empty())
}

async fn list_tickets(
    State(db): State<PgPool>,
    Query(filter): Query<TicketFilter>,
) -> Result<Json<Vec<TicketRow>>, AppError> {
    let project = non_empty(filter.project.as_deref());
    if project.is_some_and(|p| !valid_project(p)) {
        return Err(bad("bad project name"));
    }
    let status = non_empty(filter.status.as_deref());
    if status.is_some_and(|s| !valid_status(s)) {
        return Err(bad("status: open, doing, done or cancelled"));
    }
    let rows = sqlx::query_as(
        "select seq, project, code, summary, status, priority, tags, created_at, updated_at from tickets \
         where ($1::text is null or project = $1) and ($2::text is null or status = $2) order by seq desc",
    )
    .bind(project)
    .bind(status)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select tickets"))
    .await
    .context("select tickets")?;
    Ok(Json(rows))
}

async fn get_ticket(
    State(db): State<PgPool>,
    Path(code): Path<String>,
) -> Result<Json<TicketDetail>, AppError> {
    let ticket: Option<Ticket> = sqlx::query_as(&format!(
        "select {TICKET_COLUMNS} from tickets where code = $1"
    ))
    .bind(&code)
    .fetch_optional(&db)
    .instrument(telemetry::db_span("select ticket"))
    .await
    .context("select ticket")?;
    let Some(ticket) = ticket else {
        return Err(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"));
    };
    let documents = sqlx::query_as(
        "select d.repo, d.path, d.role, d.title, l.rel from ticket_documents l \
         join documents d on d.repo = l.repo and d.path = l.path \
         where l.ticket_seq = $1 and l.valid_to is null order by d.path",
    )
    .bind(ticket.seq)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select ticket documents"))
    .await
    .context("select ticket documents")?;
    let fields = people::ticket_fields(&db, ticket.seq).await?;
    Ok(Json(TicketDetail {
        ticket,
        documents,
        fields,
    }))
}

async fn list_projects(State(db): State<PgPool>) -> Result<Json<Vec<ProjectCount>>, AppError> {
    let rows = sqlx::query_as(
        "select project, count(*) as tickets from tickets group by project order by project",
    )
    .fetch_all(&db)
    .instrument(telemetry::db_span("select projects"))
    .await
    .context("select projects")?;
    Ok(Json(rows))
}

async fn create_ticket(
    State(db): State<PgPool>,
    actor: tx::Actor,
    Json(new): Json<NewTicket>,
) -> Result<(StatusCode, Json<TicketOut>), AppError> {
    let project = new.project.as_deref().unwrap_or("").trim();
    let summary = new.summary.trim();
    if project.is_empty() || summary.is_empty() {
        return Err(bad("project and summary are required"));
    }
    if !valid_project(project) {
        return Err(bad(
            "project: lowercase latin letters, digits and dashes, up to 40",
        ));
    }
    let priority = non_empty(new.priority.as_deref());
    if priority.is_some_and(|p| !valid_priority(p)) {
        return Err(bad("priority: p0, p1, p2 or p3"));
    }
    if !valid_tags(&new.tags) {
        return Err(bad("tags: up to 30, each 1 to 60 bytes"));
    }
    let tags: Vec<&str> = new.tags.iter().map(|t| t.trim()).collect();
    let mut fields = people::Fields {
        assignee: new.assignee.clone().map(Some),
        components: new.components.clone(),
    };
    fields.check()?;
    // An explicit code (the field is present but empty) is an error; without the field the hub assigns the code.
    let manual = match new.code.as_deref().map(str::trim) {
        Some("") => return Err(bad("code is empty; omit it to get one by number")),
        other => other,
    };
    // The number is taken up front so that the code derived from it goes into the same insert. If the assigned code is
    // already taken by a code set by hand, the next number is taken. Each attempt runs in its own transaction with an
    // actor: an insert error aborts the transaction.
    for _ in 0..5 {
        let mut tx = tx::begin(&db, &actor).await?;
        let (seq,): (i64,) =
            sqlx::query_as("select nextval(pg_get_serial_sequence('tickets', 'seq'))")
                .fetch_one(&mut *tx)
                .instrument(telemetry::db_span("next ticket seq"))
                .await
                .context("next ticket seq")?;
        let code = manual.map_or_else(|| code_for(seq), str::to_owned);
        let row = sqlx::query_as(&format!(
            "insert into tickets (seq, project, code, summary, body, priority, tags) overriding system value \
             values ($1, $2, $3, $4, $5, $6, $7) returning {TICKET_COLUMNS}"
        ))
        .bind(seq)
        .bind(project)
        .bind(&code)
        .bind(summary)
        .bind(new.body.trim())
        .bind(priority)
        .bind(&tags)
        .fetch_one(&mut *tx)
        .instrument(telemetry::db_span("insert ticket"))
        .await;
        match row {
            Ok(ticket) => {
                let ticket: Ticket = ticket;
                // Assignee and components in the same transaction: an unknown person cancels the creation too.
                people::apply(&mut tx, ticket.seq, &fields).await?;
                let fields = people::ticket_fields(&mut *tx, ticket.seq).await?;
                tx.commit().await.context("commit ticket")?;
                return Ok((StatusCode::CREATED, Json(TicketOut { ticket, fields })));
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() && manual.is_none() => {
                continue;
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(AppError::Client(
                    StatusCode::CONFLICT,
                    "code already exists",
                ));
            }
            Err(e) => return Err(anyhow::Error::new(e).context("insert ticket").into()),
        }
    }
    Err(anyhow::anyhow!("no free ticket code after 5 tries").into())
}

async fn update_ticket(
    State(db): State<PgPool>,
    actor: tx::Actor,
    Path(code): Path<String>,
    Json(patch): Json<TicketPatch>,
) -> Result<Json<TicketOut>, AppError> {
    let project = patch.project.as_deref().map(str::trim);
    if project.is_some_and(|p| !valid_project(p)) {
        return Err(bad(
            "project: lowercase latin letters, digits and dashes, up to 40",
        ));
    }
    let summary = patch.summary.as_deref().map(str::trim);
    if summary == Some("") {
        return Err(bad("summary is empty"));
    }
    let status = patch.status.as_deref().map(str::trim);
    if status.is_some_and(|s| !valid_status(s)) {
        return Err(bad("status: open, doing, done or cancelled"));
    }
    let priority = patch.priority.as_ref().map(|p| non_empty(p.as_deref()));
    if priority.flatten().is_some_and(|p| !valid_priority(p)) {
        return Err(bad("priority: p0, p1, p2 or p3"));
    }
    if patch.tags.as_deref().is_some_and(|t| !valid_tags(t)) {
        return Err(bad("tags: up to 30, each 1 to 60 bytes"));
    }
    let tags: Option<Vec<&str>> = patch
        .tags
        .as_ref()
        .map(|t| t.iter().map(|x| x.trim()).collect());
    let mut fields = people::Fields {
        assignee: patch.assignee.clone(),
        components: patch.components.clone(),
    };
    fields.check()?;
    let mut tx = tx::begin(&db, &actor).await?;
    let row: Option<Ticket> = sqlx::query_as(&format!(
        "update tickets set \
            project = coalesce($2, project), summary = coalesce($3, summary), status = coalesce($4, status), \
            body = coalesce($5, body), priority = case when $6 then $7 else priority end, \
            tags = coalesce($8, tags), updated_at = now() \
         where code = $1 returning {TICKET_COLUMNS}"
    ))
    .bind(&code)
    .bind(project)
    .bind(summary)
    .bind(status)
    .bind(patch.body.as_deref().map(str::trim))
    .bind(priority.is_some())
    .bind(priority.flatten())
    .bind(tags)
    .fetch_optional(&mut *tx)
    .instrument(telemetry::db_span("update ticket"))
    .await
    .context("update ticket")?;
    let Some(ticket) = row else {
        return Err(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"));
    };
    people::apply(&mut tx, ticket.seq, &fields).await?;
    let fields = people::ticket_fields(&mut *tx, ticket.seq).await?;
    tx.commit().await.context("commit ticket")?;
    Ok(Json(TicketOut { ticket, fields }))
}

#[derive(Serialize, sqlx::FromRow)]
struct DocumentRow {
    repo: String,
    path: String,
    role: String,
    title: String,
    created_at: chrono::DateTime<chrono::Utc>,
    retired_at: Option<chrono::DateTime<chrono::Utc>>,
    tickets: i64,
}

async fn list_documents(State(db): State<PgPool>) -> Result<Json<Vec<DocumentRow>>, AppError> {
    let rows = sqlx::query_as(
        "select d.repo, d.path, d.role, d.title, d.created_at, d.retired_at, \
                (select count(*) from ticket_documents l \
                  where l.repo = d.repo and l.path = d.path and l.valid_to is null) as tickets \
         from documents d order by d.created_at desc, d.path",
    )
    .fetch_all(&db)
    .instrument(telemetry::db_span("select documents"))
    .await
    .context("select documents")?;
    Ok(Json(rows))
}

/// `/health` is open for probes; the other routes are only for known callers (callers.rs).
fn app(db: PgPool, callers: callers::Callers) -> Router {
    let api = Router::new()
        .route("/api/tickets", get(list_tickets).post(create_ticket))
        .route("/api/tickets/{code}", get(get_ticket).patch(update_ticket))
        .route("/api/tickets/{code}/links", get(graph::ticket_links))
        .route("/api/tickets/{code}/fields", post(people::set_fields))
        .route("/api/tickets/{code}/estimate", put(plan::put_estimate))
        .route("/api/tickets/{code}/wsjf", put(plan::put_wsjf))
        .route("/api/tickets/{code}/plan", get(plan::get_ticket_plan))
        .route(
            "/api/scopes",
            get(plan::list_scopes).post(plan::create_scope),
        )
        .route("/api/scopes/{root}", put(plan::update_scope))
        .route("/api/plan/{root}", get(plan::get_plan))
        .route("/api/plan/{root}/baseline", post(plan::approve))
        .route("/api/plan/{root}/fever", get(plan::get_fever))
        .route("/api/next", get(plan::next))
        .route("/api/backlog", get(plan::backlog))
        .route(
            "/api/people",
            get(people::list_people).post(people::create_person),
        )
        .route(
            "/api/people/logins",
            get(logins::list_logins).post(logins::bind_login),
        )
        .route(
            "/api/people/logins/{handle}/retire",
            post(logins::retire_login),
        )
        .route(
            "/api/components",
            get(people::list_components).post(people::create_component),
        )
        .route("/api/graph", get(graph_view::get_graph))
        .route("/api/edges", post(graph::create_edge))
        .route("/api/edges/{id}/confirm", post(graph::confirm_edge))
        .route("/api/edges/{id}/close", post(graph::close_edge))
        .route("/api/projects", get(list_projects))
        .route("/api/ticket-page", get(query::get_ticket_page))
        .route("/api/ticket-stats", get(query::get_ticket_stats))
        .route("/api/document-page", get(query::get_document_page))
        .route("/api/documents", get(list_documents))
        // The static sync path takes precedence over the {*path} template: documents live under docs/, no sync path among them.
        .route(
            "/api/documents/sync",
            post(documents::sync).layer(DefaultBodyLimit::max(documents::SYNC_LIMIT)),
        )
        .route("/api/documents/{*path}", get(documents::get_document))
        .route("/api/mailbox/exchange", post(mailbox::exchange))
        .route("/api/mailbox/exchange-user", post(mailbox::exchange_user))
        .route("/api/mailbox/keys", get(mailbox::list_keys))
        .route("/api/mailbox/keys/{id}", patch(mailbox::set_key_status))
        .route("/api/mailbox/feed", get(mailbox::feed))
        .route_layer(axum::middleware::from_fn_with_state(
            callers,
            callers::require,
        ));
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(api)
        .with_state(db)
        .layer(axum::middleware::from_fn(telemetry::observe))
}

async fn shutdown() {
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

async fn run() -> anyhow::Result<()> {
    // Installation settings (step 2.3): without HUB_ACCOUNT or HUB_TIMEZONE the service does not start, before connecting to the database.
    let config = config::Config::from_env()?;
    let uri = std::env::var("HUBDB_URI").context("HUBDB_URI is not set")?;
    let port: u16 = std::env::var("PORT")
        .context("PORT is not set")?
        .parse()
        .context("PORT is not a port number")?;

    // Caller secrets (callers.rs): the directory is re-read every 30 s. In the cluster HUB_CALLERS_REQUIRED=true,
    // and hub does not start without secrets; the open mode remains only for local runs.
    let required = matches!(
        std::env::var("HUB_CALLERS_REQUIRED").as_deref(),
        Ok("true" | "1")
    );
    let dir = std::env::var_os("HUB_CALLERS_DIR").map(std::path::PathBuf::from);
    let set = match &dir {
        Some(dir) => callers::CallerSet::load(dir).context("load hub callers")?,
        None => callers::CallerSet::default(),
    };
    anyhow::ensure!(
        !(required && set.is_empty()),
        "HUB_CALLERS_REQUIRED=true, but HUB_CALLERS_DIR has no caller secrets"
    );
    if set.is_empty() {
        tracing::warn!(
            "no hub caller secrets (HUB_CALLERS_DIR unset or empty): /api is open to anyone who reaches the port"
        );
    } else {
        tracing::info!(callers = ?set.names(), "hub callers required on /api");
    }
    let callers = callers::Callers::from_set(set);
    if let Some(dir) = dir {
        callers.spawn_reload(dir);
    }
    // Seeds from the HUB_SEED_DIR directory (seed.rs) are read before the database: a missing required directory or an
    // unreadable file makes hub fail right away. So does an invalid default documents repository.
    let seeds = seed::Seeds::from_env()?;
    let default_repo = documents::default_repo()?;
    if default_repo.is_none() {
        tracing::warn!(
            "HUB_DEFAULT_DOCS_REPO is not set: GET /api/documents/<path> without ?repo= answers 400"
        );
    }

    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&uri)
        .await
        .context("connect to hub database")?;
    // Migrations run only in `hub migrate` (Job hub-db-migrate as the app owner); the service as role hub
    // only checks the journal and does not start if the database is behind the image (the old pod lives on with maxUnavailable 0).
    migrations::verify(&db, &migrations::migrator()?)
        .await
        .context("check database schema")?;
    // The instance row is seeded by `hub migrate`; the service does not start with a different account or without the row.
    config
        .check_instance(&db)
        .await
        .context("check public.instance")?;
    // Registry of document repositories (repositories.toml): first, before accepting syncs.
    documents::seed(&db, seeds.repositories.as_deref())
        .await
        .context("seed repositories")?;
    // People and components: ticket tags yield components.
    people::seed(&db, &seeds)
        .await
        .context("seed people and components")?;
    // Links from ticket text: after the seeds. A cycle or a database error does not
    // fail the start (ERROR log and hub.seed.links.* metrics); only an unparsable file does.
    graph_view::seed(&db, &seeds.links)
        .await
        .context("seed links")?;
    // Initial plan scopes: after links, so that children are already linked; does not create the database.
    plan_seed::seed(&db, seeds.scopes.as_deref())
        .await
        .context("seed scopes")?;
    // Scope plans are recomputed every 5 s from the plan.dirty queue and at 00:05 installation local time.
    plan::spawn(db.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, version = env!("CARGO_PKG_VERSION"), "hub listening");
    axum::serve(listener, app(db, callers))
        .with_graceful_shutdown(shutdown())
        .await
        .context("serve")
}

// The runtime is built by hand: telemetry starts before it and flushes the rest after it.
fn main() {
    let telemetry = telemetry::init(telemetry::export_enabled());
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async {
            // `hub migrate`: migrations only, without a port or caller secrets; otherwise the service.
            // `hub migrate --adopt` (steps 2.12c, 2.15): adopt the baseline first (HUB_MIGRATIONS=baseline only).
            let args: Vec<String> = std::env::args().skip(1).collect();
            match args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                [] => run().await,
                ["migrate"] => migrations::run(false).await,
                ["migrate", "--adopt"] => migrations::run(true).await,
                other => Err(anyhow::anyhow!(
                    "unknown command {other:?}: expected none, `migrate` or `migrate --adopt`"
                )),
            }
        });
    if let Err(err) = &result {
        tracing::error!(error = format!("{err:#}"), "hub failed");
    }
    telemetry.shutdown();
    if result.is_err() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    // A lazy pool opens no connection, so the tests run without Postgres.
    // Port 1 is closed: a database query fails after 100 ms, not after 30 s.
    fn lazy_db() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap()
    }

    // Without caller secrets (local run): the API is open.
    fn test_app() -> Router {
        app(lazy_db(), callers::Callers::default())
    }

    const WEB_SECRET: &str = "0123456789abcdef0123456789abcdef-web";
    const DOCS_SECRET: &str = "0123456789abcdef0123456789abcdef-docs";

    fn guarded_app() -> Router {
        app(
            lazy_db(),
            callers::Callers::from_set(
                callers::CallerSet::new([
                    ("hub-web", WEB_SECRET),
                    ("hub-ingest", "0123456789abcdef0123456789abcdef-ingest"),
                    ("docs-demo", DOCS_SECRET),
                ])
                .unwrap(),
            ),
        )
    }

    // A request that passes the caller check and is rejected by the handler before the database: tests have no database,
    // so "let through" means 400 from the handler (invalid status), not 200.
    fn ticket_request(caller: Option<&str>, auth: Option<&str>) -> Request<Body> {
        let mut req = Request::get("/api/tickets?status=closed");
        if let Some(c) = caller {
            req = req.header("x-hub-caller", c);
        }
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        req.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn api_needs_a_known_caller_with_its_secret_then_reaches_the_handler_400() {
        let bearer = format!("Bearer {WEB_SECRET}");
        for (caller, auth) in [
            (None, None),
            (Some("hub-web"), None),
            (None, Some(bearer.as_str())),
            (Some("hub-web"), Some("Bearer wrong")),
            (Some("hub-web"), Some(WEB_SECRET)),
            (Some("hub-ingest"), Some(bearer.as_str())),
            (Some("stranger"), Some(bearer.as_str())),
        ] {
            let res = guarded_app()
                .oneshot(ticket_request(caller, auth))
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{caller:?} {auth:?}"
            );
        }
        let res = guarded_app()
            .oneshot(ticket_request(Some("hub-web"), Some(&bearer)))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "400 from the handler (no database in tests) means the caller check passed"
        );
        // A POST body is not read without a caller either.
        let res = guarded_app()
            .oneshot(
                Request::post("/api/mailbox/exchange")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // A docs-* caller may only call document sync, and document sync is only for docs-*.
    #[tokio::test]
    async fn document_sync_is_open_only_to_docs_callers() {
        let call = |method: &str, uri: &str, caller: &str, secret: &str| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-hub-caller", caller)
                .header("authorization", format!("Bearer {secret}"))
                .header("content-type", "application/json")
                .body(Body::from("not json"))
                .unwrap()
        };
        for (method, uri, caller, secret) in [
            (
                "GET",
                "/api/tickets?status=closed",
                "docs-demo",
                DOCS_SECRET,
            ),
            (
                "GET",
                "/api/documents/docs/superpowers/specs/a.md",
                "docs-demo",
                DOCS_SECRET,
            ),
            ("POST", "/api/tickets", "docs-demo", DOCS_SECRET),
            ("POST", "/api/documents/sync", "hub-web", WEB_SECRET),
        ] {
            let res = guarded_app()
                .oneshot(call(method, uri, caller, secret))
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::FORBIDDEN,
                "{caller} {method} {uri}"
            );
            let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
            assert_eq!(&body[..], br#"{"error":"caller_not_allowed"}"#);
        }
        // Its own route: 400 from the handler (body is not JSON), before the database.
        let res = guarded_app()
            .oneshot(call(
                "POST",
                "/api/documents/sync",
                "docs-demo",
                DOCS_SECRET,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // Snapshot body limit is 32 MiB: exactly 32 MiB reach the handler (400: repo is not a string), 33 MiB give 413.
    #[tokio::test]
    async fn document_sync_takes_32_mib_and_refuses_33() {
        for (size, want) in [
            (documents::SYNC_LIMIT, StatusCode::BAD_REQUEST),
            (33 << 20, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let mut body = br#"{"repo":1}"#.to_vec();
            body.resize(size, b' ');
            let res = test_app()
                .oneshot(
                    Request::post("/api/documents/sync")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), want, "{size} bytes");
        }
    }

    // Actor and kind (tx.rs): the kind is checked before the database. Confirmation by an agent is 403 JSON, a human from the loader 403,
    // a human from hub-web reaches the database (the tests have none: 500).
    #[tokio::test]
    async fn actor_kind_is_checked_by_caller_before_the_database() {
        let confirm = |caller: &str, secret: &str, kind: Option<&str>| {
            let mut req = Request::post("/api/edges/1/confirm")
                .header("x-hub-caller", caller)
                .header("authorization", format!("Bearer {secret}"));
            if let Some(k) = kind {
                req = req
                    .header("x-hub-actor-kind", k)
                    .header("x-hub-actor", "user:1");
            }
            req.body(Body::empty()).unwrap()
        };
        let res = guarded_app()
            .oneshot(confirm("hub-web", WEB_SECRET, Some("agent")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], br#"{"error":"confirm_requires_human"}"#);
        let res = guarded_app()
            .oneshot(confirm(
                "hub-ingest",
                "0123456789abcdef0123456789abcdef-ingest",
                Some("human"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "human from the loader");
        let res = guarded_app()
            .oneshot(confirm("hub-web", WEB_SECRET, None))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::FORBIDDEN,
            "hub-web without a session is service:hub-web, not human"
        );
        let res = guarded_app()
            .oneshot(confirm("hub-web", WEB_SECRET, Some("human")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn plan_bodies_and_actor_are_checked_before_the_database() {
        // There is no database, so 400 and 403 mean the request was rejected before it.
        let cases = [
            (
                "PUT",
                "/api/tickets/a/estimate",
                "agent",
                r#"{"effort_sp":4}"#,
                400,
            ),
            ("PUT", "/api/tickets/a/estimate", "agent", r#"{}"#, 400),
            (
                "PUT",
                "/api/tickets/a/estimate",
                "agent",
                r#"{"effort_sp":3,"aggressive_hd":4,"safe_hd":2}"#,
                400,
            ),
            (
                "PUT",
                "/api/tickets/a/wsjf",
                "agent",
                r#"{"business_value":4,"time_criticality":1,"risk_opportunity":1}"#,
                400,
            ),
            ("POST", "/api/scopes", "agent", r#"{}"#, 400),
            (
                "POST",
                "/api/scopes",
                "agent",
                r#"{"root":"a","mode":"backward"}"#,
                400,
            ),
            ("PUT", "/api/scopes/a", "agent", r#"{"root":"a"}"#, 400),
            ("POST", "/api/plan/a/baseline", "agent", r#"{}"#, 403),
            ("GET", "/api/next?person=Bad_One", "agent", "", 400),
            ("GET", "/api/next", "agent", "", 400),
            ("GET", "/api/backlog?limit=0", "agent", "", 400),
            ("GET", "/api/backlog?project=Bad", "agent", "", 400),
        ];
        for (method, uri, kind, body, want) in cases {
            let mut req = Request::builder()
                .method(method)
                .uri(uri)
                .header(tx::ACTOR_KIND_HEADER, kind)
                .header(tx::ACTOR_HEADER, "user:1");
            if !body.is_empty() {
                req = req.header("content-type", "application/json");
            }
            let res = test_app()
                .oneshot(req.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status().as_u16(), want, "{method} {uri} {body}");
        }
    }

    #[tokio::test]
    async fn edge_bodies_are_checked_before_the_database() {
        for (uri, body) in [
            (
                "/api/edges",
                r#"{"src":{"kind":"ticket","key":"a"},"rel":"blocks","dst":{"kind":"ticket","key":"a"}}"#,
            ),
            ("/api/edges/1/close", r#"{"reason":"gone"}"#),
        ] {
            let res = test_app()
                .oneshot(
                    Request::post(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                res.status().is_client_error(),
                "{uri} {body}: {}",
                res.status()
            );
        }
    }

    #[tokio::test]
    async fn health_stays_open_and_import_is_gone() {
        let health = guarded_app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let bearer = format!("Bearer {WEB_SECRET}");
        let import = guarded_app()
            .oneshot(
                Request::post("/api/mailbox/import")
                    .header("x-hub-caller", "hub-web")
                    .header("authorization", &bearer)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn without_caller_secrets_the_api_stays_open_and_reaches_the_handler_400() {
        let res = test_app()
            .oneshot(ticket_request(None, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn health_is_ok_and_empty_ticket_is_rejected() {
        let health = test_app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let bad = test_app()
            .oneshot(
                Request::post("/api/tickets")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"project":"acme","code":"  ","summary":"x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    // The project is checked before the database: an invalid name gives 400, not 500 from a Postgres constraint.
    #[tokio::test]
    async fn bad_project_is_rejected_before_the_database() {
        for body in [
            r#"{"project":"","code":"a","summary":"x"}"#,
            r#"{"project":"Demo Project","code":"a","summary":"x"}"#,
            r#"{"code":"a","summary":"x"}"#,
            r#"{"project":null,"code":"a","summary":"x"}"#,
        ] {
            let res = test_app()
                .oneshot(
                    Request::post("/api/tickets")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let res = test_app()
            .oneshot(
                Request::get("/api/tickets?project=Bad_Name")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn codes_follow_the_old_roadmap() {
        for (seq, code) in [
            (0, "0"),
            (1, "1"),
            (10, "a"),
            (18, "j"),
            (31, "z"),
            (32, "10"),
            (1001, "z9"),
        ] {
            assert_eq!(code_for(seq), code, "{seq}");
        }
    }

    // Update and create checks fire before the database.
    #[tokio::test]
    async fn bad_fields_are_rejected_before_the_database() {
        for (method, uri, body) in [
            ("PATCH", "/api/tickets/a", r#"{"status":"closed"}"#),
            ("PATCH", "/api/tickets/a", r#"{"priority":"p9"}"#),
            ("PATCH", "/api/tickets/a", r#"{"summary":"  "}"#),
            ("PATCH", "/api/tickets/a", r#"{"project":"Bad"}"#),
            ("PATCH", "/api/tickets/a", r#"{"tags":[" "]}"#),
            (
                "POST",
                "/api/tickets",
                r#"{"project":"acme","summary":"x","priority":"high"}"#,
            ),
            (
                "POST",
                "/api/tickets",
                r#"{"project":"acme","summary":"x","code":""}"#,
            ),
            ("GET", "/api/tickets?status=closed", ""),
        ] {
            let res = test_app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::BAD_REQUEST,
                "{method} {uri} {body}"
            );
        }
    }

    #[test]
    fn patch_tells_missing_assignee_from_null() {
        let p: TicketPatch = serde_json::from_str(r#"{"status":"done"}"#).unwrap();
        assert_eq!((p.assignee, p.components), (None, None));
        let p: TicketPatch = serde_json::from_str(r#"{"assignee":null,"components":[]}"#).unwrap();
        assert_eq!((p.assignee, p.components), (Some(None), Some(vec![])));
        let p: TicketPatch = serde_json::from_str(r#"{"assignee":"alice"}"#).unwrap();
        assert_eq!(p.assignee, Some(Some("alice".into())));
    }

    // Assignee and components are checked before the database: 400, not 500 from a CHECK or 404 from a node lookup.
    #[tokio::test]
    async fn bad_assignee_and_components_are_rejected_before_the_database() {
        for (method, uri, body) in [
            ("PATCH", "/api/tickets/a", r#"{"assignee":"Alice Brown"}"#),
            ("PATCH", "/api/tickets/a", r#"{"components":["bad_slug"]}"#),
            (
                "POST",
                "/api/tickets",
                r#"{"project":"acme","summary":"x","assignee":"1alice"}"#,
            ),
            ("POST", "/api/tickets/a/fields", r#"{"assignee":"-x"}"#),
            ("POST", "/api/tickets/a/fields", "{}"),
            ("POST", "/api/people", r#"{"handle":"Alice","name":"Eve"}"#),
            ("POST", "/api/components", r#"{"slug":"a_b","title":"X"}"#),
            (
                "POST",
                "/api/components",
                r#"{"slug":"ab","title":"X","tags":["x"]}"#,
            ),
            ("GET", "/api/ticket-page?assignee=1ed", ""),
            ("GET", "/api/ticket-page?component=a_b", ""),
        ] {
            let res = test_app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::BAD_REQUEST,
                "{method} {uri} {body}"
            );
        }
    }

    #[test]
    fn patch_tells_missing_priority_from_null() {
        let p: TicketPatch = serde_json::from_str(r#"{"status":"done"}"#).unwrap();
        assert!(p.priority.is_none());
        let p: TicketPatch = serde_json::from_str(r#"{"priority":null}"#).unwrap();
        assert_eq!(p.priority, Some(None));
        let p: TicketPatch = serde_json::from_str(r#"{"priority":"p1"}"#).unwrap();
        assert_eq!(p.priority, Some(Some("p1".into())));
    }

    #[test]
    fn statuses() {
        for ok in ["open", "doing", "done", "cancelled"] {
            assert!(valid_status(ok), "{ok}");
        }
        for bad in ["", "closed", "archived", "Done", " open"] {
            assert!(!valid_status(bad), "{bad:?}");
        }
    }

    #[test]
    fn project_names() {
        for ok in ["acme", "demo", "acme-hub", "a1", "0x"] {
            assert!(valid_project(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "Acme",
            "acme hub",
            "acme_hub",
            "项目",
            &"a".repeat(41),
        ] {
            assert!(!valid_project(bad), "{bad}");
        }
    }

    // Without OTEL_EXPORTER_OTLP_ENDPOINT there are no exporters, and a service with the telemetry layer responds as before.
    #[tokio::test]
    async fn without_otlp_endpoint_nothing_is_exported_and_requests_are_served() {
        let telemetry = telemetry::init(false);
        assert!(!telemetry.exporting());
        let health = test_app()
            .oneshot(
                Request::get("/health")
                    .header(
                        "traceparent",
                        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        telemetry.shutdown();
    }

    // A real handler without a database: the response is 500, and the log has the whole chain of causes on one line.
    #[tokio::test]
    async fn failed_request_logs_the_whole_error_chain() {
        #[derive(Clone, Default)]
        struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Log::default();
        let sink = log.clone();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_writer(move || sink.clone())
                .finish(),
        );
        let res = test_app()
            .oneshot(Request::get("/api/tickets").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let log = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains(r#""error":"select tickets: pool timed out"#),
            "{log}"
        );
    }
}
