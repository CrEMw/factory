//! Documents of all installation repositories.
//!
//! POST /api/documents/sync          a snapshot of a repository's documents from git, only for `docs-*` callers (callers.rs), the
//!                                    caller's repository from the `document_repository` registry; body up to [`SYNC_LIMIT`]
//! GET  /api/documents/{*path}?repo= a document, its project, live edges to other documents and tickets with rel
//!
//! The snapshot rules live in SQL (`public.sync_repository_documents`, migrations 0005 and 0017): here only the
//! caller check, a transaction with actor `service:<caller>` and translation of database errors into API responses:
//! 409 `stale`, 422 `invalid_snapshot`, `invalid_json`, `unknown_repo`, 403 `repo_not_allowed`.
//!
//! GET without `repo` (old `/d/<path>` links) reads the repository from `HUB_DEFAULT_DOCS_REPO`; without the variable such a request
//! is 400 `repo_required` (step 2.6 of the factory plan). The image may set it (`services/Dockerfile`).
//!
//! The repository registry (`public.document_repository`) is filled by the `repositories.toml` seed from the `HUB_SEED_DIR` directory ([`seed`],
//! seed.rs): the import adds and updates, never deletes (0024_seed_repos.sql).

use anyhow::Context;
use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::LazyLock;
use tracing::Instrument;

use crate::{
    AppError,
    callers::Caller,
    callers::UNAUTHENTICATED,
    non_empty,
    query::valid_repo,
    telemetry,
    tx::{self, Actor},
};

/// Snapshot body limit: the largest corpus today is 13.9 MB. If corpora grow: a two-phase protocol (manifest, then bodies).
pub const SYNC_LIMIT: usize = 32 << 20;
/// Prefix of the callers that may only call [`sync`] (callers.rs).
pub const DOCS_CALLER_PREFIX: &str = "docs-";

fn err(status: StatusCode, body: Value) -> AppError {
    AppError::Json(status, body)
}

/// Default repository for GET without `repo`: `HUB_DEFAULT_DOCS_REPO`, an empty string counts as unset. Read
/// once; [`default_repo`] checks its shape at startup.
static DEFAULT_REPO: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var("HUB_DEFAULT_DOCS_REPO")
        .ok()
        .and_then(|r| non_empty(Some(&r)).map(str::to_owned))
});

/// Check of `HUB_DEFAULT_DOCS_REPO` at startup: a bad shape fails the start, not every request.
pub fn default_repo() -> anyhow::Result<Option<&'static str>> {
    let repo = DEFAULT_REPO.as_deref();
    if let Some(r) = repo {
        anyhow::ensure!(
            valid_repo(r),
            "HUB_DEFAULT_DOCS_REPO {r:?} is not owner/name"
        );
    }
    Ok(repo)
}

/// Explanation of the 400 `repo_required` response.
const REPO_REQUIRED: &str = "?repo=owner/name is required: HUB_DEFAULT_DOCS_REPO is not set";

/// Repository of a request: its own, else the default, else 400 `repo_required`.
fn resolve_repo<'a>(asked: Option<&'a str>, default: Option<&'a str>) -> Result<&'a str, AppError> {
    non_empty(asked).or(default).ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            json!({"error": "repo_required", "detail": REPO_REQUIRED}),
        )
    })
}

/// A row of the `repositories.toml` seed; field names as in `public.import_document_repositories`.
#[derive(Deserialize, Serialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeedRepository {
    pub repo: String,
    pub project: String,
    pub caller: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoriesSeed {
    #[serde(default)]
    repository: Vec<SeedRepository>,
}

/// Parsing and checking the repository seed: registry CHECK rules, caller `docs-<name>`, no duplicate repository or caller.
pub fn parse_repositories(text: &str) -> anyhow::Result<Vec<SeedRepository>> {
    let seed: RepositoriesSeed = toml::from_str(text).context("repositories.toml")?;
    let mut seen = std::collections::BTreeSet::new();
    for r in &seed.repository {
        anyhow::ensure!(
            valid_repo(&r.repo),
            "repositories.toml {}: repo is owner/name",
            r.repo
        );
        anyhow::ensure!(
            crate::valid_project(&r.project),
            "repositories.toml {}: bad project {:?}",
            r.repo,
            r.project
        );
        let name = r.caller.strip_prefix(DOCS_CALLER_PREFIX).unwrap_or("");
        anyhow::ensure!(
            name.len() <= 35 && crate::valid_project(name),
            "repositories.toml {}: caller is docs-<name>, not {:?}",
            r.repo,
            r.caller
        );
        anyhow::ensure!(
            seen.insert(r.repo.clone()) && seen.insert(format!("caller:{}", r.caller)),
            "repositories.toml {}: repo or caller twice",
            r.repo
        );
    }
    Ok(seed.repository)
}

