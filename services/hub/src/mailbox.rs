//! Agent mailbox.
//!
//! The exchange rules live in SQL (`migrations/0008_agent_mailbox.sql`, checked by
//! `tools/test-agent-mailbox.mjs`); here: parameter parsing, the key hash and the response text.
//! The curl key is not stored in the hub and never written to spans or logs: only `sha256:<hex>`.
//! A message from another agent is data, not an order.
//!
//! Owner check (migration 0022_person_login.sql): the MCP key owner is checked against the sign-in binding on /people
//! ([`owner_allowed`], 403 `agent_owner_mismatch`), and only the owning human per binding approves and revokes the key,
//! with a fresh sign-in (`mailbox.review_key`).

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::Instrument;

use anyhow::Context;

use crate::{
    AppError, bad, logins, telemetry,
    tx::{self, Actor, Fresh},
};

const MAX_MSG_CHARS: usize = 1000;

#[derive(Debug, PartialEq)]
pub struct Exchange {
    pub owner: String,
    pub agent: String,
    pub msg: String,
}

/// `<owner>-<slug>`: owner `[a-z0-9]+`, slug `[A-Za-z0-9_.-]{1,60}`; text non-empty after trim, at most 1000 characters.
pub fn validate(agent: &str, msg: &str) -> Result<Exchange, &'static str> {
    let (owner, slug) = agent.split_once('-').unwrap_or(("", ""));
    let owner_ok = !owner.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let slug_ok = (1..=60).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if !owner_ok || !slug_ok {
        return Err("agent must be <owner>-<slug>: [a-z0-9]+-[A-Za-z0-9_.-]{1,60}");
    }
    let msg = msg.trim();
    if msg.is_empty() {
        return Err("msg required (non-empty)");
    }
    if msg.chars().count() > MAX_MSG_CHARS {
        return Err("msg too long (max 1000)");
    }
    Ok(Exchange {
        owner: owner.to_owned(),
        agent: agent.to_owned(),
        msg: msg.to_owned(),
    })
}

