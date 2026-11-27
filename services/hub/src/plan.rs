//! Scopes, estimates, WSJF and the critical chain computation in hub (spec "Ticket fields",
//! "Critical chain", "API and MCP"). Tables: 0013_plan.sql; the pure computation: [`crate::ccpm`].
//!
//! PUT  /api/tickets/{code}/estimate  `{effort_sp, aggressive_hd?, safe_hd?, remaining_hd?, start_not_before?}`
//! PUT  /api/tickets/{code}/wsjf      `{business_value, time_criticality, risk_opportunity}`
//! GET  /api/tickets/{code}/plan      `{estimate, wsjf}`: live versions (null if none), for the ticket page forms
//! GET  /api/scopes                   live scopes with the last computation
//! POST /api/scopes                   `{root, due_date?, mode?, buffer_method?, green_a_pct?, red_a_pct?,
//!                                    zone_slope_pct?, max_defaulted_pct?}` -> 201; 409 `scope_exists`
//! PUT  /api/scopes/{root}            the same fields without root: a new row version, missing fields come from the live version
//!                                    (`due_date: null` clears the due date); 404 `no_scope`; thresholds are changed only
//!                                    by a human: 403 `thresholds_require_human`
//! GET  /api/plan/{root}              the current scope plan (from plan.current, computed on the fly before the first computation)
//! GET  /api/plan/{root}/fever        fever points by day and buffer (empty until a baseline is approved)
//! POST /api/plan/{root}/baseline     `{accept_defaults?}`: approve the baseline, human only; 409
//!                                    `defaults_over_threshold`, `nothing_to_approve`, `stale_baseline`
//! GET  /api/next?person=<handle>     the next task across all scopes and outside them (rule 13)
//! GET  /api/backlog?project=&limit=  open and doing leaves by WSJF descending (without WSJF last), scopes by WSJF
//!
//! Every write goes as a new row version in a transaction with an actor (tx.rs); the database checks the actor kind and the rules.
//! The background recomputation ([`spawn`]) takes scopes from `plan.dirty` every [`EVERY`], builds the input from the graph (live
//! confirmed `blocks`, `part_of`, `duplicates`, `assigned_to`), estimates and WSJF, compares `input_hash` with
//! `plan.current` and writes the plan only when it differs, together with today's fever points if there is a baseline.
//! At [`NIGHTLY`] installation local time (zone from public.instance, the time comes from Postgres) all scopes
//! are marked dirty: today moves. A scope whose plan was written [`WRITE_LIMIT`] times within [`WRITE_WINDOW`]
//! is deferred until the end of the window and is not picked from the queue ([`WriteLog`]).
//! Plan time: working half-days ([`crate::ccpm::model::HalfDay`]); a scope's due date is the end of the `due_date` day.

