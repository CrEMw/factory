//! Write actor and a transaction with it (spec "Actor and trust").
//!
//! Every hub write runs in one transaction whose first command is
//! `SELECT set_config('hub.actor', $1, true), set_config('hub.actor_kind', $2, true)`. Graph triggers
//! reject a write without an actor and an edge confirmation by a non-human; the `ticket_change` journal records actor and kind.
//!
//! The actor kind is derived by the server, not the client. The verified caller name ([`Caller`]) is set by the `callers::require` layer.
//! hub-web claims a human or an agent with the [`ACTOR_HEADER`] and [`ACTOR_KIND_HEADER`] headers: `human` only for
//! a form action with a live page session, `agent` for any MCP call. Other callers may not claim `human` or
//! `agent` (403): their kind is `service:<name>` and the actor is their name. hub-web without headers (a form without a session) writes as
//! `service:hub-web`. In open mode (local run without secrets, caller `unauthenticated`) the headers
//! are trusted the same way as from hub-web: there is nobody to verify.

use anyhow::Context;
use axum::{extract::FromRequestParts, http::StatusCode, http::request::Parts};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::Instrument;

use crate::{AppError, callers::Caller, callers::UNAUTHENTICATED, telemetry};

pub const ACTOR_HEADER: &str = "x-hub-actor";
pub const ACTOR_KIND_HEADER: &str = "x-hub-actor-kind";
/// Fresh sign-in: hub-web sets `x-hub-fresh: 1` with the `human` actor if the form session is no older than
/// 5 minutes (`session.createdAt`). Only hub-web knows the session, so hub trusts the header only together with
/// `human`, which only hub-web claims anyway. Login binding and mailbox key decisions require it.
pub const FRESH_HEADER: &str = "x-hub-fresh";
const HUB_WEB: &str = "hub-web";

/// Who writes: an identifier for the journal and a kind (`human`, `agent`, `seed`, `service:<name>`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Actor {
    pub id: String,
    pub kind: String,
}

impl Actor {
    /// A seed writer; the startup seeds set their actor in SQL, so only the tests build one here.
    #[cfg(test)]
    pub fn seed(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            kind: "seed".to_owned(),
        }
    }

    pub fn is_human(&self) -> bool {
        self.kind == "human"
    }
}

/// Actor identifier: 1..200 bytes without control characters.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control)
}

/// Kind for a service: `hub-ingest` writes as `service:ingest`, `hub-analysis-worker` as `service:analysis`.
fn service_kind(caller: &str) -> String {
    match caller {
        "hub-ingest" => "service:ingest".to_owned(),
        "hub-analysis-worker" => "service:analysis".to_owned(),
        other => format!("service:{other}"),
    }
}

/// Actor kind the hub writes: `human`, `agent`, `seed` or `service:` and a name `[a-z][a-z0-9-]{0,39}`. The CHECK on
/// `ticket_change.actor_kind` (0011_ticket_change.sql) also admits the kind of journal rows written before strict
/// actors (0018); nothing writes it any more.
fn valid_kind(kind: &str) -> bool {
    if matches!(kind, "human" | "agent" | "seed") {
        return true;
    }
    let Some(name) = kind.strip_prefix("service:") else {
        return false;
    };
    let b = name.as_bytes();
    (1..=40).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Actor from the verified caller and the claimed headers.
pub fn resolve(
    caller: &str,
    kind: Option<&str>,
    id: Option<&str>,
) -> Result<Actor, (StatusCode, &'static str)> {
    let kind = kind.map(str::trim).filter(|k| !k.is_empty());
    let id = id.map(str::trim).filter(|i| !i.is_empty());
    let trusted = caller == HUB_WEB || caller == UNAUTHENTICATED;
    match (kind, id) {
        // A caller name that the database would not accept as a kind (the naming rule in callers.rs is wider than the journal CHECK): 403 here,
        // not 500 from the CHECK on write.
        (None, None) => {
            let kind = service_kind(caller);
            if valid_kind(&kind) && valid_id(caller) {
                Ok(Actor {
                    id: caller.to_owned(),
                    kind,
                })
            } else {
                Err((
                    StatusCode::FORBIDDEN,
                    "caller name does not fit an actor kind: service:[a-z][a-z0-9-]{0,39}",
                ))
            }
        }
        (Some(k @ ("human" | "agent")), Some(id)) if trusted => {
            if valid_id(id) {
                Ok(Actor {
                    id: id.to_owned(),
                    kind: k.to_owned(),
                })
            } else {
                Err((
                    StatusCode::BAD_REQUEST,
                    "x-hub-actor: 1 to 200 bytes, no control characters",
                ))
            }
        }
        (Some("human" | "agent"), Some(_)) => Err((
            StatusCode::FORBIDDEN,
            "this caller cannot act as human or agent",
        )),
        (Some(_), Some(_)) => Err((
            StatusCode::FORBIDDEN,
            "x-hub-actor-kind: human or agent, only from hub-web",
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "x-hub-actor and x-hub-actor-kind go together",
        )),
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Actor {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let caller = parts
            .extensions
            .get::<Caller>()
            .map_or(UNAUTHENTICATED, |c| c.0.as_str())
            .to_owned();
        let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
        let actor = resolve(&caller, header(ACTOR_KIND_HEADER), header(ACTOR_HEADER))
            .map_err(|(status, msg)| AppError::Client(status, msg))?;
        tracing::Span::current().record("hub.actor_kind", actor.kind.as_str());
        Ok(actor)
    }
}

/// Whether the human at the form signed in recently: `true` only with `x-hub-fresh: 1` on the `human` actor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fresh(pub bool);

impl<S: Send + Sync> FromRequestParts<S> for Fresh {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let actor = Actor::from_request_parts(parts, state).await?;
        let claimed = parts
            .headers
            .get(FRESH_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "1");
        Ok(Self(actor.is_human() && claimed))
    }
}

/// Fresh sign-in mark in the transaction (`hub.fresh = on`): required by the binding triggers and `mailbox.review_key`.
pub async fn set_fresh(tx: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    sqlx::query("select set_config('hub.fresh', 'on', true)")
        .execute(&mut **tx)
        .instrument(telemetry::db_span("set fresh"))
        .await
        .context("set fresh")?;
    Ok(())
}

/// Write transaction: actor and kind are set by the first command and live until its end.
pub async fn begin(db: &PgPool, actor: &Actor) -> Result<Transaction<'static, Postgres>, AppError> {
    let mut tx = db
        .begin()
        .instrument(telemetry::db_span("begin"))
        .await
        .context("begin")?;
    set_actor(&mut tx, actor).await?;
    Ok(tx)
}

