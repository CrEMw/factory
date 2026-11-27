//! Installation settings from env (step 2.3 of the factory plan). Both commands read them: `hub migrate` seeds the
//! public.instance row from them (migrations.rs), the `hub` service checks env against it at startup (Config::check_instance). An
//! env error fails both the Job and the service before the first database query.
//!   HUB_ACCOUNT                required, no default: ^[a-z][a-z0-9-]{0,39}$, written to the database once;
//!   HUB_TIMEZONE               required, no default: an IANA zone name (Europe/Berlin, UTC), checked again by the database;
//!   HUB_MAILBOX_PENDING_LIMIT  pending mailbox keys per owner, default 20 (as in 0008);
//!   HUB_MAILBOX_PENDING_TTL    lifetime of a pending key (Postgres interval, "7 days"), empty: no limit;
//!   HUB_DOCUMENT_ROOTS         document roots in repositories, comma separated, default docs/superpowers;
//!   HUB_DOCUMENT_LINK_LABELS   words that introduce ticket links in documents (migration 0027), JSON
//!                              {"describes": [...], "closes": [...], "and": [...]}, default English
//!                              (DEFAULT_DOCUMENT_LINK_LABELS, the column default of public.instance).
//! The account in the service SQL (step 2.4, migration 0025) is read from the database, not from this struct:
//! `account = (select public.instance_account())`. Postgres evaluates the subquery once per query (InitPlan), while
//! a function call in a join condition would be evaluated for every row; a function argument (`plan.flush`) is
//! evaluated once even without a subquery.

use anyhow::{Context, bail, ensure};
use sqlx::PgPool;

/// Installation local time in SQL (step 2.5): the moment `$at` (timestamptz) in the time zone of the public.instance row.
/// The zone comes from an uncorrelated subquery that Postgres evaluates once per query, not per row. Without the
/// instance row the query fails with HUBIN instead of using the server time zone.
macro_rules! local_time {
    ($at:literal) => {
        concat!(
            "(",
            $at,
            " at time zone (select public.instance_timezone()))"
        )
    };
}

/// Local day of the moment `$at` in the installation time zone, like [`local_time`].
macro_rules! local_date {
    ($at:literal) => {
        concat!(
            "(",
            $at,
            " at time zone (select public.instance_timezone()))::date"
        )
    };
}

pub(crate) use {local_date, local_time};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub account: String,
    pub timezone: String,
    pub mailbox_pending_limit: i32,
    pub mailbox_pending_ttl: Option<String>,
    pub document_roots: Vec<String>,
    /// Normalised JSON of the link labels (keys sorted), always set: an unset env seeds the default.
    pub document_link_labels: String,
}

