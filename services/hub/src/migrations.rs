//! hub-db schema migrations. Only `hub migrate` runs them: in the cluster that is the Job `hub-db-migrate`
//! (an Argo CD hook before hub-web and hub) as the schema owner `app` (CNPG secret `hub-db-app`), locally the AppHost resource
//! `hub-migrate`. The service (`hub` without arguments) runs as role `hub` without DDL and does not run migrations:
//! at startup it checks the `_sqlx_migrations` journal against its embedded migrations and does not start if the database is behind.
//! After the migrations `hub migrate` seeds the public.instance row from env (config.rs, migration 0023).

use std::collections::HashMap;

use anyhow::Context;
use sqlx::{
    PgPool,
    migrate::{Migration, Migrator},
    postgres::PgPoolOptions,
};

use crate::config::Config;

/// Which embedded migrations an installation runs: env HUB_MIGRATIONS (step 2.15 of the factory plan, docs/migrations.md).
/// `chain` (the default, also when unset or empty) is services/hub/migrations; `baseline` is the single baseline
/// services/hub/baseline/1000_baseline.sql. Any other value is refused before connecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Chain,
    Baseline,
}

impl Source {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(std::env::var("HUB_MIGRATIONS").ok().as_deref())
    }

    pub fn parse(v: Option<&str>) -> anyhow::Result<Self> {
        match v.map(str::trim) {
            None | Some("" | "chain") => Ok(Self::Chain),
            Some("baseline") => Ok(Self::Baseline),
            Some(other) => {
                anyhow::bail!("HUB_MIGRATIONS={other:?}: expected `chain` or `baseline`")
            }
        }
    }

    pub fn migrator(self) -> Migrator {
        match self {
            Self::Chain => chain(),
            Self::Baseline => baseline(),
        }
    }
}

/// The migrations HUB_MIGRATIONS selects. ignore_missing: a rollback to an image without a new migration fails neither the
/// Job nor the service, which works with the newer schema (migrations only add); an adopted database keeps the chain
/// versions in its journal, and the baseline migrator ignores them, as the chain migrator ignores version 1000. Images before
/// this change cannot do that: a rollback past 0002 is forward only.
pub fn migrator() -> anyhow::Result<Migrator> {
    Ok(Source::from_env()?.migrator())
}

/// The single baseline services/hub/baseline/1000_baseline.sql: what `hub migrate --adopt` records as applied.
pub fn baseline() -> Migrator {
    let mut m = sqlx::migrate!("./baseline");
    m.set_ignore_missing(true);
    m
}

/// The migration chain services/hub/migrations: the default migrator, and the database that `hub migrate --adopt` adopts.
pub fn chain() -> Migrator {
    let mut m = sqlx::migrate!();
    m.set_ignore_missing(true);
    m
}

/// `hub migrate`: apply the migrations, seed instance and exit. Address: HUBDB_MIGRATE_URI (the app owner in the cluster),
/// else HUBDB_URI (one role for everything locally). The installation env is read before connecting: without HUB_ACCOUNT or
/// HUB_TIMEZONE the Job fails without touching the schema.
///
/// `hub migrate --adopt` (src/adopt.rs, only with HUB_MIGRATIONS=baseline): first record the baseline as applied on a database
/// built by the chain, then the same as `hub migrate`. HUB_MIGRATIONS and HUB_ADOPT_CHECKSUMS are read before connecting as well.
pub async fn run(adopt: bool) -> anyhow::Result<()> {
    let source = Source::from_env()?;
    anyhow::ensure!(
        !adopt || source == Source::Baseline,
        "`hub migrate --adopt` needs HUB_MIGRATIONS=baseline (docs/migrations.md)"
    );
    let journal = if adopt {
        Some(crate::adopt::checksums_from_env()?)
    } else {
        None
    };
    let config = Config::from_env()?;
    let uri = match std::env::var("HUBDB_MIGRATE_URI") {
        Ok(uri) if !uri.is_empty() => uri,
        _ => {
            std::env::var("HUBDB_URI").context("neither HUBDB_MIGRATE_URI nor HUBDB_URI is set")?
        }
    };
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect(&uri)
        .await
        .context("connect to hub database as the migration owner")?;
    match journal {
        Some(journal) => {
            let mut conn = db
                .acquire()
                .await
                .context("acquire a connection for adopt")?;
            crate::adopt::adopt(&mut conn, &journal).await?;
        }
        None if source == Source::Baseline => crate::adopt::refuse_unadopted(&db).await?,
        None => {}
    }
    let m = source.migrator();
    m.run(&db).await.context("run migrations")?;
    seed_instance(&db, &config).await?;
    let latest = latest(&m);
    db.close().await;
    tracing::info!(latest, "hub migrations applied");
    Ok(())
}

