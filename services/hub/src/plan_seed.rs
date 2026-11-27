//! Seed of initial plan scopes `scopes.toml` from the `HUB_SEED_DIR` directory (spec "Scopes"; seed.rs).
//! [`seed`] calls `plan.import_scopes` (0019_plan_import_scopes.sql) at startup after the link seeds: the umbrella ticket,
//! the children's `part_of` edges and a `plan.scope` row with actor `seed:scopes`. The seed does not create the database. No file: no import.
//!
//! A parse error fails the start (the installation seeds and the example are checked by the test below). A database error does not fail the start, as with
//! the link seed: hub writes a report to the log (ERROR) and to the `hub.seed.scopes.runs{status=ok|error}` metric.

use std::collections::BTreeSet;

use anyhow::Context;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::Instrument;

use crate::{graph_view::valid_code, telemetry, valid_priority, valid_project};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopesSeed {
    #[serde(default)]
    scope: Vec<SeedScope>,
}

/// A seed row; field names as in `plan.import_scopes`.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeedScope {
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green_a_pct: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_a_pct: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_slope_pct: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_defaulted_pct: Option<i32>,
}

impl SeedScope {
    /// The same bounds as the CHECKs of the `tickets` and `plan.scope` tables: the error shows up in a test, not at startup in the cluster.
    fn check(&self) -> Result<(), String> {
        if !valid_code(&self.root) {
            return Err("root: a lowercase ticket code".into());
        }
        if self.project.is_some() != self.summary.is_some() {
            return Err("project and summary go together (the umbrella ticket)".into());
        }
        if self.project.as_deref().is_some_and(|p| !valid_project(p)) {
            return Err("project: lowercase latin letters, digits and dashes, up to 40".into());
        }
        if self.summary.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err("summary is empty".into());
        }
        if self.priority.as_deref().is_some_and(|p| !valid_priority(p)) {
            return Err("priority: p0, p1, p2 or p3".into());
        }
        if self.tags.len() > 30
            || self
                .tags
                .iter()
                .any(|t| t.trim().is_empty() || t.len() > 60)
        {
            return Err("tags: up to 30, each 1 to 60 bytes".into());
        }
        let mut seen = BTreeSet::new();
        for c in &self.children {
            if !valid_code(c) || *c == self.root || !seen.insert(c) {
                return Err(format!(
                    "children: {c} is not a distinct ticket code other than root"
                ));
            }
        }
        if !matches!(self.mode.as_deref(), None | Some("forward" | "backward")) {
            return Err("mode: forward or backward".into());
        }
        if self.mode.as_deref() == Some("backward") && self.due_date.is_none() {
            return Err("mode backward needs due_date".into());
        }
        if !matches!(self.buffer_method.as_deref(), None | Some("cnp" | "ssq")) {
            return Err("buffer_method: cnp or ssq".into());
        }
        let green = self.green_a_pct.unwrap_or(10);
        let red = self.red_a_pct.unwrap_or(30);
        if !(0 <= green && green <= red && red <= 1000) {
            return Err("thresholds: 0 <= green_a_pct <= red_a_pct <= 1000".into());
        }
        if !(0..=1000).contains(&self.zone_slope_pct.unwrap_or(60)) {
            return Err("zone_slope_pct: 0..1000".into());
        }
        if !(0..=100).contains(&self.max_defaulted_pct.unwrap_or(20)) {
            return Err("max_defaulted_pct: 0..100".into());
        }
        Ok(())
    }
}

/// Parsing and checking the scope seed: roots without duplicates, a child in only one scope (part_of is single).
pub fn parse_scopes(text: &str) -> anyhow::Result<Vec<SeedScope>> {
    let seed: ScopesSeed = toml::from_str(text).context("scopes.toml")?;
    let mut roots = BTreeSet::new();
    let mut children = BTreeSet::new();
    for s in &seed.scope {
        s.check()
            .map_err(|e| anyhow::anyhow!("scopes.toml {}: {e}", s.root))?;
        anyhow::ensure!(
            roots.insert(s.root.clone()),
            "scopes.toml: root {} twice",
            s.root
        );
        for c in &s.children {
            anyhow::ensure!(
                children.insert(c.clone()),
                "scopes.toml: {c} is a child of two scopes"
            );
        }
    }
    Ok(seed.scope)
}