/// Link labels without HUB_DOCUMENT_LINK_LABELS; the same text is the column default in migration 0027 (a test checks it).
pub const DEFAULT_DOCUMENT_LINK_LABELS: &str =
    r#"{"describes":["Ticket","Tickets"],"closes":["Closes"],"and":["and"]}"#;

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Parsing through a variable lookup function: tests pass their own table instead of the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let set = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let account = set("HUB_ACCOUNT").context("HUB_ACCOUNT is not set (no default)")?;
        ensure!(
            valid_account(&account),
            "HUB_ACCOUNT {account:?} must match ^[a-z][a-z0-9-]{{0,39}}$"
        );
        let timezone = set("HUB_TIMEZONE").context("HUB_TIMEZONE is not set (no default)")?;
        ensure!(
            valid_timezone(&timezone),
            "HUB_TIMEZONE {timezone:?} is not an IANA time zone name such as Europe/Berlin or UTC"
        );
        let mailbox_pending_limit = match set("HUB_MAILBOX_PENDING_LIMIT") {
            None => 20,
            Some(v) => match v.parse::<i32>() {
                Ok(n) if n > 0 => n,
                _ => bail!("HUB_MAILBOX_PENDING_LIMIT {v:?} is not a positive integer"),
            },
        };
        let mailbox_pending_ttl = set("HUB_MAILBOX_PENDING_TTL");
        let document_roots = match set("HUB_DOCUMENT_ROOTS") {
            None => vec!["docs/superpowers".to_owned()],
            Some(v) => {
                let roots: Vec<String> = v.split(',').map(|r| r.trim().to_owned()).collect();
                for r in &roots {
                    ensure!(
                        valid_root(r),
                        "HUB_DOCUMENT_ROOTS: {r:?} is not a relative path inside the repository"
                    );
                }
                roots
            }
        };
        let document_link_labels = link_labels(
            set("HUB_DOCUMENT_LINK_LABELS")
                .as_deref()
                .unwrap_or(DEFAULT_DOCUMENT_LINK_LABELS),
        )?;
        Ok(Self {
            account,
            timezone,
            mailbox_pending_limit,
            mailbox_pending_ttl,
            document_roots,
            document_link_labels,
        })
    }

    /// Service start: the instance row was seeded by `hub migrate`, its account equals the one in env, and Postgres
    /// knows the env time zone. A different time zone does not fail the start (the Job changes it) but logs a WARN.
    pub async fn check_instance(&self, db: &PgPool) -> anyhow::Result<()> {
        let row: Option<(String, String)> =
            sqlx::query_as("select account, timezone from public.instance")
                .fetch_optional(db)
                .await
                .context("read public.instance")?;
        let Some((account, timezone)) = row else {
            bail!(
                "public.instance has no row: run `hub migrate` (Job hub-db-migrate) with HUB_ACCOUNT and HUB_TIMEZONE"
            );
        };
        ensure!(
            account == self.account,
            "HUB_ACCOUNT is {:?}, but this database belongs to account {account:?}",
            self.account
        );
        check_timezone(db, &self.timezone).await?;
        if timezone != self.timezone {
            tracing::warn!(
                env = %self.timezone,
                instance = %timezone,
                "HUB_TIMEZONE differs from public.instance: the database keeps its own until the next `hub migrate`"
            );
        }
        Ok(())
    }
}

/// HUB_DOCUMENT_LINK_LABELS: exactly the keys describes, closes and and, each a list of 1 to 20 labels; a label is 1 to 40
/// characters, without backquotes and control characters and without spaces at the ends. The database checks the same
/// (public.doc_link_labels_valid, 0027); here the Job fails with a clear message before connecting. Returns normalised JSON.
fn link_labels(raw: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| anyhow::anyhow!("HUB_DOCUMENT_LINK_LABELS is not JSON: {e}"))?;
    let obj = v
        .as_object()
        .context("HUB_DOCUMENT_LINK_LABELS must be a JSON object")?;
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    ensure!(
        keys == ["and", "closes", "describes"],
        "HUB_DOCUMENT_LINK_LABELS must have exactly the keys describes, closes and and, not {keys:?}"
    );
    for (key, list) in obj {
        let list = list
            .as_array()
            .filter(|l| (1..=20).contains(&l.len()))
            .with_context(|| {
                format!("HUB_DOCUMENT_LINK_LABELS.{key} must be a list of 1 to 20 labels")
            })?;
        for label in list {
            let ok = label.as_str().is_some_and(|l| {
                (1..=40).contains(&l.chars().count())
                    && l.trim() == l
                    && !l.chars().any(|c| c == '`' || c.is_control())
            });
            ensure!(
                ok,
                "HUB_DOCUMENT_LINK_LABELS.{key}: {label} must be a string of 1 to 40 characters without backquotes, \
                 control characters and spaces at the ends"
            );
        }
    }
    Ok(v.to_string())
}

/// The zone is in pg_timezone_names, as in public.seed_instance (0023). `now() at time zone` is not fit for the check:
/// it accepts abbreviations (MSK) and POSIX offsets (UTC+3, which POSIX reads as UTC-3), and those are not IANA names.
pub async fn check_timezone(db: &PgPool, timezone: &str) -> anyhow::Result<()> {
    let known: bool = sqlx::query_scalar(
        "select exists(select 1 from pg_catalog.pg_timezone_names where name = $1)",
    )
    .bind(timezone)
    .fetch_one(db)
    .await
    .context("checking HUB_TIMEZONE against pg_timezone_names")?;
    anyhow::ensure!(
        known,
        "HUB_TIMEZONE {timezone:?} is not a time zone Postgres knows"
    );
    Ok(())
}

fn valid_account(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('a'..='z'))
        && s.len() <= 40
        && c.all(|x| matches!(x, 'a'..='z' | '0'..='9' | '-'))
}

