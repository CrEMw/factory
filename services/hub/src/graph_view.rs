//! The "Graph" view and link migration from text (spec "API and MCP", "Pages", "Migration").
//!
//! GET /api/graph?project=&root=&depth=   tickets and live blocks and part_of edges with a layered layout
//!
//! `root` (a ticket code): the connected part around the ticket along edges in both directions, at most `depth` steps (1..10,
//! default 3). Without `root`, `project` is required: project tickets that have such edges, and their neighbours from other
//! projects one step away. No more than [`MAX_NODES`] nodes are returned (`truncated: true`).
//!
//! Layout in hub, not on the page: a node's layer is the length of the longest path to it along confirmed edges
//! (both rels mean "src before dst"), the order within a layer is by the average order of predecessors, ties by seq.
//! The same graph always gives the same layout (checked by the tests below).
//!
//! Link seed `links/*.jsonl` from the `HUB_SEED_DIR` directory (seed.rs): [`seed`] collects the rows of all files in
//! ascending name order into one import named `links` (actor `seed:links`, as with the former embedded file) and calls
//! `graph.import_links` at startup after the Roadmap snapshot and the people and component seeds. A cycle in the file or a database error does not
//! fail the start: the database rolls back the whole import, hub writes a report to the log at ERROR level and to the
//! `hub.seed.links.*` metrics, the file is fixed by a PR.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use anyhow::Context;
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::Instrument;

use crate::{AppError, bad, non_empty, telemetry, valid_project};

/// The page reads no more nodes and a phone draws no more.
pub const MAX_NODES: usize = 300;
/// Limit of edges hub reads per request: the hub graph is orders of magnitude smaller. Hit it: `truncated: true`.
const MAX_EDGES: i64 = 20_000;
const DEFAULT_DEPTH: usize = 3;

/// Link import name (actor `seed:<name>`, the `seed` metric label): one for all `links/*.jsonl` files.
const LINK_SEED: &str = "links";

#[derive(Deserialize, Serialize, Debug, PartialEq)]
pub struct SeedLink {
    pub src: String,
    pub rel: String,
    pub dst: String,
    #[serde(default)]
    pub source: Option<String>,
}

/// Seed JSONL parsing: lines with `#` and empty lines are skipped; ticket codes are lowercase, rel only blocks and part_of.
pub fn parse_links(text: &str) -> anyhow::Result<Vec<SeedLink>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let l: SeedLink =
            serde_json::from_str(line).with_context(|| format!("seed links line {}", i + 1))?;
        anyhow::ensure!(
            valid_code(&l.src) && valid_code(&l.dst) && l.src != l.dst,
            "seed links line {}: src and dst are two different lowercase ticket codes",
            i + 1
        );
        anyhow::ensure!(
            matches!(l.rel.as_str(), "blocks" | "part_of"),
            "seed links line {}: rel is blocks or part_of",
            i + 1
        );
        out.push(l);
    }
    Ok(out)
}

/// Ticket code: 1..20 lowercase Latin letters and digits (like hub codes).
pub(crate) fn valid_code(c: &str) -> bool {
    (1..=20).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Rows of all link files in sequence; a parse error names the file.
pub fn parse_link_files(files: &[(String, String)]) -> anyhow::Result<Vec<SeedLink>> {
    let mut out = Vec::new();
    for (name, text) in files {
        out.extend(parse_links(text).with_context(|| format!("links/{name}"))?);
    }
    Ok(out)
}

/// Import of link seeds. A parse error fails the start (seeds are checked by a test). No files: no import. A database error
/// (timeout, lock, unexpected trigger) or a cycle does not fail the start: hub writes a report to the log (ERROR) and to the metrics
/// `hub.seed.links.runs{seed, status=ok|cycle|error}` and `hub.seed.links.created{seed}`, by which a post-rollout check
/// sees that the import ran and created edges.
pub async fn seed(db: &PgPool, files: &[(String, String)]) -> anyhow::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let meter = opentelemetry::global::meter("hub");
    let runs = meter.u64_counter("hub.seed.links.runs").build();
    let created = meter.u64_counter("hub.seed.links.created").build();
    let name = LINK_SEED;
    let links = serde_json::to_value(parse_link_files(files)?)?;
    let seed = opentelemetry::KeyValue::new("seed", name);
    let status = match import(db, name, &links).await {
        Err(e) => {
            tracing::error!(
                seed = name,
                error = format!("{e:#}"),
                "seed links failed, nothing written"
            );
            "error"
        }
        Ok(report) => {
            let status = if report["status"] == "cycle" {
                tracing::error!(seed = name, report = %report, "seed links rejected: cycle, nothing written");
                "cycle"
            } else if report["unknown"].as_array().is_some_and(|u| !u.is_empty()) {
                tracing::warn!(seed = name, report = %report, "seed links: some rows skipped");
                "ok"
            } else {
                tracing::info!(seed = name, report = %report, "seed links");
                "ok"
            };
            created.add(
                report["created"].as_u64().unwrap_or(0),
                std::slice::from_ref(&seed),
            );
            status
        }
    };
    runs.add(1, &[seed, opentelemetry::KeyValue::new("status", status)]);
    Ok(())
}