pub async fn set_actor(tx: &mut Transaction<'_, Postgres>, actor: &Actor) -> anyhow::Result<()> {
    sqlx::query("select set_config('hub.actor', $1, true), set_config('hub.actor_kind', $2, true)")
        .bind(&actor.id)
        .bind(&actor.kind)
        .execute(&mut **tx)
        .instrument(telemetry::db_span("set actor"))
        .await
        .context("set actor")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(caller: &str, kind: Option<&str>, id: Option<&str>) -> (String, String) {
        let a = resolve(caller, kind, id).unwrap();
        (a.kind, a.id)
    }

    #[test]
    fn hub_web_claims_human_or_agent_others_are_services() {
        assert_eq!(
            ok("hub-web", Some("human"), Some("user:1")),
            ("human".into(), "user:1".into())
        );
        assert_eq!(
            ok("hub-web", Some("agent"), Some("user:1")),
            ("agent".into(), "user:1".into())
        );
        assert_eq!(
            ok("hub-web", None, None),
            ("service:hub-web".into(), "hub-web".into())
        );
        assert_eq!(
            ok("hub-ingest", None, None),
            ("service:ingest".into(), "hub-ingest".into())
        );
        assert_eq!(
            ok("hub-analysis-worker", None, None),
            ("service:analysis".into(), "hub-analysis-worker".into())
        );
        assert_eq!(
            ok(UNAUTHENTICATED, Some("human"), Some("dev")),
            ("human".into(), "dev".into())
        );
    }

    #[test]
    fn service_kinds_fit_the_journal_check() {
        for caller in [
            "hub-web",
            "hub-ingest",
            "hub-analysis-worker",
            UNAUTHENTICATED,
            "a",
            &"a".repeat(40),
        ] {
            let a = resolve(caller, None, None).unwrap();
            assert!(valid_kind(&a.kind), "{caller}: {}", a.kind);
        }
        for caller in ["bad_name", "Hub", &"a".repeat(41), "1x"] {
            assert_eq!(
                resolve(caller, None, None).unwrap_err().0,
                StatusCode::FORBIDDEN,
                "{caller}"
            );
        }
        for kind in ["human", "agent", "seed", "service:ingest"] {
            assert!(valid_kind(kind), "{kind}");
        }
        for kind in ["", "service:", "service:Ab", "service:a_b", "robot"] {
            assert!(!valid_kind(kind), "{kind}");
        }
    }

    #[test]
    fn human_from_the_loader_is_403_and_bad_headers_are_400() {
        for kind in ["human", "agent"] {
            assert_eq!(
                resolve("hub-ingest", Some(kind), Some("user:1"))
                    .unwrap_err()
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        for kind in ["seed", "service:ingest", "robot"] {
            assert_eq!(
                resolve("hub-web", Some(kind), Some("x")).unwrap_err().0,
                StatusCode::FORBIDDEN,
                "{kind}"
            );
        }
        assert_eq!(
            resolve("hub-web", Some("human"), None).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            resolve("hub-web", None, Some("user:1")).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            resolve("hub-web", Some("human"), Some("a\nb"))
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            resolve("hub-web", Some("human"), Some(&"x".repeat(201)))
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }
}