/// IANA name shape: parts separated by `/`, the first starts with a letter, parts contain letters, digits, `_`, `-`, `+`. Whether
/// the zone exists is decided by the database (check_timezone): the image has no zone database of its own.
fn valid_timezone(s: &str) -> bool {
    s.len() <= 64
        && s.split('/').count() <= 3
        && s.split('/').enumerate().all(|(i, part)| {
            let mut c = part.chars();
            let first = c.next();
            (if i == 0 {
                matches!(first, Some(x) if x.is_ascii_alphabetic())
            } else {
                first.is_some()
            }) && part
                .chars()
                .all(|x| x.is_ascii_alphanumeric() || matches!(x, '_' | '-' | '+'))
        })
}

fn valid_root(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.ends_with('/')
        && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_lookup(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    fn err(vars: &[(&str, &str)]) -> String {
        format!("{:#}", parse(vars).unwrap_err())
    }

    const BASE: [(&str, &str); 2] = [("HUB_ACCOUNT", "acme"), ("HUB_TIMEZONE", "Europe/Berlin")];

    /// Step 2.5: local time and day in hub SQL take the time zone from public.instance, with no zone literal in them.
    #[test]
    fn sql_local_time_reads_the_instance_timezone() {
        assert_eq!(
            local_time!("now()"),
            "(now() at time zone (select public.instance_timezone()))"
        );
        assert_eq!(
            local_date!("c.at"),
            "(c.at at time zone (select public.instance_timezone()))::date"
        );
        for sql in [
            crate::plan::LOCAL_NOW,
            crate::plan::TASKS_SQL,
            crate::query::READY,
        ] {
            assert!(sql.contains("(select public.instance_timezone())"), "{sql}");
            assert!(!sql.contains("Europe/"), "{sql}");
        }
        // Both done_on branches: from the journal and from updated_at.
        assert_eq!(
            crate::plan::TASKS_SQL
                .matches("(select public.instance_timezone())")
                .count(),
            2
        );
    }

    #[test]
    fn account_and_timezone_are_required_without_default() {
        assert!(err(&[("HUB_TIMEZONE", "UTC")]).contains("HUB_ACCOUNT is not set"));
        assert!(err(&[("HUB_ACCOUNT", "acme")]).contains("HUB_TIMEZONE is not set"));
        assert!(
            err(&[("HUB_ACCOUNT", " "), ("HUB_TIMEZONE", "UTC")])
                .contains("HUB_ACCOUNT is not set")
        );
        assert!(
            err(&[("HUB_ACCOUNT", "acme"), ("HUB_TIMEZONE", "")])
                .contains("HUB_TIMEZONE is not set")
        );
    }

    #[test]
    fn defaults_match_production() {
        assert_eq!(
            parse(&BASE).unwrap(),
            Config {
                account: "acme".into(),
                timezone: "Europe/Berlin".into(),
                mailbox_pending_limit: 20,
                mailbox_pending_ttl: None,
                document_roots: vec!["docs/superpowers".into()],
                document_link_labels:
                    r#"{"and":["and"],"closes":["Closes"],"describes":["Ticket","Tickets"]}"#.into(),
            }
        );
        let mut vars = BASE.to_vec();
        vars.push(("HUB_MAILBOX_PENDING_TTL", ""));
        assert_eq!(
            parse(&vars).unwrap().mailbox_pending_ttl,
            None,
            "empty TTL is no TTL"
        );
    }

    #[test]
    fn optional_values_are_read() {
        let mut vars = BASE.to_vec();
        vars.extend([
            ("HUB_MAILBOX_PENDING_LIMIT", "5"),
            ("HUB_MAILBOX_PENDING_TTL", "7 days"),
            ("HUB_DOCUMENT_ROOTS", "docs, notes/adr"),
        ]);
        let c = parse(&vars).unwrap();
        assert_eq!(c.mailbox_pending_limit, 5);
        assert_eq!(c.mailbox_pending_ttl.as_deref(), Some("7 days"));
        assert_eq!(c.document_roots, ["docs", "notes/adr"]);
    }

    #[test]
    fn bad_account_is_refused() {
        for bad in [
            "Acme",
            "1acme",
            "acme_hub",
            "ac me",
            "-acme",
            &"a".repeat(41),
        ] {
            let e = err(&[("HUB_ACCOUNT", bad), ("HUB_TIMEZONE", "UTC")]);
            assert!(e.contains("must match"), "{bad:?}: {e}");
        }
        assert!(parse(&[("HUB_ACCOUNT", &"a".repeat(40)), ("HUB_TIMEZONE", "UTC")]).is_ok());
    }

    #[test]
    fn bad_timezone_is_refused() {
        for good in [
            "UTC",
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+3",
        ] {
            assert!(
                parse(&[("HUB_ACCOUNT", "acme"), ("HUB_TIMEZONE", good)]).is_ok(),
                "{good}"
            );
        }
        for bad in [
            "Europe Berlin",
            "/etc/passwd",
            "../x",
            "Europe/",
            "+03:00",
            "Europe//Berlin",
            "3Zone",
        ] {
            let e = err(&[("HUB_ACCOUNT", "acme"), ("HUB_TIMEZONE", bad)]);
            assert!(e.contains("not an IANA time zone"), "{bad:?}: {e}");
        }
    }

    /// The Rust default is the column default of public.instance (0027): an unset env changes nothing in a fresh database.
    #[cfg(chain)]
    #[test]
    fn default_link_labels_are_the_column_default() {
        let migration = include_str!("../migrations/0027_document_link_labels.sql");
        assert!(
            migration.contains(&format!("DEFAULT '{DEFAULT_DOCUMENT_LINK_LABELS}'::jsonb")),
            "0027 has another default"
        );
    }

    /// The same against the baseline (1000), the schema of every new installation, present in every tree.
    #[test]
    fn default_link_labels_are_the_baseline_column_default() {
        let baseline = include_str!("../baseline/1000_baseline.sql");
        let (_, rest) = baseline
            .split_once("document_link_labels jsonb DEFAULT '")
            .expect("baseline has the column document_link_labels");
        let (default, _) = rest.split_once("'::jsonb").unwrap();
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(parse(default), parse(DEFAULT_DOCUMENT_LINK_LABELS));
    }

    #[test]
    fn link_labels_are_read_and_checked() {
        let mut vars = BASE.to_vec();
        let custom = r#"{"describes": ["\u00c9tiquette", "Ticket"], "closes": ["Closes"], "and": ["and", "a b"]}"#;
        vars.push(("HUB_DOCUMENT_LINK_LABELS", custom));
        assert_eq!(
            parse(&vars).unwrap().document_link_labels,
            "{\"and\":[\"and\",\"a b\"],\"closes\":[\"Closes\"],\"describes\":[\"\u{00c9}tiquette\",\"Ticket\"]}"
        );
        for bad in [
            "x",
            "[]",
            "{}",
            r#"{"describes": ["a"], "closes": ["b"]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": ["c"], "or": ["d"]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": []}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": "c"}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": [1]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": [""]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": [" c"]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": ["c`"]}"#,
            r#"{"describes": ["a"], "closes": ["b"], "and": ["c\nd"]}"#,
        ] {
            let mut vars = BASE.to_vec();
            vars.push(("HUB_DOCUMENT_LINK_LABELS", bad));
            assert!(err(&vars).contains("HUB_DOCUMENT_LINK_LABELS"), "{bad}");
        }
        let long = format!(
            r#"{{"describes": ["{}"], "closes": ["b"], "and": ["c"]}}"#,
            "x".repeat(41)
        );
        let mut vars = BASE.to_vec();
        vars.push(("HUB_DOCUMENT_LINK_LABELS", &long));
        assert!(err(&vars).contains("HUB_DOCUMENT_LINK_LABELS"));
    }

    #[test]
    fn bad_limit_and_roots_are_refused() {
        for bad in ["0", "-1", "x", "2.5"] {
            let mut vars = BASE.to_vec();
            vars.push(("HUB_MAILBOX_PENDING_LIMIT", bad));
            assert!(err(&vars).contains("HUB_MAILBOX_PENDING_LIMIT"), "{bad}");
        }
        for bad in ["/abs", "docs,,x", "../up", "docs/", "a/./b"] {
            let mut vars = BASE.to_vec();
            vars.push(("HUB_DOCUMENT_ROOTS", bad));
            assert!(err(&vars).contains("HUB_DOCUMENT_ROOTS"), "{bad}");
        }
    }
}