async fn import(
    db: &PgPool,
    name: &str,
    links: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let mut tx = db.begin().await.context("begin seed links")?;
    let (report,): (serde_json::Value,) =
        sqlx::query_as("select graph.import_links($1::jsonb, $2)")
            .bind(links)
            .bind(name)
            .fetch_one(&mut *tx)
            .instrument(telemetry::db_span("import links"))
            .await
            .context("import links")?;
    tx.commit().await.context("commit seed links")?;
    Ok(report)
}

/// Node layout: the layer (column from left to right) and the order within the layer (row from top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub layer: usize,
    pub order: usize,
}

/// Layers by the longest path and order within a layer. `nodes`: node keys (seq), `edges`: (src, dst), src before dst.
/// Edges with an end outside `nodes`, loops and duplicates are ignored. A cycle (confirmed edges never have one, the database
/// forbids it) does not loop: the cycle node with the smaller key is placed as if it had no incoming edges from the cycle.
pub fn layout(nodes: &[i64], edges: &[(i64, i64)]) -> BTreeMap<i64, Place> {
    let set: BTreeSet<i64> = nodes.iter().copied().collect();
    let edges: BTreeSet<(i64, i64)> = edges
        .iter()
        .copied()
        .filter(|(s, d)| s != d && set.contains(s) && set.contains(d))
        .collect();
    let mut preds: BTreeMap<i64, Vec<i64>> = set.iter().map(|&n| (n, Vec::new())).collect();
    let mut succs: BTreeMap<i64, Vec<i64>> = preds.clone();
    for &(s, d) in &edges {
        preds.get_mut(&d).expect("node").push(s);
        succs.get_mut(&s).expect("node").push(d);
    }

    // Kahn with an ordered set of ready nodes: the traversal does not depend on the input order.
    let mut indeg: BTreeMap<i64, usize> = preds.iter().map(|(&n, p)| (n, p.len())).collect();
    let mut ready: BTreeSet<i64> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut layer: BTreeMap<i64, usize> = BTreeMap::new();
    while layer.len() < set.len() {
        let n = match ready.pop_first() {
            Some(n) => n,
            // Cycle: the smallest node not yet placed.
            None => *set
                .iter()
                .find(|n| !layer.contains_key(n))
                .expect("unplaced"),
        };
        if layer.contains_key(&n) {
            continue;
        }
        let l = preds[&n]
            .iter()
            .filter_map(|p| layer.get(p))
            .map(|l| l + 1)
            .max()
            .unwrap_or(0);
        layer.insert(n, l);
        for s in &succs[&n] {
            let d = indeg.get_mut(s).expect("node");
            *d = d.saturating_sub(1);
            if *d == 0 && !layer.contains_key(s) {
                ready.insert(*s);
            }
        }
    }

    // Order: layer 0 by key, then by the average order of predecessors (an integer, in thousandths), then by key.
    let depth = layer.values().copied().max().map_or(0, |m| m + 1);
    let mut by_layer: Vec<Vec<i64>> = vec![Vec::new(); depth];
    for (&n, &l) in &layer {
        by_layer[l].push(n);
    }
    let mut order: HashMap<i64, usize> = HashMap::new();
    for nodes in &mut by_layer {
        let key = |n: &i64| {
            let pos: Vec<usize> = preds[n]
                .iter()
                .filter_map(|p| order.get(p))
                .copied()
                .collect();
            let bary = if pos.is_empty() {
                usize::MAX
            } else {
                pos.iter().sum::<usize>() * 1000 / pos.len()
            };
            (bary, *n)
        };
        let mut keyed: Vec<((usize, i64), i64)> = nodes.iter().map(|n| (key(n), *n)).collect();
        keyed.sort();
        *nodes = keyed.into_iter().map(|(_, n)| n).collect();
        for (i, n) in nodes.iter().enumerate() {
            order.insert(*n, i);
        }
    }
    layer
        .into_iter()
        .map(|(n, l)| {
            (
                n,
                Place {
                    layer: l,
                    order: order[&n],
                },
            )
        })
        .collect()
}

