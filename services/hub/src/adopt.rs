//! `hub migrate --adopt` (step 2.12c of the factory plan, docs/migrations.md): a database built by the migration chain
//! services/hub/migrations gets the baseline services/hub/baseline/1000_baseline.sql recorded as applied, without running it.
//! Always built; `hub migrate --adopt` runs only with HUB_MIGRATIONS=baseline (step 2.15 of the factory plan).
//!
//! One transaction under the advisory lock that sqlx `Migrator::run` takes (`Migrate::lock` of the same connection: the id is
//! derived from the database name, so a parallel `hub migrate` waits for adopt and the other way round):
//! - version 1000 is in the journal: nothing to do;
//! - else the journal versions below 1000 must be exactly the list of HUB_ADOPT_CHECKSUMS, every one applied successfully and
//!   with the listed checksum, and no version above 1000 may exist;
//! - the schema catalog (services/hub/pg-schema.sql) must equal the catalog of the baseline (services/hub/baseline/catalog.txt, written
//!   by `sh tools/test-pg-schema.sh --write-catalog`) byte for byte;
//! - then the journal row (1000, 'baseline', true, <sqlx checksum of the baseline>, 0) is inserted in the same transaction.
//!
//! Any difference refuses adopt: the transaction rolls back, the report of differences goes to stderr and `hub` exits with 1.
//!
//! The expected catalog is embedded at build time rather than computed at adopt time: computing it needs the baseline applied
//! to another database (CREATEDB, which the migration role has not) or next to the live schema (the same names), and an embedded
//! file is reviewed in git and checked against a real Postgres by tools/test-pg-schema.sh. Deparsed texts (views, defaults,
//! constraints) are stable within a Postgres major version; on another major the comparison may fail, and then adopt refuses
//! rather than guesses (the report names both versions).

use std::collections::BTreeMap;

use anyhow::Context;
use sqlx::{PgConnection, PgPool, migrate::Migrate};

use crate::migrations::baseline;

/// The version of the baseline in the sqlx journal.
pub const BASELINE_VERSION: i64 = 1000;

/// The catalog query shared with tools/test-pg-schema.sh and the clone rehearsal.
const CATALOG_SQL: &str = include_str!("../pg-schema.sql");

/// The catalog of the baseline on an empty database: one line per object property, sorted by bytes.
const EXPECTED_CATALOG: &str = include_str!("../baseline/catalog.txt");

/// The Postgres major version that wrote services/hub/baseline/catalog.txt (the image of tools/test-graph-pg.sh).
const CATALOG_PG_MAJOR: i32 = 18;

/// Report lines at most, and characters per line, so that a broken database does not flood the Job log.
const REPORT_LINES: usize = 200;
const REPORT_WIDTH: usize = 400;

/// Expected journal of the chain: version to sqlx checksum (SHA-384 of the migration file).
pub type Journal = BTreeMap<i64, Vec<u8>>;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Version 1000 was already in the journal; nothing changed.
    AlreadyAdopted,
    /// The baseline row was inserted.
    Adopted,
}

/// HUB_ADOPT_CHECKSUMS: one line per chain migration, `<version> <sha384 hex>`, as
/// `sha384sum services/hub/migrations/NNNN_*.sql` gives it (docs/migrations.md). Blank lines and lines starting with `#` are
/// skipped. Read before connecting, like the rest of the Job env.
pub fn checksums_from_env() -> anyhow::Result<Journal> {
    let text = std::env::var("HUB_ADOPT_CHECKSUMS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .context("`hub migrate --adopt` needs HUB_ADOPT_CHECKSUMS: the expected journal of the migration chain (docs/migrations.md)")?;
    parse_checksums(&text).context("HUB_ADOPT_CHECKSUMS")
}

pub fn parse_checksums(text: &str) -> anyhow::Result<Journal> {
    let mut journal = Journal::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [version, sum] = fields.as_slice() else {
            anyhow::bail!(
                "line {}: expected `<version> <sha384 hex>`, got {line:?}",
                n + 1
            );
        };
        let version: i64 = version
            .parse()
            .with_context(|| format!("line {}: version {version:?}", n + 1))?;
        anyhow::ensure!(
            (1..BASELINE_VERSION).contains(&version),
            "line {}: version {version} is outside 1..{BASELINE_VERSION}",
            n + 1
        );
        let sum = decode_hex(sum).with_context(|| format!("line {}: checksum", n + 1))?;
        anyhow::ensure!(
            sum.len() == 48,
            "line {}: checksum has {} bytes, SHA-384 has 48",
            n + 1,
            sum.len()
        );
        anyhow::ensure!(
            journal.insert(version, sum).is_none(),
            "line {}: version {version} is listed twice",
            n + 1
        );
    }
    anyhow::ensure!(!journal.is_empty(), "no versions listed");
    Ok(journal)
}

fn decode_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(s.len().is_multiple_of(2), "odd number of hex digits");
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .context("not a hex digit")
        })
        .collect()
}