use std::{
    collections::{BTreeMap, BTreeSet},
    panic::AssertUnwindSafe,
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use opentelemetry::{KeyValue, global, metrics::Gauge};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tracing::Instrument;

use crate::{
    AppError, bad,
    ccpm::{
        self, CCPM_ALGO_VERSION,
        model::{
            Baseline, BlockEdge, BufferKind, BufferMethod, Estimate, ExternalIn, Flags, HalfDay,
            Mode, Plan, PlanConfig, PlanInput, Status, TaskIn, WsjfIn, Zone, date_of, half_day_of,
        },
        next::OutsideTask,
    },
    config::{local_date, local_time},
    graph, people, query, telemetry,
    tx::{self, Actor},
};

/// Fibonacci points for effort and the WSJF terms (like the CHECK in 0013_plan.sql).
pub const FIB: [i64; 7] = [1, 2, 3, 5, 8, 13, 21];
/// Tag of the `plan.type_default_estimate` row for tickets without a matching tag.
pub const OTHER_TAG: &str = "*";
/// How often the background recomputation checks the queue.
pub const EVERY: Duration = Duration::from_secs(5);
/// When, in installation local time, all scopes are marked dirty (today moves).
pub const NIGHTLY: NaiveTime = NaiveTime::from_hms_opt(0, 5, 0).expect("00:05");
/// How many scopes the recomputation takes per pass.
const BATCH: i64 = 50;
/// Installation local time from Postgres: zone from public.instance (step 2.5), zone rules from the Postgres tzdata, no
/// dependency in hub.
pub(crate) const LOCAL_NOW: &str = concat!("select ", local_time!("now()"), "::timestamp");

static BUFFER_CONSUMED: LazyLock<Gauge<i64>> = LazyLock::new(|| {
    global::meter("hub")
        .i64_gauge("ccpm_buffer_consumed_pct")
        .with_unit("%")
        .build()
});

fn json_err(status: StatusCode, body: Value) -> AppError {
    AppError::Json(status, body)
}

/// plan trigger errors into API responses; everything else as for the graph.
fn db_error(e: sqlx::Error, what: &'static str) -> AppError {
    if let sqlx::Error::Database(d) = &e {
        let mapped = match d.code().as_deref() {
            Some("PLKND") => Some((StatusCode::FORBIDDEN, "actor_kind_not_allowed")),
            Some("GRHUM") => Some((StatusCode::FORBIDDEN, "approve_requires_human")),
            Some("PLSUP") => Some((StatusCode::CONFLICT, "stale_baseline")),
            Some("PLDEF") => Some((StatusCode::CONFLICT, "defaults_over_threshold")),
            Some("PLTHR") => Some((StatusCode::FORBIDDEN, "thresholds_require_human")),
            Some("PLSCP") => Some((StatusCode::NOT_FOUND, "no_scope")),
            Some("23505") => match d.constraint() {
                Some("baseline_succession") => Some((StatusCode::CONFLICT, "stale_baseline")),
                Some("estimate_live" | "wsjf_live" | "scope_live") => {
                    Some((StatusCode::CONFLICT, "concurrent_write"))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some((status, error)) = mapped {
            return json_err(status, json!({ "error": error }));
        }
    }
    graph::db_error(e, what)
}

// ---------------------------------------------------------------------------------------------------------------
// Computation input from database rows (pure functions, tests below)

/// A scope ticket with everything the computation needs.
#[derive(sqlx::FromRow, Clone, Debug, Default)]
pub struct TaskRow {
    pub seq: i64,
    pub code: String,
    pub status: String,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    pub part_of: Option<i64>,
    pub duplicates: Option<i64>,
    pub assignee: Option<String>,
    pub effort_sp: Option<i64>,
    pub aggressive_hd: Option<i64>,
    pub safe_hd: Option<i64>,
    pub remaining_hd: Option<i64>,
    pub start_not_before: Option<NaiveDate>,
    pub business_value: Option<i64>,
    pub time_criticality: Option<i64>,
    pub risk_opportunity: Option<i64>,
    /// Closing day in installation local time (done only).
    pub done_on: Option<NaiveDate>,
}

/// Live confirmed `blocks`: `src` blocks `dst` (ticket seqs).
#[derive(sqlx::FromRow, Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeRow {
    pub src: i64,
    pub dst: i64,
}

/// A blocker outside the scope and the projected end of its task in `plan.current` of another scope.
#[derive(sqlx::FromRow, Clone, Debug)]
pub struct ExternalRow {
    pub seq: i64,
    pub code: String,
    pub status: String,
    pub forecast_end: Option<i64>,
}

pub fn status_of(s: &str) -> Status {
    match s {
        "doing" => Status::Doing,
        "done" => Status::Done,
        "cancelled" => Status::Cancelled,
        _ => Status::Open,
    }
}

/// `p0`..`p3` -> 0..3; otherwise empty (goes last).
pub fn priority_of(p: Option<&str>) -> Option<i64> {
    p?.strip_prefix('p')?.parse().ok()
}

/// `ccpm::plan` input from rows: tasks by seq, edges ending in the scope (starting in the scope or at a known external
/// blocker), only the external blockers that are on edges. The root's parent outside the scope is not part of the computation. The order of
/// database rows does not affect the input: it is canonical and goes into `input_hash`.
pub fn build_input(
    root: i64,
    tasks: Vec<TaskRow>,
    edges: &[EdgeRow],
    externals: Vec<ExternalRow>,
) -> PlanInput {
    let mut tasks: Vec<TaskIn> = tasks
        .into_iter()
        .map(|r| {
            let status = status_of(&r.status);
            let mut tags = r.tags;
            tags.sort();
            tags.dedup();
            TaskIn {
                seq: r.seq,
                code: r.code,
                status,
                priority: priority_of(r.priority.as_deref()),
                tags,
                assignee: r.assignee,
                estimate: r.effort_sp.map(|effort_sp| Estimate {
                    effort_sp,
                    aggressive_hd: r.aggressive_hd,
                    safe_hd: r.safe_hd,
                    remaining_hd: r.remaining_hd,
                    start_not_before: r.start_not_before.map(half_day_of),
                }),
                wsjf: match (r.business_value, r.time_criticality, r.risk_opportunity) {
                    (Some(business_value), Some(time_criticality), Some(risk_opportunity)) => {
                        Some(WsjfIn {
                            business_value,
                            time_criticality,
                            risk_opportunity,
                        })
                    }
                    _ => None,
                },
                part_of: r.part_of.filter(|_| r.seq != root),
                duplicates: r.duplicates,
                done_at: r
                    .done_on
                    .filter(|_| status == Status::Done)
                    .map(half_day_of),
            }
        })
        .collect();
    tasks.sort_by_key(|t| t.seq);
    tasks.dedup_by_key(|t| t.seq);
    let inside: BTreeSet<i64> = tasks.iter().map(|t| t.seq).collect();
    let ext: BTreeMap<i64, ExternalRow> = externals
        .into_iter()
        .filter(|e| !inside.contains(&e.seq))
        .map(|e| (e.seq, e))
        .collect();
    let blocks: BTreeSet<BlockEdge> = edges
        .iter()
        .filter(|e| e.src != e.dst && inside.contains(&e.dst))
        .filter(|e| inside.contains(&e.src) || ext.contains_key(&e.src))
        .map(|e| BlockEdge {
            src: e.src,
            dst: e.dst,
        })
        .collect();
    let used: BTreeSet<i64> = blocks
        .iter()
        .map(|e| e.src)
        .filter(|s| !inside.contains(s))
        .collect();
    let external = ext
        .into_values()
        .filter(|e| used.contains(&e.seq))
        .map(|e| ExternalIn {
            seq: e.seq,
            status: status_of(&e.status),
            code: e.code,
            forecast_end: e.forecast_end,
        })
        .collect();
    PlanInput {
        root,
        tasks,
        blocks: blocks.into_iter().collect(),
        external,
    }
}

/// A live `plan.scope` row with the root's code and summary.
#[derive(sqlx::FromRow, Serialize, Clone, Debug)]
pub struct ScopeRow {
    pub id: i64,
    #[serde(skip)]
    pub root_seq: i64,
    /// Root code.
    pub root: String,
    pub summary: String,
    pub due_date: Option<NaiveDate>,
    pub mode: String,
    pub buffer_method: String,
    pub green_a_pct: i64,
    pub red_a_pct: i64,
    pub zone_slope_pct: i64,
    pub max_defaulted_pct: i64,
    pub valid_from: DateTime<Utc>,
    pub actor: String,
    pub actor_kind: String,
}

const SCOPE_SELECT: &str = "select s.id, s.root_seq, t.code as root, t.summary, s.due_date, s.mode, \
    s.buffer_method, s.green_a_pct::int8 as green_a_pct, s.red_a_pct::int8 as red_a_pct, \
    s.zone_slope_pct::int8 as zone_slope_pct, s.max_defaulted_pct::int8 as max_defaulted_pct, s.valid_from, \
    s.actor, s.actor_kind from plan.scope s join tickets t on t.seq = s.root_seq \
    where s.account = (select public.instance_account()) and s.valid_to is null";

/// Computation settings: the live scope and the points dictionaries. Due date: the end of the `due_date` day (the start of the next working
/// half-day after it).
pub fn config(s: &ScopeRow, defaults: &[(String, i64)], points: &[(i64, i64)]) -> PlanConfig {
    let base = PlanConfig::default();
    let mut type_default_sp = BTreeMap::new();
    let mut other = base.other_default_sp;
    for (tag, sp) in defaults {
        if tag == OTHER_TAG {
            other = *sp;
        } else {
            type_default_sp.insert(tag.clone(), *sp);
        }
    }
    PlanConfig {
        mode: if s.mode == "backward" {
            Mode::Backward
        } else {
            Mode::Forward
        },
        due: s.due_date.map(|d| half_day_of(d.succ_opt().unwrap_or(d))),
        buffer_method: if s.buffer_method == "ssq" {
            BufferMethod::Ssq
        } else {
            BufferMethod::Cnp
        },
        green_a_pct: s.green_a_pct,
        red_a_pct: s.red_a_pct,
        zone_slope_pct: s.zone_slope_pct,
        max_defaulted_pct: s.max_defaulted_pct,
        type_default_sp,
        other_default_sp: other,
        points_to_hd: points.iter().copied().collect(),
    }
}

/// Everything the result depends on: the field order is fixed by the struct (no JSON maps are involved here).
#[derive(Serialize)]
struct HashIn<'a> {
    algo: u32,
    today: HalfDay,
    baseline: Option<i64>,
    config: &'a PlanConfig,
    input: &'a PlanInput,
}

/// `sha256(canonical input + today + CCPM_ALGO_VERSION + scope settings and dictionaries + baseline id)`.
pub fn input_hash(
    input: &PlanInput,
    today: HalfDay,
    cfg: &PlanConfig,
    baseline: Option<i64>,
) -> Vec<u8> {
    let bytes = serde_json::to_vec(&HashIn {
        algo: CCPM_ALGO_VERSION,
        today,
        baseline,
        config: cfg,
        input,
    })
    .expect("plan input serializes");
    Sha256::digest(bytes).to_vec()
}

/// Nightly marking of all scopes: installation local time has passed [`NIGHTLY`] and it has not happened today yet. After hub
/// starts (`last` is empty) it runs at once if 00:05 has already passed: that way a new algorithm recomputes everything after a rollout.
pub fn nightly_due(now: NaiveDateTime, last: Option<NaiveDate>) -> bool {
    now.time() >= NIGHTLY && last != Some(now.date())
}

// ---------------------------------------------------------------------------------------------------------------
// Reading from the database

pub(crate) const TASKS_SQL: &str = concat!(
    "with recursive down(seq) as ( \
      select $1::bigint \
      union \
      select ct.ticket_seq from down \
      join graph.ticket pt on pt.account = (select public.instance_account()) and pt.ticket_seq = down.seq \
      join graph.edge e on e.dst = pt.node_id and e.rel = 'part_of' and e.state = 'confirmed' and e.valid_to is null \
      join graph.ticket ct on ct.node_id = e.src) \
    select t.seq, t.code, t.status, t.priority, t.tags, \
      (select p.ticket_seq from graph.ticket g join graph.edge e on e.src = g.node_id and e.rel = 'part_of' \
        and e.state = 'confirmed' and e.valid_to is null join graph.ticket p on p.node_id = e.dst \
        where g.ticket_seq = t.seq) as part_of, \
      (select p.ticket_seq from graph.ticket g join graph.edge e on e.src = g.node_id and e.rel = 'duplicates' \
        and e.state = 'confirmed' and e.valid_to is null join graph.ticket p on p.node_id = e.dst \
        where g.ticket_seq = t.seq) as duplicates, \
      (select gp.handle from graph.ticket g join graph.edge e on e.src = g.node_id and e.rel = 'assigned_to' \
        and e.state = 'confirmed' and e.valid_to is null join graph.person gp on gp.node_id = e.dst \
        where g.ticket_seq = t.seq) as assignee, \
      pe.effort_sp::int8 as effort_sp, pe.aggressive_hd::int8 as aggressive_hd, pe.safe_hd::int8 as safe_hd, \
      pe.remaining_hd::int8 as remaining_hd, pe.start_not_before, \
      pw.business_value::int8 as business_value, pw.time_criticality::int8 as time_criticality, \
      pw.risk_opportunity::int8 as risk_opportunity, \
      case when t.status = 'done' then coalesce( \
        (select ",
    local_date!("c.at"),
    " from ticket_change c \
          where c.ticket_seq = t.seq and c.field = 'status' and c.new = '\"done\"'::jsonb order by c.id desc limit 1), \
        ",
    local_date!("t.updated_at"),
    ") end as done_on \
    from down join tickets t on t.seq = down.seq \
    left join plan.ticket_estimate pe on pe.account = (select public.instance_account()) and pe.ticket_seq = t.seq and pe.valid_to is null \
    left join plan.ticket_wsjf pw on pw.account = (select public.instance_account()) and pw.ticket_seq = t.seq and pw.valid_to is null \
    order by t.seq"
);

const EDGES_SQL: &str = "select s.ticket_seq as src, d.ticket_seq as dst from graph.edge e \
    join graph.ticket s on s.node_id = e.src join graph.ticket d on d.node_id = e.dst \
    where e.account = (select public.instance_account()) and e.rel = 'blocks' and e.state = 'confirmed' and e.valid_to is null \
      and d.ticket_seq = any($1) order by 1, 2";

/// Projected end of an external blocker: its task in the current plan of another live scope.
const EXTERNAL_SQL: &str = "select t.seq, t.code, t.status, \
      (select max((x ->> 'end')::int8) from plan.current c \
        join plan.scope s on s.account = c.account and s.root_seq = c.root_seq and s.valid_to is null \
        cross join lateral jsonb_array_elements(c.result -> 'tasks') x \
        where c.account = (select public.instance_account()) and c.root_seq <> $2 and (x ->> 'seq')::int8 = t.seq) as forecast_end \
    from tickets t where t.seq = any($1) order by t.seq";

/// Scope input, its settings and the live baseline.
pub struct Loaded {
    pub scope: ScopeRow,
    pub cfg: PlanConfig,
    pub input: PlanInput,
    pub baseline: Option<(i64, Baseline)>,
}

pub async fn local_now(conn: &mut PgConnection) -> anyhow::Result<NaiveDateTime> {
    let (now,): (NaiveDateTime,) = sqlx::query_as(LOCAL_NOW)
        .fetch_one(&mut *conn)
        .instrument(telemetry::db_span("select local time"))
        .await
        .context("select local time")?;
    Ok(now)
}

async fn live_scope(conn: &mut PgConnection, root: i64) -> anyhow::Result<Option<ScopeRow>> {
    sqlx::query_as(&format!("{SCOPE_SELECT} and s.root_seq = $1"))
        .bind(root)
        .fetch_optional(&mut *conn)
        .instrument(telemetry::db_span("select scope"))
        .await
        .context("select scope")
}

/// Scope input from the graph, estimates and WSJF; empty if the root has no live scope.
pub async fn load(conn: &mut PgConnection, root: i64) -> anyhow::Result<Option<Loaded>> {
    let Some(scope) = live_scope(conn, root).await? else {
        return Ok(None);
    };
    let defaults: Vec<(String, i64)> = sqlx::query_as(
        "select tag, effort_sp::int8 from plan.type_default_estimate \
         where account = (select public.instance_account()) and valid_to is null order by tag",
    )
    .fetch_all(&mut *conn)
    .instrument(telemetry::db_span("select type defaults"))
    .await
    .context("select type defaults")?;
    let points: Vec<(i64, i64)> = sqlx::query_as(
        "select effort_sp::int8, aggressive_hd::int8 from plan.points_to_hd \
         where account = (select public.instance_account()) and valid_to is null order by effort_sp",
    )
    .fetch_all(&mut *conn)
    .instrument(telemetry::db_span("select points"))
    .await
    .context("select points")?;
    let tasks: Vec<TaskRow> = sqlx::query_as(TASKS_SQL)
        .bind(root)
        .fetch_all(&mut *conn)
        .instrument(telemetry::db_span("select scope tasks"))
        .await
        .context("select scope tasks")?;
    let seqs: Vec<i64> = tasks.iter().map(|t| t.seq).collect();
    let edges: Vec<EdgeRow> = sqlx::query_as(EDGES_SQL)
        .bind(&seqs)
        .fetch_all(&mut *conn)
        .instrument(telemetry::db_span("select scope edges"))
        .await
        .context("select scope edges")?;
    let inside: BTreeSet<i64> = seqs.iter().copied().collect();
    let outside: Vec<i64> = edges
        .iter()
        .map(|e| e.src)
        .filter(|s| !inside.contains(s))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let externals: Vec<ExternalRow> = if outside.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(EXTERNAL_SQL)
            .bind(&outside)
            .bind(root)
            .fetch_all(&mut *conn)
            .instrument(telemetry::db_span("select external blockers"))
            .await
            .context("select external blockers")?
    };
    let baseline: Option<(i64, Value)> = sqlx::query_as(
        "select id, result from plan.baseline_live where account = (select public.instance_account()) and root_seq = $1",
    )
    .bind(root)
    .fetch_optional(&mut *conn)
    .instrument(telemetry::db_span("select live baseline"))
    .await
    .context("select live baseline")?;
    let baseline = match baseline {
        Some((id, v)) => Some((
            id,
            serde_json::from_value(v).with_context(|| format!("baseline {id}"))?,
        )),
        None => None,
    };
    let cfg = config(&scope, &defaults, &points);
    Ok(Some(Loaded {
        input: build_input(root, tasks, &edges, externals),
        scope,
        cfg,
        baseline,
    }))
}

/// Projection against the live baseline (if any) for the day `today`.
pub fn compute(l: &Loaded, today: NaiveDate) -> Plan {
    ccpm::plan(
        &l.input,
        l.baseline.as_ref().map(|b| &b.1),
        half_day_of(today),
        &l.cfg,
    )
}

/// Computation with panic catching: a `ccpm::plan` panic gives an error with a log record (500 in a route), not
/// a dropped connection or the end of the background task.
pub fn caught<T>(scope: &str, f: impl FnOnce() -> T) -> anyhow::Result<T> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| {
        tracing::error!(scope, "ccpm::plan panicked");
        anyhow::anyhow!("ccpm::plan panicked for scope {scope}")
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Background recomputation

fn plan_actor() -> Actor {
    Actor {
        id: "hub-plan".to_owned(),
        kind: "service:plan".to_owned(),
    }
}

/// Recompute one scope. `marked`: the queue counter value read before the computation; afterwards `done`
/// is raised to that value, and a mark during the computation gives one more pass. Returns whether the plan was written.
pub async fn recompute(
    db: &PgPool,
    root: i64,
    marked: i64,
    today: NaiveDate,
) -> anyhow::Result<bool> {
    let mut tx = db.begin().await.context("begin plan")?;
    tx::set_actor(&mut tx, &plan_actor()).await?;
    // Two hub copies do not compute one scope at the same time; a busy one is taken by the next pass.
    let (got,): (bool,) = sqlx::query_as("select pg_try_advisory_xact_lock(193701, $1::int)")
        .bind(root)
        .fetch_one(&mut *tx)
        .await
        .context("plan lock")?;
    if !got {
        return Ok(false);
    }
    let mut written = false;
    if let Some(l) = load(&mut tx, root).await? {
        let today_hd = half_day_of(today);
        let hash = input_hash(&l.input, today_hd, &l.cfg, l.baseline.as_ref().map(|b| b.0));
        let old: Option<(Vec<u8>, i32)> = sqlx::query_as(
            "select input_hash, algo_version from plan.current where account = (select public.instance_account()) and root_seq = $1",
        )
        .bind(root)
        .fetch_optional(&mut *tx)
        .await
        .context("select current plan hash")?;
        let same =
            old.is_some_and(|(h, v)| h == hash && i64::from(v) == i64::from(CCPM_ALGO_VERSION));
        if !same {
            let span = tracing::info_span!(
                "ccpm.plan",
                scope = %l.scope.root,
                tasks = l.input.tasks.len(),
                edges = l.input.blocks.len(),
                chain_len = tracing::field::Empty,
                duration_ms = tracing::field::Empty,
            );
            let start = Instant::now();
            // A computation panic must not kill the background task: the scope is marked processed, the plan stays as it was.
            let plan = span.in_scope(|| caught(&l.scope.root, || compute(&l, today)));
            span.record("duration_ms", start.elapsed().as_millis() as u64);
            if let Ok(plan) = plan {
                span.record("chain_len", plan.chain.len());
                write_plan(&mut tx, &l, &plan, hash, today).await?;
                written = true;
            }
        }
    }
    // done of its own scope and marks of others (new plan.current) in one command in root order (0013_plan.sql).
    sqlx::query("select plan.flush(public.instance_account(), $1, $2)")
        .bind(root)
        .bind(marked)
        .execute(&mut *tx)
        .await
        .context("plan dirty done")?;
    tx.commit().await.context("commit plan")?;
    Ok(written)
}

async fn write_plan(
    conn: &mut PgConnection,
    l: &Loaded,
    plan: &Plan,
    hash: Vec<u8>,
    today: NaiveDate,
) -> anyhow::Result<()> {
    let root = l.scope.root_seq;
    let baseline = l.baseline.as_ref().map(|b| b.0);
    sqlx::query(
        "insert into plan.current (root_seq, input_hash, algo_version, computed_at, today, baseline_id, result) \
         values ($1, $2, $3, now(), $4, $5, $6) on conflict (account, root_seq) do update set \
         input_hash = excluded.input_hash, algo_version = excluded.algo_version, computed_at = now(), \
         today = excluded.today, baseline_id = excluded.baseline_id, result = excluded.result",
    )
    .bind(root)
    .bind(hash)
    .bind(CCPM_ALGO_VERSION as i32)
    .bind(today)
    .bind(baseline)
    .bind(serde_json::to_value(plan)?)
    .execute(&mut *conn)
    .instrument(telemetry::db_span("upsert current plan"))
    .await
    .context("upsert current plan")?;
    let Some(baseline) = baseline else {
        return Ok(());
    };
    let pct = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    for b in &plan.buffers {
        let (Some(cc), Some(consumed)) = (b.chain_complete_pct, b.consumed_pct) else {
            continue;
        };
        let kind = match b.kind {
            BufferKind::Project => "project",
            BufferKind::Feeding => "feeding",
        };
        let zone = b.zone.map(|z| match z {
            Zone::Green => "green",
            Zone::Yellow => "yellow",
            Zone::Red => "red",
        });
        let flags: Vec<&str> = if b.compressed {
            vec!["compressed"]
        } else {
            vec![]
        };
        sqlx::query(
            "insert into plan.fever_point (root_seq, day, buffer_key, baseline_id, kind, chain_complete_pct, \
             consumed_pct, zone, flags) values ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             on conflict (account, root_seq, day, buffer_key) do update set baseline_id = excluded.baseline_id, \
             kind = excluded.kind, chain_complete_pct = excluded.chain_complete_pct, \
             consumed_pct = excluded.consumed_pct, zone = excluded.zone, flags = excluded.flags",
        )
        .bind(root)
        .bind(today)
        .bind(&b.key)
        .bind(baseline)
        .bind(kind)
        .bind(pct(cc))
        .bind(pct(consumed))
        .bind(zone)
        .bind(&flags)
        .execute(&mut *conn)
        .instrument(telemetry::db_span("upsert fever point"))
        .await
        .context("upsert fever point")?;
        BUFFER_CONSUMED.record(
            consumed,
            &[
                KeyValue::new("scope", l.scope.root.clone()),
                KeyValue::new("kind", kind),
                KeyValue::new("buffer", b.key.clone()),
            ],
        );
    }
    Ok(())
}

/// How many times the plan of one scope is written within [`WRITE_WINDOW`] before the recomputation defers it.
pub const WRITE_LIMIT: usize = 30;
/// Window of the plan write limit for one scope.
pub const WRITE_WINDOW: Duration = Duration::from_secs(600);

/// "Seesaw" limit: a new scope plan marks the scopes that read its milestones (0020), and if
/// they move this scope's milestones in return, the pair is recomputed in a loop. A repeated `input_hash` stops the loop when
/// the computation converges; this log holds a loop that does not converge: a scope whose plan was written [`WRITE_LIMIT`] times
/// within [`WRITE_WINDOW`] is deferred until the end of the window. Deferred scopes are not picked from the queue and do not hold its
/// head; the WARN and the `ccpm_plan_throttled` metric are written once per entry into the deferred state.
/// The mark in `plan.dirty` stays (done is not raised), so the plan is finished after the window instead of being lost.
/// The log lives in process memory: each hub copy has its own.
#[derive(Default, Debug)]
pub struct WriteLog {
    writes: BTreeMap<i64, std::collections::VecDeque<Instant>>,
    held: BTreeSet<i64>,
}

/// Deferred scopes at the moment of a pass: all of them, the just deferred and the just released.
#[derive(Debug, PartialEq, Eq)]
pub struct Held {
    pub roots: Vec<i64>,
    pub entered: Vec<i64>,
    pub left: Vec<i64>,
}

impl WriteLog {
    /// Which scopes are deferred now; also forgets records older than the window and remembers the deferred ones, so that
    /// the next call names entry and exit only once.
    pub fn held(&mut self, now: Instant) -> Held {
        self.writes.retain(|_, q| {
            while q
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) >= WRITE_WINDOW)
            {
                q.pop_front();
            }
            !q.is_empty()
        });
        let held: BTreeSet<i64> = self
            .writes
            .iter()
            .filter(|(_, q)| q.len() >= WRITE_LIMIT)
            .map(|(r, _)| *r)
            .collect();
        let entered = held.difference(&self.held).copied().collect();
        let left = self.held.difference(&held).copied().collect();
        self.held = held;
        Held {
            roots: self.held.iter().copied().collect(),
            entered,
            left,
        }
    }

    pub fn record(&mut self, root: i64, now: Instant) {
        self.writes.entry(root).or_default().push_back(now);
    }
}

