//! Binding a hub sign-in to a graph person (stage 1 of the shared agent memory plan; spec
//! "Human identity").
//!
//! GET  /api/people/logins[?user_id=]       live bindings (with `user_id`: zero or one)
//! POST /api/people/logins                  `{handle}` -> 201: "this is me", bind one's own account
//! POST /api/people/logins/{handle}/retire  retire a person's binding (a wrong one or someone else's)
//!
//! Both writes only from a human (`human`, hub-web form) with a fresh sign-in (`x-hub-fresh: 1`, signed in no more than 5
//! minutes ago): otherwise 403 `requires_human` or `fresh_session_required`. The account comes from the actor `user:<id>`, not from
//! the body. Invariants in the database (0022_person_login.sql): one live binding per account (409 `already_bound`) and per
//! person (409 `handle_taken`), binding only one's own account, not to an agent (409 `agent_person`) and not to a retired one
//! (409 `retired`); unknown person 404 `unknown_person`, retiring without a live binding 404 `no_login`.
//! The binding makes the handle the owner of the account's MCP key in the mailbox (a database trigger).

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use tracing::Instrument;

use crate::{
    AppError, bad,
    people::valid_handle,
    telemetry,
    tx::{self, Actor, Fresh},
};

fn err(status: StatusCode, error: &str) -> AppError {
    AppError::Json(status, json!({ "error": error }))
}

#[derive(Serialize, sqlx::FromRow, Debug)]
pub struct Login {
    pub user_id: String,
    pub handle: String,
    pub actor: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_by: Option<String>,
}

const LOGIN_COLUMNS: &str = "user_id, handle, actor, created_at, retired_at, retired_by";

/// Better Auth account from the actor `user:<id>`.
pub fn login_user(actor: &Actor) -> Option<&str> {
    actor.id.strip_prefix("user:").filter(|u| !u.is_empty())
}

/// A human with a fresh sign-in: otherwise 403 before the database (the database checks the same).
pub fn require_fresh_human(actor: &Actor, fresh: bool) -> Result<(), AppError> {
    if !actor.is_human() {
        return Err(err(StatusCode::FORBIDDEN, "requires_human"));
    }
    if !fresh {
        return Err(err(StatusCode::FORBIDDEN, "fresh_session_required"));
    }
    Ok(())
}

/// Binding and mailbox key decision errors (SQLSTATE from 0022_person_login.sql) into API responses.
pub fn identity_error(e: sqlx::Error, what: &'static str) -> AppError {
    if let sqlx::Error::Database(d) = &e {
        let mapped = match (d.code().as_deref(), d.constraint()) {
            (Some("GRHUM"), _) => Some((StatusCode::FORBIDDEN, "requires_human")),
            (Some("GRFSH"), _) => Some((StatusCode::FORBIDDEN, "fresh_session_required")),
            (Some("GRLGN"), _) => Some((StatusCode::FORBIDDEN, "not_your_login")),
            (Some("GRNLG"), _) => Some((StatusCode::FORBIDDEN, "no_login")),
            (Some("GROWN"), _) => Some((StatusCode::FORBIDDEN, "not_key_owner")),
            (Some("GRRET"), _) => Some((StatusCode::CONFLICT, "retired")),
            (Some("GRFRB"), _) => Some((StatusCode::CONFLICT, "forbidden")),
            (Some("P0002"), _) => Some((StatusCode::NOT_FOUND, "no_such_key")),
            (Some("23505"), Some("person_login_user")) => {
                Some((StatusCode::CONFLICT, "already_bound"))
            }
            (Some("23505"), Some("person_login_handle")) => {
                Some((StatusCode::CONFLICT, "handle_taken"))
            }
            (Some("23503"), _) => Some((StatusCode::NOT_FOUND, "unknown_person")),
            _ => None,
        };
        if let Some((status, error)) = mapped {
            return err(status, error);
        }
    }
    anyhow::Error::new(e).context(what).into()
}

#[derive(Deserialize)]
pub struct LoginQuery {
    user_id: Option<String>,
}