/// The HUB_ADOPT_CHECKSUMS text of a migrator (tests build the expected journal from the embedded chain).
#[cfg(test)]
pub fn checksums_of(m: &sqlx::migrate::Migrator) -> String {
    m.iter()
        .filter(|x| !x.migration_type.is_down_migration())
        .map(|x| {
            let hex: String = x.checksum.iter().map(|b| format!("{b:02x}")).collect();
            format!("{} {hex}\n", x.version)
        })
        .collect()
}

/// Adopt the baseline on `conn` (see the module comment). `Err` is a refusal or a database error; the transaction is rolled back
/// in both cases.
pub async fn adopt(conn: &mut PgConnection, journal: &Journal) -> anyhow::Result<Outcome> {
    conn.lock().await.context("take the sqlx migration lock")?;
    let outcome = adopt_locked(conn, journal).await;
    let unlocked = conn.unlock().await;
    let outcome = outcome?;
    unlocked.context("release the sqlx migration lock")?;
    Ok(outcome)
}

async fn adopt_locked(conn: &mut PgConnection, journal: &Journal) -> anyhow::Result<Outcome> {
    let m = baseline();
    let baseline = m
        .iter()
        .find(|x| x.version == BASELINE_VERSION)
        .context("the embedded migrator has no baseline version 1000")?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .context("begin the adopt transaction")?;
    let (journal_exists,): (bool,) =
        sqlx::query_as("select to_regclass('public._sqlx_migrations') is not null")
            .fetch_one(&mut *tx)
            .await?;
    anyhow::ensure!(
        journal_exists,
        "adopt refused: no public._sqlx_migrations, the database was not built by the migration chain \
         (an empty database gets the baseline with plain `hub migrate`)"
    );
    let rows: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
        "select version, success, checksum from public._sqlx_migrations order by version",
    )
    .fetch_all(&mut *tx)
    .await
    .context("read public._sqlx_migrations")?;
    if rows.iter().any(|(v, _, _)| *v == BASELINE_VERSION) {
        tracing::info!("baseline already adopted: version 1000 is in the journal");
        return Ok(Outcome::AlreadyAdopted);
    }

    let mut report = journal_differences(&rows, journal);
    let diff = catalog_diff(&mut tx).await?;
    if !diff.is_empty() {
        let (major,): (i32,) =
            sqlx::query_as("select current_setting('server_version_num')::int / 10000")
                .fetch_one(&mut *tx)
                .await?;
        let mut head = format!(
            "catalog: {} lines differ from the baseline (- expected, + this database)",
            diff.len()
        );
        if major != CATALOG_PG_MAJOR {
            head.push_str(&format!(
                "; the expected catalog comes from Postgres {CATALOG_PG_MAJOR}, this server is {major}"
            ));
        }
        report.push(head);
        report.extend(diff.iter().take(REPORT_LINES).map(|l| clip(l)));
        if diff.len() > REPORT_LINES {
            report.push(format!("... {} more", diff.len() - REPORT_LINES));
        }
    }
    if !report.is_empty() {
        tx.rollback().await?;
        eprintln!(
            "hub migrate --adopt refused, nothing written:\n{}",
            report.join("\n")
        );
        anyhow::bail!(
            "adopt refused: {}",
            report
                .iter()
                .filter(|l| !l.starts_with(['-', '+', '.']))
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    sqlx::query(
        "insert into public._sqlx_migrations (version, description, success, checksum, execution_time) \
         values ($1, $2, true, $3, 0)",
    )
    .bind(BASELINE_VERSION)
    .bind(&*baseline.description)
    .bind(&*baseline.checksum)
    .execute(&mut *tx)
    .await
    .context("record the baseline in public._sqlx_migrations")?;
    tx.commit().await.context("commit adopt")?;
    tracing::info!(
        versions = journal.len(),
        "baseline adopted: version 1000 recorded"
    );
    Ok(Outcome::Adopted)
}

/// Differences between the journal rows and the expected list, one report line per kind.
fn journal_differences(rows: &[(i64, bool, Vec<u8>)], journal: &Journal) -> Vec<String> {
    let (mut extra, mut failed, mut changed, mut above) = (vec![], vec![], vec![], vec![]);
    for (v, ok, sum) in rows {
        if *v > BASELINE_VERSION {
            above.push(*v);
            continue;
        }
        match journal.get(v) {
            None => extra.push(*v),
            Some(want) => {
                if !ok {
                    failed.push(*v);
                }
                if want != sum {
                    changed.push(*v);
                }
            }
        }
    }
    let missing: Vec<i64> = journal
        .keys()
        .filter(|v| !rows.iter().any(|(r, _, _)| r == *v))
        .copied()
        .collect();
    let mut report = vec![];
    for (what, list) in [
        ("versions not in HUB_ADOPT_CHECKSUMS", extra),
        ("versions missing from the journal", missing),
        ("versions with success = false", failed),
        ("versions with a different checksum", changed),
        ("versions above 1000 without 1000", above),
    ] {
        if !list.is_empty() {
            report.push(format!("journal: {what} {list:?}"));
        }
    }
    report
}

/// The catalog of the database on `conn` (services/hub/pg-schema.sql) against the baseline: `- line` expected but absent, `+ line`
/// present but not expected, in byte order. Empty means equal.
pub async fn catalog_diff(conn: &mut PgConnection) -> anyhow::Result<Vec<String>> {
    sqlx::raw_sql(CATALOG_SQL)
        .execute(&mut *conn)
        .await
        .context("create pg_temp.schema_catalog (services/hub/pg-schema.sql)")?;
    let mut have: Vec<String> = sqlx::query_scalar(
        r#"select line from pg_temp.schema_catalog() as line order by line collate "C""#,
    )
    .fetch_all(&mut *conn)
    .await
    .context("read the schema catalog")?;
    // Byte order: Rust compares str by bytes, as COLLATE "C" does; sorting again keeps the merge below correct.
    have.sort();
    let mut want: Vec<&str> = EXPECTED_CATALOG.lines().collect();
    want.sort();
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < want.len() || j < have.len() {
        match (want.get(i), have.get(j)) {
            (Some(w), Some(h)) if *w == h.as_str() => {
                i += 1;
                j += 1;
            }
            (Some(w), Some(h)) if *w < h.as_str() => {
                out.push(format!("- {w}"));
                i += 1;
            }
            (Some(w), None) => {
                out.push(format!("- {w}"));
                i += 1;
            }
            (_, Some(h)) => {
                out.push(format!("+ {h}"));
                j += 1;
            }
            (None, None) => break,
        }
    }
    Ok(out)
}

fn clip(line: &str) -> String {
    match line.char_indices().nth(REPORT_WIDTH) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// Plain `hub migrate` of the baseline build on a database built by the chain would run the baseline over the existing schema;
/// refuse with the way out instead.
pub async fn refuse_unadopted(db: &PgPool) -> anyhow::Result<()> {
    let (exists,): (bool,) =
        sqlx::query_as("select to_regclass('public._sqlx_migrations') is not null")
            .fetch_one(db)
            .await?;
    if !exists {
        // An empty database: the baseline applies the normal way.
        return Ok(());
    }
    let (chain, adopted): (bool, bool) = sqlx::query_as(
        "select coalesce(bool_or(version < 1000), false), coalesce(bool_or(version = 1000), false) \
         from public._sqlx_migrations",
    )
    .fetch_one(db)
    .await
    .context("read public._sqlx_migrations")?;
    anyhow::ensure!(
        !chain || adopted,
        "the database was built by the migration chain and has no baseline: run `hub migrate --adopt` once (docs/migrations.md)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b";

    #[test]
    fn checksums_parse_and_refuse_bad_lines() {
        let j = parse_checksums(&format!("# chain\n\n1 {SUM}\n  17   {SUM}  \n")).unwrap();
        assert_eq!(j.keys().copied().collect::<Vec<_>>(), vec![1, 17]);
        assert_eq!(j[&1].len(), 48);
        for bad in [
            format!("1 {SUM}\n1 {SUM}"),
            format!("1000 {SUM}"),
            format!("0 {SUM}"),
            format!("x {SUM}"),
            "1 abcd".to_owned(),
            format!("1 {}zz", &SUM[2..]),
            format!("1 {SUM} extra"),
            "# nothing".to_owned(),
        ] {
            assert!(parse_checksums(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn embedded_chain_checksums_round_trip() {
        let chain = crate::migrations::chain();
        let j = parse_checksums(&checksums_of(&chain)).unwrap();
        assert!(j.contains_key(&1) && !j.contains_key(&15) && !j.contains_key(&16));
        for x in chain.iter() {
            assert_eq!(j[&x.version].as_slice(), &*x.checksum);
        }
    }

    #[test]
    fn baseline_is_version_1000_and_the_catalog_is_embedded() {
        let m = baseline();
        let v: Vec<i64> = m.iter().map(|x| x.version).collect();
        assert_eq!(v, vec![BASELINE_VERSION]);
        assert_eq!(&*m.iter().next().unwrap().description, "baseline");
        assert!(EXPECTED_CATALOG.lines().count() > 1000);
        assert!(CATALOG_SQL.contains("pg_temp.schema_catalog()"));
    }

    #[test]
    fn journal_differences_name_each_kind() {
        let j = parse_checksums(&format!("1 {SUM}\n2 {SUM}\n3 {SUM}\n")).unwrap();
        let s = j[&1].clone();
        let rows = vec![
            (1, true, s.clone()),
            (3, false, vec![0]),
            (999, true, s.clone()),
            (1001, true, s),
        ];
        let r = journal_differences(&rows, &j).join("\n");
        assert!(r.contains("not in HUB_ADOPT_CHECKSUMS [999]"), "{r}");
        assert!(r.contains("missing from the journal [2]"), "{r}");
        assert!(r.contains("success = false [3]"), "{r}");
        assert!(r.contains("different checksum [3]"), "{r}");
        assert!(r.contains("above 1000 without 1000 [1001]"), "{r}");
        let ok = vec![
            (1, true, j[&1].clone()),
            (2, true, j[&2].clone()),
            (3, true, j[&3].clone()),
        ];
        assert!(journal_differences(&ok, &j).is_empty());
    }
}