static WRITES: LazyLock<std::sync::Mutex<WriteLog>> = LazyLock::new(Default::default);

static THROTTLED: LazyLock<opentelemetry::metrics::Counter<u64>> = LazyLock::new(|| {
    global::meter("hub")
        .u64_counter("ccpm_plan_throttled")
        .build()
});

/// Recompute dirty scopes (no more than [`BATCH`] per pass). Returns how many plans were written.
pub async fn recompute_dirty(db: &PgPool, today: NaiveDate) -> anyhow::Result<usize> {
    recompute_dirty_with(db, today, &WRITES, BATCH).await
}

/// [`recompute_dirty`] with its own write log and pass size (tests).
pub async fn recompute_dirty_with(
    db: &PgPool,
    today: NaiveDate,
    log: &std::sync::Mutex<WriteLog>,
    batch: i64,
) -> anyhow::Result<usize> {
    let held = log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .held(Instant::now());
    for &root in &held.entered {
        THROTTLED.add(1, &[KeyValue::new("root", root)]);
        tracing::warn!(
            root,
            limit = WRITE_LIMIT,
            window_s = WRITE_WINDOW.as_secs(),
            "plan scope written too often, recompute postponed"
        );
    }
    for &root in &held.left {
        tracing::info!(root, "plan scope write window is over, recompute resumes");
    }
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "select root_seq, marked from plan.dirty where account = (select public.instance_account()) and marked > done \
         and root_seq <> all($2::bigint[]) order by since, root_seq limit $1",
    )
    .bind(batch)
    .bind(&held.roots)
    .fetch_all(db)
    .instrument(telemetry::db_span("select dirty scopes"))
    .await
    .context("select dirty scopes")?;
    let mut written = 0;
    for (root, marked) in rows {
        match recompute(db, root, marked, today).await {
            Ok(true) => {
                written += 1;
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record(root, Instant::now());
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(root, error = format!("{e:#}"), "plan recompute failed"),
        }
    }
    Ok(written)
}