pub async fn list_logins(
    State(db): State<PgPool>,
    Query(q): Query<LoginQuery>,
) -> Result<Json<Vec<Login>>, AppError> {
    let user = q
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    if user.is_some_and(|u| u.len() > 200) {
        return Err(bad("user_id: up to 200 bytes"));
    }
    let rows = sqlx::query_as(&format!(
        "select {LOGIN_COLUMNS} from graph.person_login \
         where account = (select public.instance_account()) and retired_at is null and ($1::text is null or user_id = $1) \
         order by handle"
    ))
    .bind(user)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select logins"))
    .await
    .context("select logins")?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindLogin {
    handle: String,
}

pub async fn bind_login(
    State(db): State<PgPool>,
    actor: Actor,
    Fresh(fresh): Fresh,
    Json(b): Json<BindLogin>,
) -> Result<(StatusCode, Json<Login>), AppError> {
    let handle = b.handle.trim().to_lowercase();
    if !valid_handle(&handle) {
        return Err(bad("handle: ^[a-z][a-z0-9-]{0,39}$, not none"));
    }
    require_fresh_human(&actor, fresh)?;
    let Some(user) = login_user(&actor) else {
        return Err(err(StatusCode::FORBIDDEN, "not_your_login"));
    };
    let mut tx = tx::begin(&db, &actor).await?;
    tx::set_fresh(&mut tx).await?;
    let person: Option<(bool, bool)> = sqlx::query_as(
        "select p.is_agent, n.retired_at is not null from graph.person p join graph.node n on n.id = p.node_id \
         where p.account = (select public.instance_account()) and p.handle = $1",
    )
    .bind(&handle)
    .fetch_optional(&mut *tx)
    .instrument(telemetry::db_span("select login person"))
    .await
    .context("select login person")?;
    match person {
        None => return Err(err(StatusCode::NOT_FOUND, "unknown_person")),
        Some((true, _)) => return Err(err(StatusCode::CONFLICT, "agent_person")),
        Some((_, true)) => return Err(err(StatusCode::CONFLICT, "retired")),
        Some(_) => {}
    }
    let login: Login = sqlx::query_as(&format!(
        "insert into graph.person_login (user_id, handle) values ($1, $2) returning {LOGIN_COLUMNS}"
    ))
    .bind(user)
    .bind(&handle)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("insert login"))
    .await
    .map_err(|e| identity_error(e, "insert login"))?;
    tx.commit().await.context("commit login")?;
    tracing::info!(handle = login.handle.as_str(), "login bound to a person");
    Ok((StatusCode::CREATED, Json(login)))
}

pub async fn retire_login(
    State(db): State<PgPool>,
    actor: Actor,
    Fresh(fresh): Fresh,
    Path(handle): Path<String>,
) -> Result<Json<Login>, AppError> {
    let handle = handle.trim().to_lowercase();
    if !valid_handle(&handle) {
        return Err(bad("handle: ^[a-z][a-z0-9-]{0,39}$, not none"));
    }
    require_fresh_human(&actor, fresh)?;
    let mut tx = tx::begin(&db, &actor).await?;
    tx::set_fresh(&mut tx).await?;
    // retired_by is set by a trigger (the transaction actor); here only a non-empty value for the CHECK.
    let login: Option<Login> = sqlx::query_as(&format!(
        "update graph.person_login set retired_at = now(), retired_by = '-' \
         where account = (select public.instance_account()) and handle = $1 and retired_at is null returning {LOGIN_COLUMNS}"
    ))
    .bind(&handle)
    .fetch_optional(&mut *tx)
    .instrument(telemetry::db_span("retire login"))
    .await
    .map_err(|e| identity_error(e, "retire login"))?;
    let Some(login) = login else {
        return Err(err(StatusCode::NOT_FOUND, "no_login"));
    };
    tx.commit().await.context("commit login retire")?;
    tracing::info!(handle = login.handle.as_str(), "login retired");
    Ok(Json(login))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(kind: &str, id: &str) -> Actor {
        Actor {
            id: id.into(),
            kind: kind.into(),
        }
    }

    fn status_of(r: Result<(), AppError>) -> Option<String> {
        match r {
            Ok(()) => None,
            Err(AppError::Json(s, body)) => Some(format!("{} {}", s.as_u16(), body["error"])),
            Err(_) => Some("other".into()),
        }
    }

    #[test]
    fn only_a_human_with_a_fresh_login_binds() {
        assert_eq!(
            status_of(require_fresh_human(&actor("human", "user:1"), true)),
            None
        );
        assert_eq!(
            status_of(require_fresh_human(&actor("human", "user:1"), false)).as_deref(),
            Some(r#"403 "fresh_session_required""#)
        );
        assert_eq!(
            status_of(require_fresh_human(&actor("agent", "user:1"), true)).as_deref(),
            Some(r#"403 "requires_human""#)
        );
        assert_eq!(
            status_of(require_fresh_human(
                &actor("service:hub-web", "hub-web"),
                true
            ))
            .as_deref(),
            Some(r#"403 "requires_human""#)
        );
    }

    #[test]
    fn the_user_comes_from_the_actor() {
        assert_eq!(login_user(&actor("human", "user:abc")), Some("abc"));
        assert_eq!(login_user(&actor("human", "user:")), None);
        assert_eq!(login_user(&actor("human", "dev")), None);
    }
}