/// Seeding public.instance with one call of public.seed_instance (0023): no row, it is written; a row exists, only
/// the time zone, limit, TTL, document roots and link labels (0027) change; a different account is rejected (HUBAC) and nothing is written. A time
/// zone change goes to the log as a WARN with the old and the new value.
pub async fn seed_instance(db: &PgPool, c: &Config) -> anyhow::Result<()> {
    let old: Option<String> =
        sqlx::query_scalar(
            "select public.seed_instance($1, $2, $3, $4::text::interval, $5, $6::text::jsonb)",
        )
            .bind(&c.account)
            .bind(&c.timezone)
            .bind(c.mailbox_pending_limit)
            .bind(c.mailbox_pending_ttl.as_deref())
            .bind(&c.document_roots)
            .bind(&c.document_link_labels)
            .fetch_one(db)
            .await
            .map_err(|e| {
                let code = e
                    .as_database_error()
                    .and_then(|d| d.code())
                    .map(|c| c.into_owned());
                let why = match code.as_deref() {
                    Some("HUBAC") => {
                        "HUB_ACCOUNT differs from the account written in public.instance"
                    }
                    Some("22023") => "HUB_TIMEZONE is not a time zone Postgres knows",
                    Some("22007" | "22008" | "22P02") => {
                        "HUB_MAILBOX_PENDING_TTL is not a Postgres interval"
                    }
                    Some("23514") => {
                        "an instance value (limit, TTL, HUB_DOCUMENT_LINK_LABELS) breaks a check of public.instance"
                    }
                    _ => "seed public.instance",
                };
                anyhow::Error::new(e).context(why)
            })?;
    match old {
        None => tracing::info!(account = %c.account, timezone = %c.timezone, "instance created"),
        Some(old) if old != c.timezone => tracing::warn!(
            account = %c.account,
            old = %old,
            new = %c.timezone,
            "instance timezone changed"
        ),
        Some(_) => tracing::info!(account = %c.account, "instance is up to date"),
    }
    Ok(())
}

/// Highest number of the embedded migrations (for the log).
fn latest(m: &Migrator) -> i64 {
    applicable(m).map(|x| x.version).max().unwrap_or(0)
}

fn applicable(m: &Migrator) -> impl Iterator<Item = &Migration> {
    m.iter().filter(|x| !x.migration_type.is_down_migration())
}

/// Schema check at service startup: every embedded migration is in `_sqlx_migrations`, applied successfully and with the same
/// checksum. Extra versions in the database (database newer than the image, rollback) are fine, as with `set_ignore_missing`.
/// Role hub only needs SELECT on the journal (0021_db_roles.sql).
pub async fn verify(db: &PgPool, m: &Migrator) -> anyhow::Result<()> {
    let rows: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("select version, success, checksum from public._sqlx_migrations")
            .fetch_all(db)
            .await
            .context("read _sqlx_migrations (no table: run `hub migrate` first)")?;
    let applied: HashMap<i64, (bool, Vec<u8>)> = rows
        .into_iter()
        .map(|(v, ok, sum)| (v, (ok, sum)))
        .collect();
    let (mut missing, mut failed, mut changed) = (Vec::new(), Vec::new(), Vec::new());
    for x in applicable(m) {
        match applied.get(&x.version) {
            None => missing.push(x.version),
            Some((false, _)) => failed.push(x.version),
            Some((true, sum)) if sum.as_slice() != &*x.checksum => changed.push(x.version),
            Some(_) => {}
        }
    }
    anyhow::ensure!(
        missing.is_empty() && failed.is_empty() && changed.is_empty(),
        "hub database schema does not match this image: missing {missing:?}, failed {failed:?}, \
         checksum changed {changed:?}; migrations run in `hub migrate` (Job hub-db-migrate), not in the service"
    );
    tracing::info!(latest = latest(m), "hub database schema is up to date");
    Ok(())
}

#[cfg(test)]
mod source_tests {
    use super::Source;

    #[test]
    fn hub_migrations_selects_the_source_and_refuses_anything_else() {
        for v in [None, Some(""), Some("chain"), Some(" chain ")] {
            assert_eq!(Source::parse(v).unwrap(), Source::Chain, "{v:?}");
        }
        assert_eq!(Source::parse(Some("baseline")).unwrap(), Source::Baseline);
        for v in ["Baseline", "base", "1000", "chain,baseline"] {
            assert!(Source::parse(Some(v)).is_err(), "{v:?}");
        }
        assert!(Source::Chain.migrator().iter().all(|x| x.version < 1000));
        assert_eq!(
            Source::Baseline
                .migrator()
                .iter()
                .map(|x| x.version)
                .collect::<Vec<_>>(),
            vec![1000]
        );
    }
}