/// One pass of the background task: the nightly mark by installation local time, then the queue.
pub async fn tick(db: &PgPool, last_nightly: &mut Option<NaiveDate>) -> anyhow::Result<usize> {
    let mut conn = db.acquire().await.context("acquire")?;
    let now = local_now(&mut conn).await?;
    if nightly_due(now, *last_nightly) {
        sqlx::query("select plan.mark_all_now(public.instance_account())")
            .execute(&mut *conn)
            .instrument(telemetry::db_span("mark all scopes"))
            .await
            .context("mark all scopes")?;
        *last_nightly = Some(now.date());
        tracing::info!(day = %now.date(), "all plan scopes marked dirty");
    }
    drop(conn);
    recompute_dirty(db, now.date()).await
}

/// Background recomputation every [`EVERY`] until the process ends.
pub fn spawn(db: PgPool) {
    tokio::spawn(async move {
        let mut last_nightly = None;
        let mut every = tokio::time::interval(EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if let Err(e) = tick(&db, &mut last_nightly).await {
                tracing::warn!(error = format!("{e:#}"), "plan tick failed");
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------------------------
// Routes

async fn seq_of(conn: &mut PgConnection, code: &str) -> Result<i64, AppError> {
    let row: Option<(i64,)> = sqlx::query_as("select seq from tickets where code = $1")
        .bind(code.trim().to_lowercase())
        .fetch_optional(&mut *conn)
        .instrument(telemetry::db_span("select ticket seq"))
        .await
        .context("select ticket seq")?;
    row.map(|r| r.0)
        .ok_or(AppError::Client(StatusCode::NOT_FOUND, "no such ticket"))
}

fn no_scope() -> AppError {
    json_err(StatusCode::NOT_FOUND, json!({"error": "no_scope"}))
}

fn fib(v: Option<i64>) -> bool {
    v.is_some_and(|v| FIB.contains(&v))
}

/// Estimate body. Missing fields are empty in the new version: PUT sets the whole estimate.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct EstimateIn {
    pub effort_sp: Option<i64>,
    pub aggressive_hd: Option<i64>,
    pub safe_hd: Option<i64>,
    pub remaining_hd: Option<i64>,
    pub start_not_before: Option<NaiveDate>,
}

impl EstimateIn {
    /// The same bounds as the CHECK in 0013_plan.sql.
    pub fn check(&self) -> Result<(), &'static str> {
        if !fib(self.effort_sp) {
            return Err("effort_sp: Fibonacci points 1, 2, 3, 5, 8, 13 or 21");
        }
        if self.aggressive_hd.is_some_and(|a| !(1..=1000).contains(&a)) {
            return Err("aggressive_hd: half-days, 1 to 1000");
        }
        if self
            .safe_hd
            .is_some_and(|s| !(1..=2000).contains(&s) || self.aggressive_hd.is_some_and(|a| s < a))
        {
            return Err("safe_hd: half-days, 1 to 2000, not below aggressive_hd");
        }
        if self.remaining_hd.is_some_and(|r| !(0..=1000).contains(&r)) {
            return Err("remaining_hd: half-days, 0 to 1000");
        }
        if self
            .start_not_before
            .is_some_and(|d| !(2000..=2100).contains(&chrono::Datelike::year(&d)))
        {
            return Err("start_not_before: a date between 2000 and 2100");
        }
        Ok(())
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub struct EstimateOut {
    pub code: String,
    pub effort_sp: i64,
    pub aggressive_hd: Option<i64>,
    pub safe_hd: Option<i64>,
    pub remaining_hd: Option<i64>,
    pub start_not_before: Option<NaiveDate>,
    pub valid_from: DateTime<Utc>,
    pub actor: String,
    pub actor_kind: String,
}

pub async fn put_estimate(
    State(db): State<PgPool>,
    actor: Actor,
    Path(code): Path<String>,
    Json(e): Json<EstimateIn>,
) -> Result<Json<EstimateOut>, AppError> {
    e.check().map_err(bad)?;
    let mut tx = tx::begin(&db, &actor).await?;
    let seq = seq_of(&mut tx, &code).await?;
    let out: EstimateOut = sqlx::query_as(
        "insert into plan.ticket_estimate (ticket_seq, effort_sp, aggressive_hd, safe_hd, remaining_hd, \
         start_not_before) values ($1, $2::int, $3::int, $4::int, $5::int, $6) \
         returning (select code from tickets where seq = $1) as code, effort_sp::int8 as effort_sp, \
         aggressive_hd::int8 as aggressive_hd, safe_hd::int8 as safe_hd, remaining_hd::int8 as remaining_hd, \
         start_not_before, valid_from, actor, actor_kind",
    )
    .bind(seq)
    .bind(e.effort_sp)
    .bind(e.aggressive_hd)
    .bind(e.safe_hd)
    .bind(e.remaining_hd)
    .bind(e.start_not_before)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("insert estimate"))
    .await
    .map_err(|e| db_error(e, "insert estimate"))?;
    tx.commit().await.context("commit estimate")?;
    Ok(Json(out))
}

/// Cost of delay terms, each a Fibonacci number.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct WsjfBody {
    pub business_value: Option<i64>,
    pub time_criticality: Option<i64>,
    pub risk_opportunity: Option<i64>,
}

impl WsjfBody {
    pub fn check(&self) -> Result<(), &'static str> {
        if [
            self.business_value,
            self.time_criticality,
            self.risk_opportunity,
        ]
        .into_iter()
        .all(fib)
        {
            Ok(())
        } else {
            Err(
                "business_value, time_criticality, risk_opportunity: Fibonacci points 1, 2, 3, 5, 8, 13 or 21",
            )
        }
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub struct WsjfOut {
    pub code: String,
    pub business_value: i64,
    pub time_criticality: i64,
    pub risk_opportunity: i64,
    /// Cost of delay: the sum of the three terms.
    pub cost_of_delay: i64,
    /// Effort of the live estimate.
    pub effort_sp: Option<i64>,
    /// WSJF in hundredths: `cost_of_delay * 100 / effort_sp`; empty without an estimate.
    pub wsjf: Option<i64>,
    pub valid_from: DateTime<Utc>,
    pub actor: String,
    pub actor_kind: String,
}

pub async fn put_wsjf(
    State(db): State<PgPool>,
    actor: Actor,
    Path(code): Path<String>,
    Json(w): Json<WsjfBody>,
) -> Result<Json<WsjfOut>, AppError> {
    w.check().map_err(bad)?;
    let mut tx = tx::begin(&db, &actor).await?;
    let seq = seq_of(&mut tx, &code).await?;
    let out: WsjfOut = sqlx::query_as(
        "with w as (insert into plan.ticket_wsjf (ticket_seq, business_value, time_criticality, risk_opportunity) \
         values ($1, $2::int, $3::int, $4::int) returning *) \
         select t.code, w.business_value::int8 as business_value, w.time_criticality::int8 as time_criticality, \
         w.risk_opportunity::int8 as risk_opportunity, \
         (w.business_value + w.time_criticality + w.risk_opportunity)::int8 as cost_of_delay, \
         pe.effort_sp::int8 as effort_sp, \
         ((w.business_value + w.time_criticality + w.risk_opportunity) * 100 / pe.effort_sp)::int8 as wsjf, \
         w.valid_from, w.actor, w.actor_kind \
         from w join tickets t on t.seq = w.ticket_seq \
         left join plan.ticket_estimate pe on pe.account = (select public.instance_account()) and pe.ticket_seq = w.ticket_seq \
          and pe.valid_to is null",
    )
    .bind(seq)
    .bind(w.business_value)
    .bind(w.time_criticality)
    .bind(w.risk_opportunity)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("insert wsjf"))
    .await
    .map_err(|e| db_error(e, "insert wsjf"))?;
    tx.commit().await.context("commit wsjf")?;
    Ok(Json(out))
}

/// The ticket's live estimate and WSJF: the ticket page forms open with the current values.
pub async fn get_ticket_plan(
    State(db): State<PgPool>,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let mut conn = db.acquire().await.context("acquire")?;
    let seq = seq_of(&mut conn, &code).await?;
    let estimate: Option<EstimateOut> = sqlx::query_as(
        "select t.code, e.effort_sp::int8 as effort_sp, e.aggressive_hd::int8 as aggressive_hd, \
         e.safe_hd::int8 as safe_hd, e.remaining_hd::int8 as remaining_hd, e.start_not_before, e.valid_from, \
         e.actor, e.actor_kind from plan.ticket_estimate e join tickets t on t.seq = e.ticket_seq \
         where e.account = (select public.instance_account()) and e.ticket_seq = $1 and e.valid_to is null",
    )
    .bind(seq)
    .fetch_optional(&mut *conn)
    .instrument(telemetry::db_span("select estimate"))
    .await
    .context("select estimate")?;
    let wsjf: Option<WsjfOut> = sqlx::query_as(
        "select t.code, w.business_value::int8 as business_value, w.time_criticality::int8 as time_criticality, \
         w.risk_opportunity::int8 as risk_opportunity, \
         (w.business_value + w.time_criticality + w.risk_opportunity)::int8 as cost_of_delay, \
         pe.effort_sp::int8 as effort_sp, \
         ((w.business_value + w.time_criticality + w.risk_opportunity) * 100 / pe.effort_sp)::int8 as wsjf, \
         w.valid_from, w.actor, w.actor_kind \
         from plan.ticket_wsjf w join tickets t on t.seq = w.ticket_seq \
         left join plan.ticket_estimate pe on pe.account = (select public.instance_account()) and pe.ticket_seq = w.ticket_seq \
          and pe.valid_to is null \
         where w.account = (select public.instance_account()) and w.ticket_seq = $1 and w.valid_to is null",
    )
    .bind(seq)
    .fetch_optional(&mut *conn)
    .instrument(telemetry::db_span("select wsjf"))
    .await
    .context("select wsjf")?;
    Ok(Json(json!({"estimate": estimate, "wsjf": wsjf})))
}

/// Distinguishes a `null` field (`Some(None)`) from a missing field (`None`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Scope settings. Missing fields come from the base: defaults in POST, the live version in PUT
/// (PUT changes only the named fields). `due_date: null` clears the due date.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ScopeIn {
    /// Root code; POST only.
    pub root: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub due_date: Option<Option<NaiveDate>>,
    pub mode: Option<String>,
    pub buffer_method: Option<String>,
    pub green_a_pct: Option<i64>,
    pub red_a_pct: Option<i64>,
    pub zone_slope_pct: Option<i64>,
    pub max_defaulted_pct: Option<i64>,
}

#[derive(Debug, PartialEq)]
pub struct ScopeValues {
    pub due_date: Option<NaiveDate>,
    pub mode: &'static str,
    pub buffer_method: &'static str,
    pub green_a_pct: i64,
    pub red_a_pct: i64,
    pub zone_slope_pct: i64,
    pub max_defaulted_pct: i64,
}

impl ScopeValues {
    /// Defaults from [`PlanConfig::default`] (the same as the DEFAULTs of the plan.scope columns).
    pub fn defaults() -> Self {
        let d = PlanConfig::default();
        ScopeValues {
            due_date: None,
            mode: "forward",
            buffer_method: "cnp",
            green_a_pct: d.green_a_pct,
            red_a_pct: d.red_a_pct,
            zone_slope_pct: d.zone_slope_pct,
            max_defaulted_pct: d.max_defaulted_pct,
        }
    }

    /// Values of the live scope version.
    pub fn of(s: &ScopeRow) -> Self {
        ScopeValues {
            due_date: s.due_date,
            mode: if s.mode == "backward" {
                "backward"
            } else {
                "forward"
            },
            buffer_method: if s.buffer_method == "ssq" {
                "ssq"
            } else {
                "cnp"
            },
            green_a_pct: s.green_a_pct,
            red_a_pct: s.red_a_pct,
            zone_slope_pct: s.zone_slope_pct,
            max_defaulted_pct: s.max_defaulted_pct,
        }
    }
}

impl ScopeIn {
    /// Values on top of the base `d` with the same bounds as the CHECK in 0013_plan.sql.
    pub fn values(&self, d: &ScopeValues) -> Result<ScopeValues, &'static str> {
        let mode = match self.mode.as_deref() {
            None => d.mode,
            Some("forward") => "forward",
            Some("backward") => "backward",
            Some(_) => return Err("mode: forward or backward"),
        };
        let buffer_method = match self.buffer_method.as_deref() {
            None => d.buffer_method,
            Some("cnp") => "cnp",
            Some("ssq") => "ssq",
            Some(_) => return Err("buffer_method: cnp or ssq"),
        };
        let v = ScopeValues {
            due_date: self.due_date.unwrap_or(d.due_date),
            mode,
            buffer_method,
            green_a_pct: self.green_a_pct.unwrap_or(d.green_a_pct),
            red_a_pct: self.red_a_pct.unwrap_or(d.red_a_pct),
            zone_slope_pct: self.zone_slope_pct.unwrap_or(d.zone_slope_pct),
            max_defaulted_pct: self.max_defaulted_pct.unwrap_or(d.max_defaulted_pct),
        };
        if !(0 <= v.green_a_pct && v.green_a_pct <= v.red_a_pct && v.red_a_pct <= 1000) {
            return Err("green_a_pct, red_a_pct: 0 <= green <= red <= 1000");
        }
        if !(0..=1000).contains(&v.zone_slope_pct) {
            return Err("zone_slope_pct: 0 to 1000");
        }
        if !(0..=100).contains(&v.max_defaulted_pct) {
            return Err("max_defaulted_pct: 0 to 100");
        }
        if v.mode == "backward" && v.due_date.is_none() {
            return Err("mode backward needs due_date");
        }
        if v.due_date
            .is_some_and(|d| !(2000..=2100).contains(&chrono::Datelike::year(&d)))
        {
            return Err("due_date: a date between 2000 and 2100");
        }
        Ok(v)
    }
}

async fn insert_scope(
    conn: &mut PgConnection,
    root: i64,
    v: &ScopeValues,
) -> Result<ScopeRow, AppError> {
    sqlx::query(
        "insert into plan.scope (root_seq, due_date, mode, buffer_method, green_a_pct, red_a_pct, zone_slope_pct, \
         max_defaulted_pct) values ($1, $2, $3, $4, $5::int, $6::int, $7::int, $8::int)",
    )
    .bind(root)
    .bind(v.due_date)
    .bind(v.mode)
    .bind(v.buffer_method)
    .bind(v.green_a_pct)
    .bind(v.red_a_pct)
    .bind(v.zone_slope_pct)
    .bind(v.max_defaulted_pct)
    .execute(&mut *conn)
    .instrument(telemetry::db_span("insert scope"))
    .await
    .map_err(|e| db_error(e, "insert scope"))?;
    Ok(live_scope(conn, root)
        .await?
        .context("scope after insert")?)
}

pub async fn create_scope(
    State(db): State<PgPool>,
    actor: Actor,
    Json(s): Json<ScopeIn>,
) -> Result<(StatusCode, Json<ScopeRow>), AppError> {
    let v = s.values(&ScopeValues::defaults()).map_err(bad)?;
    let Some(root) = s.root.as_deref().map(str::trim).filter(|r| !r.is_empty()) else {
        return Err(bad("root: code of the umbrella ticket"));
    };
    let mut tx = tx::begin(&db, &actor).await?;
    let seq = seq_of(&mut tx, root).await?;
    if live_scope(&mut tx, seq).await?.is_some() {
        return Err(json_err(
            StatusCode::CONFLICT,
            json!({"error": "scope_exists"}),
        ));
    }
    let out = insert_scope(&mut tx, seq, &v).await?;
    tx.commit().await.context("commit scope")?;
    Ok((StatusCode::CREATED, Json(out)))
}

pub async fn update_scope(
    State(db): State<PgPool>,
    actor: Actor,
    Path(root): Path<String>,
    Json(s): Json<ScopeIn>,
) -> Result<Json<ScopeRow>, AppError> {
    if s.root.is_some() {
        return Err(bad("root goes in the path"));
    }
    let mut tx = tx::begin(&db, &actor).await?;
    let seq = seq_of(&mut tx, &root).await?;
    let Some(live) = live_scope(&mut tx, seq).await? else {
        return Err(no_scope());
    };
    let v = s.values(&ScopeValues::of(&live)).map_err(bad)?;
    let out = insert_scope(&mut tx, seq, &v).await?;
    tx.commit().await.context("commit scope")?;
    Ok(Json(out))
}

/// A scope in the list: settings and a summary of the last computation.
#[derive(Serialize, sqlx::FromRow)]
pub struct ScopeSummary {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub scope: ScopeRow,
    pub computed_at: Option<DateTime<Utc>>,
    /// Projected due date (end of PB), a working half-day.
    pub plan_end: Option<i64>,
    /// Root WSJF in hundredths.
    pub wsjf_root: Option<i64>,
    /// The queue has an unprocessed mark.
    pub pending: bool,
    pub baseline_id: Option<i64>,
}

const SCOPE_SUMMARY: &str = "select s.id, s.root_seq, t.code as root, t.summary, s.due_date, s.mode, \
    s.buffer_method, s.green_a_pct::int8 as green_a_pct, s.red_a_pct::int8 as red_a_pct, \
    s.zone_slope_pct::int8 as zone_slope_pct, s.max_defaulted_pct::int8 as max_defaulted_pct, s.valid_from, \
    s.actor, s.actor_kind, c.computed_at, (c.result ->> 'end')::int8 as plan_end, \
    (c.result ->> 'wsjf_root')::int8 as wsjf_root, coalesce(d.marked > d.done, false) as pending, \
    b.id as baseline_id \
    from plan.scope s join tickets t on t.seq = s.root_seq \
    left join plan.current c on c.account = s.account and c.root_seq = s.root_seq \
    left join plan.dirty d on d.account = s.account and d.root_seq = s.root_seq \
    left join plan.baseline_live b on b.account = s.account and b.root_seq = s.root_seq \
    where s.account = (select public.instance_account()) and s.valid_to is null and ($1::text is null or t.project = $1)";

pub async fn list_scopes(State(db): State<PgPool>) -> Result<Json<Vec<ScopeSummary>>, AppError> {
    Ok(Json(scope_summaries(&db, None, "s.root_seq").await?))
}

async fn scope_summaries(
    db: &PgPool,
    project: Option<&str>,
    order: &'static str,
) -> Result<Vec<ScopeSummary>, AppError> {
    Ok(sqlx::query_as(&format!("{SCOPE_SUMMARY} order by {order}"))
        .bind(project)
        .fetch_all(db)
        .instrument(telemetry::db_span("select scopes"))
        .await
        .context("select scopes")?)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BaselineInfo {
    pub id: i64,
    pub approved_at: DateTime<Utc>,
    pub approved_by: String,
    pub approved_kind: String,
    pub accepted_defaults: bool,
    pub defaulted_pct: i64,
    pub max_defaulted_pct: i64,
}

const BASELINE_INFO: &str = "select id, approved_at, approved_by, approved_kind, accepted_defaults, \
    defaulted_pct::int8 as defaulted_pct, max_defaulted_pct::int8 as max_defaulted_pct from plan.baseline_live \
    where account = (select public.instance_account()) and root_seq = $1";

/// Plan calendar for clients: half-day `n` starts on the day `date_of(n)`.
fn calendar() -> Value {
    json!({"unit": "half_day", "epoch": "2024-01-01", "workdays": "mon-fri"})
}

pub async fn get_plan(
    State(db): State<PgPool>,
    Path(root): Path<String>,
) -> Result<Json<Value>, AppError> {
    let mut conn = db.acquire().await.context("acquire")?;
    let seq = seq_of(&mut conn, &root).await?;
    let Some(scope) = live_scope(&mut conn, seq).await? else {
        return Err(no_scope());
    };
    let stored: Option<(DateTime<Utc>, NaiveDate, Value)> = sqlx::query_as(
        "select computed_at, today, result from plan.current \
         where account = (select public.instance_account()) and root_seq = $1 and algo_version = $2",
    )
    .bind(seq)
    .bind(CCPM_ALGO_VERSION as i32)
    .fetch_optional(&mut *conn)
    .instrument(telemetry::db_span("select current plan"))
    .await
    .context("select current plan")?;
    let (pending,): (bool,) = sqlx::query_as(
        "select coalesce((select marked > done from plan.dirty where account = (select public.instance_account()) and root_seq = $1), false)",
    )
    .bind(seq)
    .fetch_one(&mut *conn)
    .await
    .context("select plan dirty")?;
    let baseline: Option<BaselineInfo> = sqlx::query_as(BASELINE_INFO)
        .bind(seq)
        .fetch_optional(&mut *conn)
        .instrument(telemetry::db_span("select baseline"))
        .await
        .context("select baseline")?;
    // Before the first background computation (a new scope, a new algorithm version) the plan is computed on the fly and not written.
    let (source, computed_at, today, plan) = match stored {
        Some((at, today, plan)) => ("stored", Some(at), today, plan),
        None => {
            let today = local_now(&mut conn).await?.date();
            let l = load(&mut conn, seq).await?.ok_or_else(no_scope)?;
            (
                "live",
                None,
                today,
                serde_json::to_value(caught(&l.scope.root, || compute(&l, today))?)?,
            )
        }
    };
    let end_date = plan["end"].as_i64().map(date_of);
    Ok(Json(json!({
        "root": scope.root,
        "scope": scope,
        "source": source,
        "computed_at": computed_at,
        "today": today,
        "end_date": end_date,
        "pending": pending,
        "baseline": baseline,
        "calendar": calendar(),
        "plan": plan,
    })))
}

/// A fever point: one per day per buffer (the last computation of the day).
#[derive(Serialize, sqlx::FromRow)]
pub struct FeverPoint {
    pub day: NaiveDate,
    /// `pb`, `fb:<seq>` or `fb:finish`.
    pub buffer_key: String,
    pub kind: String,
    pub baseline_id: i64,
    pub chain_complete_pct: i64,
    pub consumed_pct: i64,
    pub zone: Option<String>,
    pub flags: Vec<String>,
}

/// Fever points of a scope by day and buffer, of all baselines (each point has its baseline); empty until a baseline is approved.
pub async fn get_fever(
    State(db): State<PgPool>,
    Path(root): Path<String>,
) -> Result<Json<Value>, AppError> {
    let mut conn = db.acquire().await.context("acquire")?;
    let seq = seq_of(&mut conn, &root).await?;
    let Some(scope) = live_scope(&mut conn, seq).await? else {
        return Err(no_scope());
    };
    let baseline: Option<BaselineInfo> = sqlx::query_as(BASELINE_INFO)
        .bind(seq)
        .fetch_optional(&mut *conn)
        .instrument(telemetry::db_span("select baseline"))
        .await
        .context("select baseline")?;
    let points: Vec<FeverPoint> = sqlx::query_as(
        "select day, buffer_key, kind, baseline_id, chain_complete_pct::int8 as chain_complete_pct, \
         consumed_pct::int8 as consumed_pct, zone, flags from plan.fever_point \
         where account = (select public.instance_account()) and root_seq = $1 order by day, buffer_key",
    )
    .bind(seq)
    .fetch_all(&mut *conn)
    .instrument(telemetry::db_span("select fever points"))
    .await
    .context("select fever points")?;
    Ok(Json(json!({
        "root": scope.root,
        "baseline": baseline,
        "points": points,
    })))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ApproveIn {
    /// Approve even though the share of the critical chain with default estimates is above the scope threshold (recorded in the baseline).
    #[serde(default)]
    pub accept_defaults: bool,
}

/// Baseline approval: the plan is recomputed without the previous baseline (chain and buffers by the current longest chain) and
/// written as a new row that replaces the live one. Human only (hub-web form); the database checks the same.
pub async fn approve(
    State(db): State<PgPool>,
    actor: Actor,
    Path(root): Path<String>,
    body: Option<Json<ApproveIn>>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    if !actor.is_human() {
        return Err(json_err(
            StatusCode::FORBIDDEN,
            json!({"error": "approve_requires_human"}),
        ));
    }
    let accept = body.is_some_and(|b| b.accept_defaults);
    let mut tx = tx::begin(&db, &actor).await?;
    let seq = seq_of(&mut tx, &root).await?;
    let today = local_now(&mut tx).await?.date();
    let Some(l) = load(&mut tx, seq).await? else {
        return Err(no_scope());
    };
    let plan = caught(&l.scope.root, || {
        ccpm::plan(&l.input, None, half_day_of(today), &l.cfg)
    })?;
    if plan.chain.is_empty() {
        return Err(json_err(
            StatusCode::CONFLICT,
            json!({"error": "nothing_to_approve"}),
        ));
    }
    if !ccpm::fever::can_approve(&plan, accept) {
        return Err(json_err(
            StatusCode::CONFLICT,
            json!({"error": "defaults_over_threshold", "defaulted_pct": plan.defaulted_pct,
                   "max_defaulted_pct": l.cfg.max_defaulted_pct}),
        ));
    }
    let base = ccpm::fever::baseline(&plan);
    let info: BaselineInfo = sqlx::query_as(
        "insert into plan.baseline (root_seq, supersedes, scope_id, accepted_defaults, defaulted_pct, algo_version, \
         result) values ($1, $2, $3, $4, $5::int, $6, $7) returning id, approved_at, approved_by, approved_kind, \
         accepted_defaults, defaulted_pct::int8 as defaulted_pct, max_defaulted_pct::int8 as max_defaulted_pct",
    )
    .bind(seq)
    .bind(l.baseline.as_ref().map(|b| b.0))
    .bind(l.scope.id)
    .bind(accept && !plan.approvable)
    .bind(plan.defaulted_pct.clamp(0, 100))
    .bind(CCPM_ALGO_VERSION as i32)
    .bind(serde_json::to_value(&base)?)
    .fetch_one(&mut *tx)
    .instrument(telemetry::db_span("insert baseline"))
    .await
    .map_err(|e| db_error(e, "insert baseline"))?;
    tx.commit().await.context("commit baseline")?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"root": l.scope.root, "baseline": info, "result": base})),
    ))
}