/// Nodes for `root`: breadth-first search along edges in both directions, at most `depth` steps, in traversal order.
fn around(root: i64, edges: &[(i64, i64)], depth: usize) -> Vec<i64> {
    let mut adj: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for &(s, d) in edges {
        adj.entry(s).or_default().insert(d);
        adj.entry(d).or_default().insert(s);
    }
    let mut seen = vec![root];
    let mut dist: HashMap<i64, usize> = HashMap::from([(root, 0)]);
    let mut queue = VecDeque::from([root]);
    while let Some(n) = queue.pop_front() {
        if dist[&n] >= depth {
            continue;
        }
        for &m in adj.get(&n).into_iter().flatten() {
            if !dist.contains_key(&m) {
                dist.insert(m, dist[&n] + 1);
                seen.push(m);
                queue.push_back(m);
            }
        }
    }
    seen
}

#[derive(Deserialize, Default)]
pub struct GraphQuery {
    pub project: Option<String>,
    pub root: Option<String>,
    pub depth: Option<String>,
}

#[derive(sqlx::FromRow)]
struct EdgeRow {
    id: i64,
    rel: String,
    state: String,
    src: i64,
    dst: i64,
}

#[derive(sqlx::FromRow)]
struct TicketRow {
    seq: i64,
    code: String,
    project: String,
    summary: String,
    status: String,
    priority: Option<String>,
}

#[derive(Serialize)]
pub struct GraphNode {
    pub code: String,
    pub project: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
    pub layer: usize,
    pub order: usize,
}

#[derive(Serialize)]
pub struct GraphEdge {
    pub id: i64,
    pub src: String,
    pub dst: String,
    pub rel: String,
    pub state: String,
}