/// Repository registry seed at startup (before the Roadmap snapshot and syncs): one transaction, the function sets the actor
/// `seed:repositories` and the snapshot lock. A broken file or a database refusal fails the start: the rollout stalls, the old pod
/// keeps working. No file: no import.
pub async fn seed(db: &PgPool, text: Option<&str>) -> anyhow::Result<Option<Value>> {
    let Some(text) = text else {
        return Ok(None);
    };
    let repos = serde_json::to_value(parse_repositories(text)?)?;
    let mut tx = db.begin().await.context("begin seed repositories")?;
    let (report,): (Value,) =
        sqlx::query_as("select public.import_document_repositories($1::jsonb)")
            .bind(&repos)
            .fetch_one(&mut *tx)
            .instrument(telemetry::db_span("import document repositories"))
            .await
            .context("import document repositories")?;
    tx.commit().await.context("commit seed repositories")?;
    tracing::info!(report = %report, "seed repositories");
    Ok(Some(report))
}

/// Only repo is needed from the body: the database parses the rest. serde skips other fields without allocating.
#[derive(Deserialize)]
struct Head {
    repo: String,
}

/// Snapshot function errors into responses; everything else: 500 with context.
fn db_error(e: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(d) = &e {
        let message = d.message().chars().take(300).collect::<String>();
        let mapped = match d.code().as_deref() {
            Some("DSSTL") => Some((StatusCode::CONFLICT, "stale")),
            Some("DSINV") => Some((StatusCode::UNPROCESSABLE_ENTITY, "invalid_snapshot")),
            Some("DSREP") => Some((StatusCode::UNPROCESSABLE_ENTITY, "unknown_repo")),
            Some("DSCAL") => Some((StatusCode::FORBIDDEN, "repo_not_allowed")),
            // The body is not JSON, or JSON with \u0000, which Postgres text cannot hold.
            Some("22P02" | "22P05") => Some((StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")),
            _ => None,
        };
        if let Some((status, error)) = mapped {
            return err(status, json!({ "error": error, "detail": message }));
        }
    }
    anyhow::Error::new(e).context("sync documents").into()
}

pub async fn sync(
    State(db): State<PgPool>,
    caller: Option<Extension<Caller>>,
    actor: Actor,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    let invalid = || {
        err(
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid_json", "detail": "a JSON object with a string repo"}),
        )
    };
    let text = std::str::from_utf8(&body).map_err(|_| invalid())?;
    let head: Head = serde_json::from_str(text).map_err(|_| invalid())?;
    let caller = caller.map_or(UNAUTHENTICATED.to_owned(), |Extension(c)| c.0);
    // A caller writes only its own repository. Open mode (local run without secrets) has nobody to check.
    if caller != UNAUTHENTICATED {
        let own: Option<(String,)> =
            sqlx::query_as("select repo from document_repository where caller = $1")
                .bind(&caller)
                .fetch_optional(&db)
                .instrument(telemetry::db_span("select document repository"))
                .await
                .context("select document repository")?;
        if own.as_ref().map(|r| r.0.as_str()) != Some(head.repo.as_str()) {
            tracing::warn!(caller = %caller, "document sync for a repository the caller does not own");
            return Err(err(
                StatusCode::FORBIDDEN,
                json!({"error": "repo_not_allowed", "detail": format!("{caller} does not write this repository")}),
            ));
        }
    }
    let mut tx = tx::begin(&db, &actor).await?;
    let (result,): (Value,) = sqlx::query_as("select public.sync_repository_documents($1::jsonb)")
        .bind(text)
        .fetch_one(&mut *tx)
        .instrument(telemetry::db_span("sync documents"))
        .await
        .map_err(db_error)?;
    tx.commit().await.context("commit document sync")?;
    tracing::info!(
        repo = %head.repo,
        revision = result["revision"].as_str().unwrap_or(""),
        documents = result["documents"].as_i64().unwrap_or(0),
        updated = result["updated"].as_i64().unwrap_or(0),
        retired = result["retired"].as_i64().unwrap_or(0),
        problems = result["problems"].as_array().map_or(0, Vec::len),
        "documents synced"
    );
    Ok(Json(result))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Document {
    repo: String,
    path: String,
    role: String,
    title: String,
    body: String,
    sha256: String,
    source_revision: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    synced_at: chrono::DateTime<chrono::Utc>,
    retired_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TicketRef {
    code: String,
    project: String,
    summary: String,
    status: String,
    rel: String,
}

/// A live edge from the document to another document. dir out: this document is src (it implements, it references).
#[derive(Serialize, sqlx::FromRow)]
pub struct DocumentLink {
    edge_id: i64,
    rel: String,
    dir: String,
    state: String,
    repo: String,
    path: String,
    title: String,
    role: String,
    retired: bool,
}

#[derive(Serialize)]
pub struct DocumentDetail {
    #[serde(flatten)]
    document: Document,
    /// Project from the repository registry (live in_project edge), else None.
    project: Option<String>,
    documents: Vec<DocumentLink>,
    tickets: Vec<TicketRef>,
}

#[derive(Deserialize)]
pub struct DocumentQuery {
    repo: Option<String>,
}

pub async fn get_document(
    State(db): State<PgPool>,
    Path(path): Path<String>,
    Query(q): Query<DocumentQuery>,
) -> Result<Json<DocumentDetail>, AppError> {
    // Without repo: the installation default repository, for the sake of old links.
    let repo = resolve_repo(q.repo.as_deref(), DEFAULT_REPO.as_deref())?;
    let document: Option<Document> = sqlx::query_as(
        "select repo, path, role, title, body, sha256, source_revision, created_at, synced_at, retired_at \
         from documents where repo = $1 and path = $2",
    )
    .bind(repo)
    .bind(&path)
    .fetch_optional(&db)
    .instrument(telemetry::db_span("select document"))
    .await
    .context("select document")?;
    let Some(document) = document else {
        return Err(AppError::Client(StatusCode::NOT_FOUND, "no such document"));
    };
    let tickets = sqlx::query_as(
        "select t.code, t.project, t.summary, t.status, l.rel from ticket_documents l \
         join tickets t on t.seq = l.ticket_seq \
         where l.repo = $1 and l.path = $2 and l.valid_to is null order by l.rel, t.seq",
    )
    .bind(repo)
    .bind(&path)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select document tickets"))
    .await
    .context("select document tickets")?;
    let (project,): (Option<String>,) = sqlx::query_as(
        "select (select n.key from graph.edge e join graph.node n on n.id = e.dst \
                 where e.src = d.node_id and e.rel = 'in_project' and e.valid_to is null \
                 order by e.id limit 1) \
         from (select (select node_id from graph.document where account = (select public.instance_account()) and repo = $1 and path = $2) \
               as node_id) d",
    )
    .bind(repo)
    .bind(&path)
    .fetch_one(&db)
    .instrument(telemetry::db_span("select document project"))
    .await
    .context("select document project")?;
    let documents = sqlx::query_as(
        "with me as (select node_id from graph.document where account = (select public.instance_account()) and repo = $1 and path = $2) \
         select e.id as edge_id, e.rel, case when e.src = me.node_id then 'out' else 'in' end as dir, e.state, \
                o.repo, o.path, d.title, d.role, d.retired_at is not null as retired \
         from me join graph.edge e on (e.src = me.node_id or e.dst = me.node_id) and e.valid_to is null \
         join graph.document o on o.node_id = case when e.src = me.node_id then e.dst else e.src end \
         join documents d on d.repo = o.repo and d.path = o.path \
         order by e.rel, dir, o.repo, o.path",
    )
    .bind(repo)
    .bind(&path)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select document links"))
    .await
    .context("select document links")?;
    Ok(Json(DocumentDetail {
        document,
        project,
        documents,
        tickets,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_repo_needs_a_default() {
        assert_eq!(resolve_repo(Some("a/b"), None).ok(), Some("a/b"));
        assert_eq!(
            resolve_repo(Some(" "), Some("acme/widgets")).ok(),
            Some("acme/widgets")
        );
        assert_eq!(
            resolve_repo(None, Some("acme/widgets")).ok(),
            Some("acme/widgets")
        );
        match resolve_repo(None, None) {
            Err(AppError::Json(status, body)) => {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(body["error"], "repo_required");
            }
            _ => panic!("400 repo_required expected"),
        }
    }

    // Every seed in the tree parses (an install seed too, when the tree has one); the example carries acme/widgets.
    #[test]
    fn seeds_parse() {
        for dir in crate::seed::tree_dirs() {
            let seeds = crate::seed::Seeds::read(&dir).unwrap();
            let repos = parse_repositories(seeds.repositories.as_deref().unwrap()).unwrap();
            assert!(!repos.is_empty(), "{}", dir.display());
        }
        let example = crate::seed::Seeds::read(&crate::seed::example_dir()).unwrap();
        let repos = parse_repositories(example.repositories.as_deref().unwrap()).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo, "acme/widgets");
        assert!(parse_repositories("").unwrap().is_empty());
    }

    #[test]
    fn bad_repository_seeds_fail() {
        let row = |repo: &str, project: &str, caller: &str| {
            format!(
                "[[repository]]\nrepo = \"{repo}\"\nproject = \"{project}\"\ncaller = \"{caller}\"\n"
            )
        };
        for bad in [
            row("acme", "w", "docs-w"),
            row("acme/w", "W", "docs-w"),
            row("acme/w", "w", "w"),
            row("acme/w", "w", "docs-"),
            row("acme/w", "w", "docs-W"),
            row("acme/w", "w", &format!("docs-{}", "a".repeat(36))),
            row("acme/w", "w", "docs-w") + &row("acme/w", "v", "docs-v"),
            row("acme/w", "w", "docs-w") + &row("acme/v", "v", "docs-w"),
            row("acme/w", "w", "docs-w") + "owner = \"x\"\n",
        ] {
            assert!(parse_repositories(&bad).is_err(), "{bad}");
        }
        assert!(parse_repositories(&row("acme/w", "w", "docs-w")).is_ok());
    }
}