/// curl key: `[A-Za-z0-9_-]{32,128}`. Only its hash goes to the database.
pub fn key_id(key: &str) -> Result<String, &'static str> {
    let ok = (32..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !ok {
        return Err("key must match [A-Za-z0-9_-]{32,128}");
    }
    let digest = Sha256::digest(key.as_bytes());
    Ok(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct Outcome {
    pub status: String,
    #[serde(default)]
    pub messages: Vec<Delivered>,
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct Delivered {
    pub from: String,
    pub text: String,
}

/// Text for the agent: one `from: text` line per message, an empty body means nothing new, otherwise the status in one word.
/// Line breaks inside a message (with surrounding spaces) become a single space.
pub fn render(outcome: &Outcome) -> String {
    if outcome.status != "approved" {
        return format!("{}\n", outcome.status);
    }
    outcome
        .messages
        .iter()
        .map(|m| format!("{}: {}\n", m.from, one_line(&m.text)))
        .collect()
}

fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i == 0 {
            out.push_str(line.trim_end());
        } else {
            out.push(' ');
            out.push_str(line.trim());
        }
    }
    out
}

fn text(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn run(db: &PgPool, key: &str, ex: &Exchange, trusted: bool) -> Result<Response, AppError> {
    let value: serde_json::Value =
        sqlx::query_scalar("select mailbox.exchange($1, $2, $3, $4, now(), $5)")
            .bind(key)
            .bind(&ex.owner)
            .bind(&ex.agent)
            .bind(&ex.msg)
            .bind(trusted)
            .fetch_one(db)
            .instrument(telemetry::db_span("mailbox exchange"))
            .await?;
    let outcome: Outcome = serde_json::from_value(value)?;
    if outcome.status == "moving" {
        return Ok(text(
            StatusCode::SERVICE_UNAVAILABLE,
            "mailbox is moving\n".to_owned(),
        ));
    }
    Ok(text(StatusCode::OK, render(&outcome)))
}

// The key comes in the POST body, not in the URL: URLs end up in spans and logs.
#[derive(Deserialize)]
pub struct CurlExchange {
    #[serde(default)]
    agent: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    msg: String,
}

/// `POST /api/mailbox/exchange {agent, key, msg}`: exchange by curl key (hub-web forwards the public call).
pub async fn exchange(
    State(db): State<PgPool>,
    Json(q): Json<CurlExchange>,
) -> Result<Response, AppError> {
    let ex = match validate(&q.agent, &q.msg) {
        Ok(ex) => ex,
        Err(e) => return Ok(text(StatusCode::BAD_REQUEST, format!("{e}\n"))),
    };
    let key = match key_id(&q.key) {
        Ok(k) => k,
        Err(e) => return Ok(text(StatusCode::BAD_REQUEST, format!("{e}\n"))),
    };
    run(&db, &key, &ex, false).await
}

#[derive(Deserialize)]
pub struct UserExchange {
    user_id: String,
    agent: String,
    msg: String,
}

/// MCP key owner per sign-in binding (transitional mode of the spec "Human identity").
/// `own`: handle of this account's live binding; `prefix_taken`: the `agent` prefix is bound to another account.
/// With a binding the prefix must match its handle. Without a binding the account works as before (owner from the
/// prefix), except for a prefix already taken by someone else's binding. Violation: `agent_owner_mismatch`.
pub fn owner_allowed(prefix: &str, own: Option<&str>, prefix_taken: bool) -> bool {
    match own {
        Some(handle) => handle == prefix,
        None => !prefix_taken,
    }
}

/// `POST /api/mailbox/exchange-user`: exchange on behalf of the signed-in hub user (MCP).
pub async fn exchange_user(
    State(db): State<PgPool>,
    Json(b): Json<UserExchange>,
) -> Result<Response, AppError> {
    let user = b.user_id.trim();
    if user.is_empty() || user.len() > 200 {
        return Err(bad("user_id is required"));
    }
    let ex = match validate(&b.agent, &b.msg) {
        Ok(ex) => ex,
        Err(e) => return Ok(text(StatusCode::BAD_REQUEST, format!("{e}\n"))),
    };
    let (own, taken): (Option<String>, bool) = sqlx::query_as(
        "select (select handle from graph.person_login \
                  where account = (select public.instance_account()) and user_id = $1 and retired_at is null), \
                exists (select 1 from graph.person_login \
                  where account = (select public.instance_account()) and handle = $2 and user_id <> $1 and retired_at is null)",
    )
    .bind(user)
    .bind(&ex.owner)
    .fetch_one(&db)
    .instrument(telemetry::db_span("mailbox login owner"))
    .await?;
    if !owner_allowed(&ex.owner, own.as_deref(), taken) {
        tracing::warn!(
            agent_owner = ex.owner.as_str(),
            login = own.as_deref().unwrap_or(""),
            "mailbox agent owner differs from the login"
        );
        let hint = match &own {
            Some(h) => format!("your login is bound to {h} on /people: use agent={h}-<session>"),
            None => format!(
                "{} is bound to another login on /people: bind your own login there",
                ex.owner
            ),
        };
        return Err(AppError::Json(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": "agent_owner_mismatch", "agent_owner": ex.owner, "login": own, "hint": hint}),
        ));
    }
    run(&db, &format!("user:{user}"), &ex, true).await
}

#[derive(Serialize, sqlx::FromRow)]
pub struct KeyRow {
    id: String,
    owner: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    approved_at: Option<chrono::DateTime<chrono::Utc>>,
    last_seen_at: chrono::DateTime<chrono::Utc>,
    /// The owner has a live sign-in binding. If not, any bound human may revoke the pending key.
    owner_bound: bool,
}

/// `KeyRow` columns from `mailbox.keys` rows under the alias `k`.
const KEY_COLUMNS: &str = "k.id, k.owner, k.status, k.created_at, k.approved_at, k.last_seen_at, \
     exists (select 1 from graph.person_login l \
             where l.account = (select public.instance_account()) and l.handle = k.owner and l.retired_at is null) as owner_bound";

/// `GET /api/mailbox/keys`: pending first, then by last call.
pub async fn list_keys(State(db): State<PgPool>) -> Result<Json<Vec<KeyRow>>, AppError> {
    let rows = sqlx::query_as(&format!(
        "select {KEY_COLUMNS} from mailbox.keys k \
         order by (k.status = 'pending') desc, k.last_seen_at desc"
    ))
    .fetch_all(&db)
    .instrument(telemetry::db_span("mailbox keys"))
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct KeyStatus {
    status: String,
}

/// `PATCH /api/mailbox/keys/{id}`: approve or revoke (and restore a revoked one). Only a human
/// with a fresh sign-in whose binding on /people equals the key owner (`mailbox.review_key` in the database, journal
/// `mailbox.key_review` with the actor). Exception: any bound human may revoke a pending key of an owner without a live binding,
/// otherwise junk keys would occupy the shared pending limit forever. Otherwise 403 `requires_human`,
/// `fresh_session_required`, `no_login` (the human has no binding) or `not_key_owner`; no key 404 `no_such_key`.
pub async fn set_key_status(
    State(db): State<PgPool>,
    actor: Actor,
    Fresh(fresh): Fresh,
    Path(id): Path<String>,
    Json(b): Json<KeyStatus>,
) -> Result<Json<KeyRow>, AppError> {
    if b.status != "approved" && b.status != "revoked" {
        return Err(bad("status: approved or revoked"));
    }
    logins::require_fresh_human(&actor, fresh)?;
    let mut tx = tx::begin(&db, &actor).await?;
    tx::set_fresh(&mut tx).await?;
    let row: KeyRow = sqlx::query_as(&format!(
        "select {KEY_COLUMNS} from mailbox.review_key($1, $2) k"
    ))
    .bind(&id)
    .bind(&b.status)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("mailbox key status"))
    .await
    .map_err(|e| logins::identity_error(e, "mailbox key status"))?;
    tx.commit().await.context("commit mailbox key status")?;
    tracing::info!(
        owner = row.owner.as_str(),
        status = row.status.as_str(),
        "mailbox key reviewed"
    );
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct FeedQuery {
    #[serde(default)]
    after_id: i64,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct FeedRow {
    id: i64,
    agent: String,
    text: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/mailbox/feed?after_id`: without after_id the last 50, otherwise everything newer (up to 200); oldest first.
pub async fn feed(
    State(db): State<PgPool>,
    Query(q): Query<FeedQuery>,
) -> Result<Json<Vec<FeedRow>>, AppError> {
    let rows = if q.after_id <= 0 {
        sqlx::query_as(
            "select * from (select id, agent, text, created_at from mailbox.messages order by id desc limit 50) m order by id",
        )
        .fetch_all(&db)
    } else {
        sqlx::query_as("select id, agent, text, created_at from mailbox.messages where id > $1 order by id limit 200")
            .bind(q.after_id)
            .fetch_all(&db)
    }
    .instrument(telemetry::db_span("mailbox feed"))
    .await?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_and_message_rules_match_the_old_mailbox() {
        assert_eq!(
            validate("alice-mailbox-ux", "  hi  "),
            Ok(Exchange {
                owner: "alice".into(),
                agent: "alice-mailbox-ux".into(),
                msg: "hi".into()
            })
        );
        for agent in [
            "alice",
            "Alice-x",
            "-x",
            "alice-",
            "alice-a b",
            "alice-ω",
            &format!("alice-{}", "a".repeat(61)),
        ] {
            assert!(validate(agent, "x").is_err(), "{agent}");
        }
        assert!(validate("alice-x", "   ").is_err());
        assert!(
            validate("alice-x", &"ω".repeat(1000)).is_ok(),
            "1000 characters, not bytes"
        );
        assert!(validate("alice-x", &"ω".repeat(1001)).is_err());
    }

    // Transitional mode: three cases of the MCP key owner.
    #[test]
    fn mcp_owner_follows_the_login_and_falls_back_to_the_prefix() {
        // With a binding: only one's own handle.
        assert!(owner_allowed("alice", Some("alice"), false));
        assert!(!owner_allowed("bob", Some("alice"), false));
        assert!(!owner_allowed("bob", Some("alice"), true));
        // Without a binding: the prefix as before, except one taken by someone else's binding.
        assert!(owner_allowed("bob", None, false));
        assert!(!owner_allowed("alice", None, true));
    }

    #[test]
    fn key_is_hashed_and_checked() {
        let id = key_id(&"a".repeat(32)).unwrap();
        assert_eq!(
            id,
            "sha256:3ba3f5f43b92602683c19aee62a20342b084dd5971ddd33808d81a328879a547"
        );
        assert!(key_id(&"a".repeat(31)).is_err());
        assert!(key_id(&"a".repeat(129)).is_err());
        assert!(key_id(&format!("{}+", "a".repeat(32))).is_err());
    }

    #[test]
    fn render_is_plain_lines_or_one_word() {
        let o = Outcome {
            status: "approved".into(),
            messages: vec![
                Delivered {
                    from: "bob-a".into(),
                    text: "line one  \n  line two\nthree".into(),
                },
                Delivered {
                    from: "alice-b".into(),
                    text: "ok".into(),
                },
            ],
        };
        assert_eq!(render(&o), "bob-a: line one line two three\nalice-b: ok\n");
        assert_eq!(
            render(&Outcome {
                status: "approved".into(),
                messages: vec![]
            }),
            ""
        );
        assert_eq!(
            render(&Outcome {
                status: "pending".into(),
                messages: vec![]
            }),
            "pending\n"
        );
    }
}