#[derive(Serialize)]
pub struct Graph {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Number of layers (columns).
    pub layers: usize,
    pub truncated: bool,
    /// By layer, then by order within the layer.
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Request check before the database: (project, root, depth).
fn parse(q: &GraphQuery) -> Result<(Option<String>, Option<String>, usize), AppError> {
    let project = non_empty(q.project.as_deref()).map(str::to_owned);
    if project.as_deref().is_some_and(|p| !valid_project(p)) {
        return Err(bad(
            "project: lowercase latin letters, digits and -, up to 40",
        ));
    }
    let root = non_empty(q.root.as_deref()).map(str::to_lowercase);
    if root.as_deref().is_some_and(|r| !valid_code(r)) {
        return Err(bad("root: a ticket code"));
    }
    if project.is_none() && root.is_none() {
        return Err(bad("project or root is required"));
    }
    let depth = match non_empty(q.depth.as_deref()) {
        None => DEFAULT_DEPTH,
        Some(d) => match d.parse::<usize>() {
            Ok(d) if (1..=10).contains(&d) => d,
            _ => return Err(bad("depth: 1 to 10")),
        },
    };
    Ok((project, root, depth))
}

pub async fn get_graph(
    State(db): State<PgPool>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<Graph>, AppError> {
    let (project, root, depth) = parse(&q)?;
    let mut rows: Vec<EdgeRow> = sqlx::query_as(
        "select e.id, e.rel, e.state, gs.ticket_seq as src, gd.ticket_seq as dst from graph.edge e \
         join graph.ticket gs on gs.node_id = e.src and gs.account = e.account \
         join graph.ticket gd on gd.node_id = e.dst and gd.account = e.account \
         where e.account = (select public.instance_account()) and e.valid_to is null and e.rel in ('blocks', 'part_of') \
         order by e.id limit $1",
    )
    .bind(MAX_EDGES + 1)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select graph edges"))
    .await
    .context("select graph edges")?;
    // A row beyond the limit only says there are more edges: the response is flagged, not silently truncated.
    let edges_truncated = rows.len() > MAX_EDGES as usize;
    rows.truncate(MAX_EDGES as usize);
    let pairs: Vec<(i64, i64)> = rows.iter().map(|e| (e.src, e.dst)).collect();

    let mut picked: Vec<i64> = match &root {
        Some(code) => {
            let seq: Option<(i64,)> = sqlx::query_as("select seq from tickets where code = $1")
                .bind(code)
                .fetch_optional(&db)
                .instrument(telemetry::db_span("select root ticket"))
                .await
                .context("select root ticket")?;
            let Some((seq,)) = seq else {
                return Err(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"));
            };
            around(seq, &pairs, depth)
        }
        None => {
            let ends: Vec<i64> = pairs
                .iter()
                .flat_map(|(s, d)| [*s, *d])
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let (own,): (Vec<i64>,) = sqlx::query_as(
                "select coalesce(array_agg(seq order by seq), '{}') from tickets \
                 where project = $1 and seq = any($2)",
            )
            .bind(project.as_deref())
            .bind(&ends)
            .fetch_one(&db)
            .instrument(telemetry::db_span("select project graph tickets"))
            .await
            .context("select project graph tickets")?;
            let own: BTreeSet<i64> = own.into_iter().collect();
            let mut out: Vec<i64> = own.iter().copied().collect();
            let mut extra: BTreeSet<i64> = BTreeSet::new();
            for &(s, d) in &pairs {
                if own.contains(&s) && !own.contains(&d) {
                    extra.insert(d);
                }
                if own.contains(&d) && !own.contains(&s) {
                    extra.insert(s);
                }
            }
            out.extend(extra);
            out
        }
    };
    let truncated = edges_truncated || picked.len() > MAX_NODES;
    picked.truncate(MAX_NODES);

    let tickets: Vec<TicketRow> = sqlx::query_as(
        "select seq, code, project, summary, status, priority from tickets where seq = any($1)",
    )
    .bind(&picked)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select graph tickets"))
    .await
    .context("select graph tickets")?;
    let by_seq: HashMap<i64, TicketRow> = tickets.into_iter().map(|t| (t.seq, t)).collect();
    let set: BTreeSet<i64> = picked.iter().copied().collect();
    let inside: Vec<&EdgeRow> = rows
        .iter()
        .filter(|e| set.contains(&e.src) && set.contains(&e.dst))
        .collect();
    let confirmed: Vec<(i64, i64)> = inside
        .iter()
        .filter(|e| e.state == "confirmed")
        .map(|e| (e.src, e.dst))
        .collect();
    let places = layout(&picked, &confirmed);

    let mut nodes: Vec<GraphNode> = places
        .iter()
        .filter_map(|(seq, p)| {
            by_seq.get(seq).map(|t| GraphNode {
                code: t.code.clone(),
                project: t.project.clone(),
                summary: t.summary.clone(),
                status: t.status.clone(),
                priority: t.priority.clone(),
                layer: p.layer,
                order: p.order,
            })
        })
        .collect();
    nodes.sort_by_key(|n| (n.layer, n.order));
    let code = |s: i64| by_seq.get(&s).map(|t| t.code.clone());
    let edges = inside
        .iter()
        .filter_map(|e| {
            Some(GraphEdge {
                id: e.id,
                src: code(e.src)?,
                dst: code(e.dst)?,
                rel: e.rel.clone(),
                state: e.state.clone(),
            })
        })
        .collect();
    Ok(Json(Graph {
        project,
        root,
        layers: places.values().map(|p| p.layer + 1).max().unwrap_or(0),
        truncated,
        nodes,
        edges,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(nodes: &[i64], edges: &[(i64, i64)]) -> Vec<(i64, usize, usize)> {
        layout(nodes, edges)
            .into_iter()
            .map(|(n, p)| (n, p.layer, p.order))
            .collect()
    }

    #[test]
    fn layers_follow_the_longest_path() {
        // 1 -> 2 -> 3 and a short path 1 -> 3: 3 is in layer 2, not 1.
        let got = places(&[1, 2, 3, 4], &[(1, 2), (2, 3), (1, 3)]);
        assert_eq!(got, vec![(1, 0, 0), (2, 1, 0), (3, 2, 0), (4, 0, 1)]);
    }

    #[test]
    fn layout_is_deterministic_for_any_input_order() {
        let nodes = [10, 3, 7, 1, 5, 9, 2];
        let edges = [(1, 5), (3, 5), (5, 9), (2, 7), (7, 9), (3, 10), (1, 5)];
        let want = places(&nodes, &edges);
        let mut n = nodes.to_vec();
        let mut e = edges.to_vec();
        for i in 0..20 {
            n.rotate_left(1);
            e.reverse();
            if i % 3 == 0 {
                e.rotate_left(2);
            }
            assert_eq!(places(&n, &e), want, "round {i}");
        }
    }

    #[test]
    fn order_in_a_layer_follows_predecessors() {
        // 3 comes from 2, 4 from 1; 1 is above 2, so 4 is above 3, although by key 3 < 4.
        let got = places(&[1, 2, 3, 4], &[(1, 4), (2, 3)]);
        assert_eq!(got, vec![(1, 0, 0), (2, 0, 1), (3, 1, 1), (4, 1, 0)]);
    }

    #[test]
    fn a_cycle_does_not_hang_and_foreign_edges_are_ignored() {
        let got = places(&[1, 2, 3], &[(1, 2), (2, 3), (3, 1), (3, 99), (2, 2)]);
        assert_eq!(got.len(), 3);
        assert_eq!(got, places(&[3, 2, 1], &[(3, 1), (2, 3), (1, 2)]));
    }

    #[test]
    fn around_walks_both_ways_up_to_depth() {
        let edges = [(1, 2), (2, 3), (3, 4), (5, 2)];
        assert_eq!(around(3, &edges, 1), vec![3, 2, 4]);
        assert_eq!(around(3, &edges, 2), vec![3, 2, 4, 1, 5]);
        assert_eq!(around(9, &edges, 3), vec![9]);
    }

    #[test]
    fn seed_file_parses_and_has_no_cycle() {
        // Every seed in the tree: the example, and an install seed when the tree has one.
        for dir in crate::seed::tree_dirs() {
            let seeds = crate::seed::Seeds::read(&dir).unwrap();
            let links = parse_link_files(&seeds.links).unwrap();
            assert!(!links.is_empty(), "{}", dir.display());
            // In every row source names the ticket whose text holds the link: "<code>:body" or ":summary".
            assert!(links.iter().all(|l| l.source.as_deref().is_some_and(|s| {
                s.split_once(':')
                    .is_some_and(|(c, f)| valid_code(c) && matches!(f, "body" | "summary"))
            })));
            // A ticket has one part_of (index edge_single).
            let parents: Vec<&str> = links
                .iter()
                .filter(|l| l.rel == "part_of")
                .map(|l| l.src.as_str())
                .collect();
            assert_eq!(parents.len(), parents.iter().collect::<BTreeSet<_>>().len());
            // No cycle: Kahn places all nodes without breaking one (every edge goes to a later layer).
            let mut ids: HashMap<&str, i64> = HashMap::new();
            for l in &links {
                for c in [&l.src, &l.dst] {
                    let next = ids.len() as i64;
                    ids.entry(c.as_str()).or_insert(next);
                }
            }
            let edges: Vec<(i64, i64)> = links
                .iter()
                .map(|l| (ids[l.src.as_str()], ids[l.dst.as_str()]))
                .collect();
            let nodes: Vec<i64> = ids.values().copied().collect();
            let p = layout(&nodes, &edges);
            assert!(edges.iter().all(|(s, d)| p[s].layer < p[d].layer));
        }
    }

    #[test]
    fn bad_seed_lines_fail() {
        assert!(parse_links("# c\n\n{\"src\":\"a\",\"rel\":\"blocks\",\"dst\":\"b\"}\n").is_ok());
        for bad in [
            r#"{"src":"A","rel":"blocks","dst":"b"}"#,
            r#"{"src":"a","rel":"blocks","dst":"a"}"#,
            r#"{"src":"a","rel":"relates","dst":"b"}"#,
            "not json",
        ] {
            assert!(parse_links(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn queries_are_checked_before_the_database() {
        let q = |p: Option<&str>, r: Option<&str>, d: Option<&str>| GraphQuery {
            project: p.map(Into::into),
            root: r.map(Into::into),
            depth: d.map(Into::into),
        };
        assert!(parse(&q(None, None, None)).is_err());
        assert!(parse(&q(Some("Demo Project"), None, None)).is_err());
        assert!(parse(&q(None, Some("a-b"), None)).is_err());
        assert!(parse(&q(None, Some("Z6"), Some("11"))).is_err());
        assert_eq!(
            parse(&q(None, Some("Z6"), None)).unwrap(),
            (None, Some("z6".into()), 3)
        );
        assert_eq!(
            parse(&q(Some("demo"), None, Some("2"))).unwrap(),
            (Some("demo".into()), None, 2)
        );
    }
}