#[derive(Deserialize)]
pub struct NextQuery {
    pub person: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct WarningOut {
    pub code: String,
    pub after: String,
    pub reason: String,
    /// Code of the scope root.
    pub scope: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct NextOut {
    pub person: String,
    pub code: Option<String>,
    /// Code of the task scope's root (empty outside scopes).
    pub scope: Option<String>,
    pub flags: Flags,
    /// Resource buffer warnings for this person (rule 12).
    pub warnings: Vec<WarningOut>,
}

/// A person's task row outside plans: readiness over the whole graph (with `start_not_before`) and WSJF.
#[derive(sqlx::FromRow)]
struct OutsideRow {
    seq: i64,
    code: String,
    status: String,
    ready: bool,
    wsjf: Option<i64>,
}

/// Current plans of all live scopes (current algorithm version only).
async fn current_plans(conn: &mut PgConnection) -> anyhow::Result<Vec<Plan>> {
    let rows: Vec<(i64, Value)> = sqlx::query_as(
        "select c.root_seq, c.result from plan.current c join plan.scope s on s.account = c.account \
         and s.root_seq = c.root_seq and s.valid_to is null where c.account = (select public.instance_account()) and c.algo_version = $1 \
         order by c.root_seq",
    )
    .bind(CCPM_ALGO_VERSION as i32)
    .fetch_all(&mut *conn)
    .instrument(telemetry::db_span("select current plans"))
    .await
    .context("select current plans")?;
    let mut out = Vec::new();
    for (root, v) in rows {
        match serde_json::from_value::<Plan>(v) {
            Ok(p) => out.push(p),
            Err(e) => tracing::warn!(root, error = %e, "plan.current does not parse; skipped"),
        }
    }
    Ok(out)
}

/// Next task: doing or the best ready one by buffer zone and WSJF (ccpm::next) across plans and tasks outside them.
pub fn next_for(person: &str, plans: &[Plan], outside: Vec<OutsideTask>) -> NextOut {
    let in_plans: BTreeSet<i64> = plans
        .iter()
        .flat_map(|p| p.tasks.iter().filter(|t| t.a.is_some()).map(|t| t.seq))
        .collect();
    let outside: Vec<OutsideTask> = outside
        .into_iter()
        .filter(|t| !in_plans.contains(&t.seq))
        .collect();
    let n = ccpm::next::next_task(person, plans, &outside);
    let root_code = |root: i64| {
        plans
            .iter()
            .find(|p| p.root == root)
            .and_then(|p| p.task(root))
            .map(|t| t.code.clone())
            .unwrap_or_else(|| root.to_string())
    };
    let code_in = |p: &Plan, seq: i64| {
        p.task(seq)
            .map_or_else(|| seq.to_string(), |t| t.code.clone())
    };
    let warnings = plans
        .iter()
        .flat_map(|p| {
            p.resource_warnings
                .iter()
                .filter(|w| w.assignee == person)
                .map(move |w| (p, w))
        })
        .map(|(p, w)| WarningOut {
            code: code_in(p, w.seq),
            after: code_in(p, w.after),
            reason: w.reason.clone(),
            scope: root_code(p.root),
        })
        .collect();
    NextOut {
        person: person.to_owned(),
        code: n.code,
        scope: n.scope.map(root_code),
        flags: n.flags,
        warnings,
    }
}

pub async fn next(
    State(db): State<PgPool>,
    Query(q): Query<NextQuery>,
) -> Result<Json<NextOut>, AppError> {
    let person = q.person.as_deref().map(str::trim).unwrap_or("");
    if !people::valid_handle(person) {
        return Err(bad("person: handle of a person (e.g. alice)"));
    }
    let mut conn = db.acquire().await.context("acquire")?;
    let (known,): (bool,) = sqlx::query_as(
        "select exists (select 1 from graph.person where account = (select public.instance_account()) and handle = $1)",
    )
    .bind(person)
    .fetch_one(&mut *conn)
    .await
    .context("select person")?;
    if !known {
        return Err(json_err(
            StatusCode::NOT_FOUND,
            json!({"error": "unknown_person"}),
        ));
    }
    let plans = current_plans(&mut conn).await?;
    let rows: Vec<OutsideRow> = sqlx::query_as(&format!(
        "select tickets.seq, tickets.code, tickets.status, (true{ready}) as ready, \
         ((xw.business_value + xw.time_criticality + xw.risk_opportunity) * 100 / xe.effort_sp)::int8 as wsjf \
         from tickets \
         join graph.ticket xt on xt.ticket_seq = tickets.seq \
         join graph.edge xa on xa.src = xt.node_id and xa.rel = 'assigned_to' and xa.state = 'confirmed' \
          and xa.valid_to is null \
         join graph.person xp on xp.node_id = xa.dst and xp.handle = $1 \
         left join plan.ticket_estimate xe on xe.account = (select public.instance_account()) and xe.ticket_seq = tickets.seq \
          and xe.valid_to is null \
         left join plan.ticket_wsjf xw on xw.account = (select public.instance_account()) and xw.ticket_seq = tickets.seq \
          and xw.valid_to is null \
         where tickets.status in ('open', 'doing') order by tickets.seq",
        ready = query::READY,
    ))
    .bind(person)
    .fetch_all(&mut *conn)
    .instrument(telemetry::db_span("select person tasks"))
    .await
    .context("select person tasks")?;
    let outside = rows
        .into_iter()
        .map(|r| OutsideTask {
            seq: r.seq,
            code: r.code,
            assignee: person.to_owned(),
            status: status_of(&r.status),
            ready: r.ready,
            wsjf: r.wsjf,
        })
        .collect();
    Ok(Json(next_for(person, &plans, outside)))
}

#[derive(Deserialize)]
pub struct BacklogQuery {
    pub project: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct BacklogItem {
    pub code: String,
    pub project: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
    pub effort_sp: Option<i64>,
    pub cost_of_delay: Option<i64>,
    /// WSJF in hundredths; empty without an estimate or WSJF, such tickets go last.
    pub wsjf: Option<i64>,
    #[serde(skip)]
    pub total: i64,
}

/// Backlog: open and doing leaves (without live part_of subtasks) by WSJF descending, then seq; scopes by root WSJF.
pub async fn backlog(
    State(db): State<PgPool>,
    Query(q): Query<BacklogQuery>,
) -> Result<Json<Value>, AppError> {
    let project = q
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if project.is_some_and(|p| !crate::valid_project(p)) {
        return Err(bad("bad project name"));
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(bad("limit: 1 to 500"));
    }
    let items: Vec<BacklogItem> = sqlx::query_as(
        "select tickets.code, tickets.project, tickets.summary, tickets.status, tickets.priority, \
         pe.effort_sp::int8 as effort_sp, \
         (pw.business_value + pw.time_criticality + pw.risk_opportunity)::int8 as cost_of_delay, \
         ((pw.business_value + pw.time_criticality + pw.risk_opportunity) * 100 / pe.effort_sp)::int8 as wsjf, \
         count(*) over () as total \
         from tickets \
         left join plan.ticket_estimate pe on pe.account = (select public.instance_account()) and pe.ticket_seq = tickets.seq \
          and pe.valid_to is null \
         left join plan.ticket_wsjf pw on pw.account = (select public.instance_account()) and pw.ticket_seq = tickets.seq \
          and pw.valid_to is null \
         where tickets.status in ('open', 'doing') and ($1::text is null or tickets.project = $1) \
          and not exists (select 1 from graph.ticket ct join graph.edge ce on ce.dst = ct.node_id \
           and ce.rel = 'part_of' and ce.state = 'confirmed' and ce.valid_to is null \
           where ct.ticket_seq = tickets.seq) \
         order by wsjf desc nulls last, tickets.seq limit $2",
    )
    .bind(project)
    .bind(limit)
    .fetch_all(&db)
    .instrument(telemetry::db_span("select backlog"))
    .await
    .context("select backlog")?;
    let total = items.first().map_or(0, |i| i.total);
    let scopes = scope_summaries(&db, project, "wsjf_root desc nulls last, s.root_seq").await?;
    Ok(Json(json!({
        "total": total,
        "items": items,
        "scopes": scopes,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: i64, code: &str) -> TaskRow {
        TaskRow {
            seq,
            code: code.into(),
            status: "open".into(),
            ..TaskRow::default()
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn input_is_canonical_and_keeps_only_edges_into_the_scope() {
        let mut root = row(10, "r");
        root.part_of = Some(99); // parent outside the scope
        let mut a = row(1, "a");
        a.part_of = Some(10);
        a.tags = vec!["feature".into(), "bug".into(), "bug".into()];
        a.priority = Some("p1".into());
        a.assignee = Some("alice".into());
        a.effort_sp = Some(3);
        a.start_not_before = Some(d(2030, 1, 7));
        a.business_value = Some(5);
        a.time_criticality = Some(3);
        a.risk_opportunity = Some(2);
        let mut b = row(2, "b");
        b.part_of = Some(10);
        b.status = "done".into();
        b.done_on = Some(d(2030, 1, 4));
        b.business_value = Some(5); // no WSJF without the other two terms
        let mut c = row(3, "c");
        c.status = "cancelled".into();
        c.done_on = Some(d(2030, 1, 4)); // done_at only for done
        let edges = [
            EdgeRow { src: 2, dst: 1 },
            EdgeRow { src: 50, dst: 1 }, // external blocker
            EdgeRow { src: 51, dst: 1 }, // external, but there is no row: the edge drops out
            EdgeRow { src: 1, dst: 60 }, // end outside the scope
            EdgeRow { src: 1, dst: 1 },
        ];
        let ext = vec![
            ExternalRow {
                seq: 60,
                code: "x".into(),
                status: "open".into(),
                forecast_end: None,
            },
            ExternalRow {
                seq: 50,
                code: "y".into(),
                status: "doing".into(),
                forecast_end: Some(42),
            },
        ];
        let input = build_input(10, vec![root, c, b.clone(), a.clone()], &edges, ext.clone());
        assert_eq!(input.root, 10);
        assert_eq!(
            input.tasks.iter().map(|t| t.seq).collect::<Vec<_>>(),
            [1, 2, 3, 10]
        );
        let ta = &input.tasks[0];
        assert_eq!(ta.tags, ["bug", "feature"]);
        assert_eq!(ta.priority, Some(1));
        assert_eq!(ta.part_of, Some(10));
        let e = ta.estimate.as_ref().unwrap();
        assert_eq!(e.effort_sp, 3);
        assert_eq!(e.start_not_before, Some(half_day_of(d(2030, 1, 7))));
        assert_eq!(ccpm::wsjf::task(ta), Some(333));
        assert_eq!(input.tasks[1].wsjf, None);
        assert_eq!(input.tasks[1].status, Status::Done);
        assert_eq!(input.tasks[1].done_at, Some(half_day_of(d(2030, 1, 4))));
        assert_eq!(input.tasks[2].done_at, None);
        assert_eq!(input.tasks[3].part_of, None, "the root's parent is outside");
        assert_eq!(
            input.blocks,
            [BlockEdge { src: 2, dst: 1 }, BlockEdge { src: 50, dst: 1 }]
        );
        assert_eq!(input.external.len(), 1);
        assert_eq!(input.external[0].seq, 50);
        assert_eq!(input.external[0].status, Status::Doing);
        assert_eq!(input.external[0].forecast_end, Some(42));

        // The order of database rows changes neither the input nor its hash.
        let mut shuffled_edges = edges;
        shuffled_edges.reverse();
        let mut ext2 = ext;
        ext2.reverse();
        let mut root2 = row(10, "r");
        root2.part_of = Some(77);
        let mut c2 = row(3, "c");
        c2.status = "cancelled".into();
        let again = build_input(10, vec![a, b, root2, c2], &shuffled_edges, ext2);
        assert_eq!(again, input);
        let cfg = PlanConfig::default();
        assert_eq!(
            input_hash(&input, 5, &cfg, None),
            input_hash(&again, 5, &cfg, None)
        );
        assert_ne!(
            input_hash(&input, 5, &cfg, None),
            input_hash(&input, 6, &cfg, None)
        );
        assert_ne!(
            input_hash(&input, 5, &cfg, None),
            input_hash(&input, 5, &cfg, Some(1))
        );
        let mut cfg2 = cfg.clone();
        cfg2.other_default_sp = 5;
        assert_ne!(
            input_hash(&input, 5, &cfg, None),
            input_hash(&input, 5, &cfg2, None)
        );
    }

    #[test]
    fn config_from_scope_and_dictionaries() {
        let s = ScopeRow {
            id: 1,
            root_seq: 10,
            root: "r".into(),
            summary: "r".into(),
            due_date: Some(d(2030, 1, 4)), // Friday
            mode: "backward".into(),
            buffer_method: "ssq".into(),
            green_a_pct: 5,
            red_a_pct: 25,
            zone_slope_pct: 50,
            max_defaulted_pct: 10,
            valid_from: Utc::now(),
            actor: "a".into(),
            actor_kind: "human".into(),
        };
        let c = config(
            &s,
            &[("bug".into(), 2), ("*".into(), 8), ("feature".into(), 5)],
            &[(3, 4)],
        );
        assert_eq!(c.mode, Mode::Backward);
        assert_eq!(c.buffer_method, BufferMethod::Ssq);
        // End of Friday = the first half-day of Monday.
        assert_eq!(c.due, Some(half_day_of(d(2030, 1, 7))));
        assert_eq!(c.other_default_sp, 8);
        assert_eq!(c.type_default_sp.len(), 2);
        assert_eq!(c.points_hd(3), 4);
        assert_eq!(c.points_hd(5), 5);
        assert_eq!(
            (
                c.green_a_pct,
                c.red_a_pct,
                c.zone_slope_pct,
                c.max_defaulted_pct
            ),
            (5, 25, 50, 10)
        );
        // Without a '*' row anything else is 3 points (product decision).
        assert_eq!(config(&s, &[], &[]).other_default_sp, 3);
    }

    #[test]
    fn write_log_postpones_a_scope_written_too_often_once_and_forgets_after_the_window() {
        let t0 = Instant::now();
        let mut log = WriteLog::default();
        let none = Held {
            roots: vec![],
            entered: vec![],
            left: vec![],
        };
        for i in 0..WRITE_LIMIT {
            assert_eq!(log.held(t0), none, "write {i}");
            log.record(1, t0);
        }
        log.record(2, t0);
        assert_eq!(
            log.held(t0),
            Held {
                roots: vec![1],
                entered: vec![1],
                left: vec![],
            },
            "limit reached, other scopes are not affected"
        );
        // Next passes in the same window: the scope is deferred, but entry is named only once.
        for s in [1, 5, 599] {
            assert_eq!(
                log.held(t0 + Duration::from_secs(s)),
                Held {
                    roots: vec![1],
                    entered: vec![],
                    left: vec![],
                },
                "{s} s"
            );
        }
        assert_eq!(
            log.held(t0 + WRITE_WINDOW),
            Held {
                roots: vec![],
                entered: vec![],
                left: vec![1],
            },
            "the window is over"
        );
        assert!(
            log.writes.is_empty() && log.held.is_empty(),
            "old writes are forgotten"
        );
        // A new window: the limit again and again one entry.
        let t1 = t0 + WRITE_WINDOW;
        for _ in 0..WRITE_LIMIT {
            log.record(1, t1);
        }
        assert_eq!(log.held(t1).entered, vec![1]);
        assert_eq!(log.held(t1).entered, Vec::<i64>::new());
    }

    #[test]
    fn nightly_mark_at_five_past_midnight_once_a_day() {
        let at = |day: u32, h: u32, m: u32| d(2030, 1, day).and_hms_opt(h, m, 0).unwrap();
        assert!(nightly_due(at(4, 10, 0), None), "after start, once");
        assert!(!nightly_due(at(4, 10, 0), Some(d(2030, 1, 4))));
        assert!(!nightly_due(at(5, 0, 4), Some(d(2030, 1, 4))));
        assert!(nightly_due(at(5, 0, 5), Some(d(2030, 1, 4))));
        assert!(
            !nightly_due(at(5, 0, 3), None),
            "before 00:05 waits for 00:05"
        );
    }

    #[test]
    fn estimate_and_wsjf_bodies() {
        let ok = EstimateIn {
            effort_sp: Some(5),
            aggressive_hd: Some(4),
            safe_hd: Some(8),
            remaining_hd: Some(0),
            start_not_before: Some(d(2030, 1, 15)),
        };
        assert!(ok.check().is_ok());
        for bad in [
            EstimateIn::default(),
            EstimateIn {
                effort_sp: Some(4),
                ..EstimateIn::default()
            },
            EstimateIn {
                effort_sp: Some(3),
                aggressive_hd: Some(0),
                ..EstimateIn::default()
            },
            EstimateIn {
                effort_sp: Some(3),
                aggressive_hd: Some(4),
                safe_hd: Some(3),
                ..EstimateIn::default()
            },
            EstimateIn {
                effort_sp: Some(3),
                remaining_hd: Some(-1),
                ..EstimateIn::default()
            },
            EstimateIn {
                effort_sp: Some(3),
                start_not_before: Some(d(1999, 1, 1)),
                ..EstimateIn::default()
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
        assert!(
            WsjfBody {
                business_value: Some(21),
                time_criticality: Some(1),
                risk_opportunity: Some(13)
            }
            .check()
            .is_ok()
        );
        assert!(
            WsjfBody {
                business_value: Some(4),
                time_criticality: Some(1),
                risk_opportunity: Some(1)
            }
            .check()
            .is_err()
        );
        assert!(
            WsjfBody {
                business_value: Some(1),
                ..WsjfBody::default()
            }
            .check()
            .is_err()
        );
    }

    #[test]
    fn scope_values_default_and_bounds() {
        let base = ScopeValues::defaults();
        let v = ScopeIn::default().values(&base).unwrap();
        assert_eq!(
            v,
            ScopeValues {
                due_date: None,
                mode: "forward",
                buffer_method: "cnp",
                green_a_pct: 10,
                red_a_pct: 30,
                zone_slope_pct: 60,
                max_defaulted_pct: 20,
            }
        );
        for bad in [
            ScopeIn {
                mode: Some("sideways".into()),
                ..ScopeIn::default()
            },
            ScopeIn {
                mode: Some("backward".into()),
                ..ScopeIn::default()
            },
            ScopeIn {
                buffer_method: Some("x".into()),
                ..ScopeIn::default()
            },
            ScopeIn {
                green_a_pct: Some(40),
                ..ScopeIn::default()
            },
            ScopeIn {
                max_defaulted_pct: Some(101),
                ..ScopeIn::default()
            },
            ScopeIn {
                zone_slope_pct: Some(-1),
                ..ScopeIn::default()
            },
        ] {
            assert!(bad.values(&base).is_err(), "{bad:?}");
        }
        let backward = ScopeIn {
            mode: Some("backward".into()),
            due_date: Some(Some(d(2030, 1, 21))),
            ..ScopeIn::default()
        }
        .values(&base)
        .unwrap();
        // PUT changes only the named fields: thresholds and mode stay from the live version; null clears the due date.
        let live = ScopeValues {
            max_defaulted_pct: 35,
            ..backward
        };
        let moved: ScopeIn = serde_json::from_value(json!({"due_date": "2030-01-25"})).unwrap();
        assert_eq!(
            moved.values(&live).unwrap(),
            ScopeValues {
                due_date: Some(d(2030, 1, 25)),
                ..live
            }
        );
        let cleared: ScopeIn =
            serde_json::from_value(json!({"due_date": null, "mode": "forward"})).unwrap();
        let v = cleared.values(&live).unwrap();
        assert_eq!(
            (v.due_date, v.mode, v.max_defaulted_pct),
            (None, "forward", 35)
        );
        let no_due: ScopeIn = serde_json::from_value(json!({"due_date": null})).unwrap();
        assert!(no_due.values(&live).is_err(), "backward without due_date");
    }

    #[test]
    fn next_skips_outside_rows_that_a_plan_already_holds() {
        use crate::ccpm::model::TaskIn as T;
        let t = |seq: i64, code: &str| T {
            seq,
            code: code.into(),
            status: Status::Open,
            priority: None,
            tags: vec![],
            assignee: Some("alice".into()),
            estimate: Some(Estimate {
                effort_sp: 2,
                aggressive_hd: None,
                safe_hd: None,
                remaining_hd: None,
                start_not_before: None,
            }),
            wsjf: None,
            part_of: Some(10),
            duplicates: None,
            done_at: None,
        };
        let mut root = t(10, "r");
        root.part_of = None;
        root.assignee = None;
        let input = PlanInput {
            root: 10,
            tasks: vec![t(1, "a"), root],
            blocks: vec![],
            external: vec![],
        };
        let plan = ccpm::plan(&input, None, 0, &PlanConfig::default());
        // A row outside plans with the same seq and a larger WSJF does not override the plan task: it is already in it.
        let dup = OutsideTask {
            seq: 1,
            code: "a".into(),
            assignee: "alice".into(),
            status: Status::Open,
            ready: true,
            wsjf: Some(10_000),
        };
        let other = OutsideTask {
            seq: 7,
            code: "z".into(),
            assignee: "alice".into(),
            status: Status::Open,
            ready: true,
            wsjf: None,
        };
        let out = next_for("alice", std::slice::from_ref(&plan), vec![dup, other]);
        assert_eq!(out.code.as_deref(), Some("a"));
        assert_eq!(out.scope.as_deref(), Some("r"));
        let none = next_for("bob", &[plan], vec![]);
        assert_eq!(none.code, None);
    }
}