/// Import of the scope seed at startup (after the link seeds: children are already linked with blockers).
pub async fn seed(db: &PgPool, text: Option<&str>) -> anyhow::Result<()> {
    let Some(text) = text else {
        return Ok(());
    };
    let scopes = serde_json::to_value(parse_scopes(text)?)?;
    let runs = opentelemetry::global::meter("hub")
        .u64_counter("hub.seed.scopes.runs")
        .build();
    let status = match import(db, &scopes).await {
        Err(e) => {
            tracing::error!(
                error = format!("{e:#}"),
                "seed scopes failed, nothing written"
            );
            "error"
        }
        Ok(report) => {
            let skipped = report["unknown"].as_array().is_some_and(|u| !u.is_empty())
                || report["links"].as_object().is_some_and(|l| {
                    l.values().any(|r| {
                        r["status"] != "ok"
                            || r["unknown"].as_array().is_some_and(|u| !u.is_empty())
                    })
                });
            if skipped {
                tracing::warn!(report = %report, "seed scopes: some rows skipped");
            } else {
                tracing::info!(report = %report, "seed scopes");
            }
            "ok"
        }
    };
    runs.add(1, &[opentelemetry::KeyValue::new("status", status)]);
    Ok(())
}

async fn import(db: &PgPool, scopes: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let mut tx = db.begin().await.context("begin seed scopes")?;
    let (report,): (serde_json::Value,) = sqlx::query_as("select plan.import_scopes($1::jsonb)")
        .bind(scopes)
        .fetch_one(&mut *tx)
        .instrument(telemetry::db_span("import scopes"))
        .await
        .context("import scopes")?;
    tx.commit().await.context("commit seed scopes")?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_file_parses_and_names_the_scope() {
        for dir in crate::seed::tree_dirs() {
            let seeds = crate::seed::Seeds::read(&dir).unwrap();
            assert!(
                !parse_scopes(seeds.scopes.as_deref().unwrap())
                    .unwrap()
                    .is_empty(),
                "{}",
                dir.display()
            );
        }
        let example = crate::seed::Seeds::read(&crate::seed::example_dir()).unwrap();
        let scopes = parse_scopes(example.scopes.as_deref().unwrap()).unwrap();
        assert_eq!(scopes.len(), 1);
        let w = &scopes[0];
        assert_eq!(w.root, "w1");
        assert_eq!(w.project.as_deref(), Some("widgets"));
        assert_eq!(w.children, ["w2", "w3"]);
        // Fields for plan.import_scopes: without scope fields the database uses its own defaults.
        let v = serde_json::to_value(w).unwrap();
        assert_eq!(v["root"], "w1");
        assert!(v.get("due_date").is_none());
        assert!(v.get("max_defaulted_pct").is_none());
    }

    #[test]
    fn bad_seeds_fail() {
        let bad = [
            "[[scope]]\nroot = \"A-1\"",
            "[[scope]]\nroot = \"r\"\nproject = \"p\"",
            "[[scope]]\nroot = \"r\"\nchildren = [\"r\"]",
            "[[scope]]\nroot = \"r\"\nchildren = [\"a\", \"a\"]",
            "[[scope]]\nroot = \"r\"\nmode = \"backward\"",
            "[[scope]]\nroot = \"r\"\nbuffer_method = \"x\"",
            "[[scope]]\nroot = \"r\"\ngreen_a_pct = 40",
            "[[scope]]\nroot = \"r\"\nmax_defaulted_pct = 101",
            "[[scope]]\nroot = \"r\"\npriority = \"p9\"",
            "[[scope]]\nroot = \"r\"\nbaseline = true",
            "[[scope]]\nroot = \"r\"\n[[scope]]\nroot = \"r\"",
            "[[scope]]\nroot = \"r\"\nchildren = [\"a\"]\n[[scope]]\nroot = \"q\"\nchildren = [\"a\"]",
        ];
        for text in bad {
            assert!(parse_scopes(text).is_err(), "{text}");
        }
        let ok = parse_scopes(
            "[[scope]]\nroot = \"r\"\nchildren = [\"a\"]\ndue_date = \"2030-01-25\"\nmode = \"backward\"",
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&ok[0]).unwrap()["due_date"],
            "2030-01-25"
        );
    }
}
