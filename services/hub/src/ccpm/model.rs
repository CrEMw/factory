//! Input and output of the critical chain computation (spec "Critical chain", rules 1-14).
//!
//! Time: a working half-day (`HalfDay`), a Mon-Fri calendar, half-day number counted from Monday 2024-01-01. All
//! durations in whole half-days, all shares in whole percent, WSJF in hundredths; no f64.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Working half-day number from the calendar epoch.
pub type HalfDay = i64;
/// Global ticket number (`tickets.seq`).
pub type Seq = i64;

/// Task and plan flags: a set, so that the JSON order does not depend on the input order.
pub type Flags = BTreeSet<String>;

/// Calendar start: a Monday.
fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, 1).expect("epoch date")
}

/// First half-day of a working day. Saturday and Sunday give the first half-day of the next Monday.
pub fn half_day_of(date: NaiveDate) -> HalfDay {
    let days = (date - epoch()).num_days();
    let weeks = days.div_euclid(7);
    let weekday = days.rem_euclid(7).min(5);
    (weeks * 5 + weekday) * 2
}

/// Calendar day the half-day falls on.
pub fn date_of(hd: HalfDay) -> NaiveDate {
    let workday = hd.div_euclid(2);
    let days = workday.div_euclid(5) * 7 + workday.rem_euclid(5);
    epoch() + chrono::Duration::days(days)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Doing,
    Done,
    Cancelled,
}

impl Status {
    /// A finished blocker: its edges count as satisfied, the task does not go into the schedule.
    pub fn finished(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }
}

/// A live `plan.ticket_estimate` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    /// Effort in Fibonacci points.
    pub effort_sp: i64,
    #[serde(default)]
    pub aggressive_hd: Option<i64>,
    #[serde(default)]
    pub safe_hd: Option<i64>,
    #[serde(default)]
    pub remaining_hd: Option<i64>,
    #[serde(default)]
    pub start_not_before: Option<HalfDay>,
}

/// A live `plan.ticket_wsjf` row, every number a Fibonacci number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsjfIn {
    pub business_value: i64,
    pub time_criticality: i64,
    pub risk_opportunity: i64,
}

/// A scope ticket in any status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIn {
    pub seq: Seq,
    pub code: String,
    pub status: Status,
    /// Priority as a number: 0 = p0 (most urgent); empty goes last.
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Assignee from the `assigned_to` edge.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub estimate: Option<Estimate>,
    #[serde(default)]
    pub wsjf: Option<WsjfIn>,
    /// Parent via `part_of`.
    #[serde(default)]
    pub part_of: Option<Seq>,
    /// Target of a `duplicates` edge: such a ticket is excluded.
    #[serde(default)]
    pub duplicates: Option<Seq>,
    /// Half-day of closing (for fever when all buffer tasks are already done).
    #[serde(default)]
    pub done_at: Option<HalfDay>,
}

/// A blocker outside the scope: a milestone with a date from `plan.current` of its scope, or without one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIn {
    pub seq: Seq,
    pub code: String,
    pub status: Status,
    #[serde(default)]
    pub forecast_end: Option<HalfDay>,
}

/// A live confirmed `blocks` edge: `src` blocks `dst`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockEdge {
    pub src: Seq,
    pub dst: Seq,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInput {
    /// Scope root (umbrella).
    pub root: Seq,
    pub tasks: Vec<TaskIn>,
    #[serde(default)]
    pub blocks: Vec<BlockEdge>,
    #[serde(default)]
    pub external: Vec<ExternalIn>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferMethod {
    /// Cut and Paste: half the sum of the safety margins.
    #[default]
    Cnp,
    /// Square root of the sum of squared margins.
    Ssq,
}

/// A live `plan.scope` row and estimate dictionaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanConfig {
    pub mode: Mode,
    pub due: Option<HalfDay>,
    pub buffer_method: BufferMethod,
    pub green_a_pct: i64,
    pub red_a_pct: i64,
    pub zone_slope_pct: i64,
    pub max_defaulted_pct: i64,
    /// `plan.type_default_estimate`: tag -> points.
    pub type_default_sp: BTreeMap<String, i64>,
    /// Points of a task without an estimate and without a matching tag.
    pub other_default_sp: i64,
    /// `plan.points_to_hd`: points -> half-days; anything missing is 1 point = 1 half-day.
    pub points_to_hd: BTreeMap<i64, i64>,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Forward,
            due: None,
            buffer_method: BufferMethod::Cnp,
            green_a_pct: 10,
            red_a_pct: 30,
            zone_slope_pct: 60,
            max_defaulted_pct: 20,
            type_default_sp: [("bug", 2), ("chore", 2), ("feature", 5)]
                .into_iter()
                .map(|(t, sp)| (t.to_string(), sp))
                .collect(),
            other_default_sp: 3,
            points_to_hd: BTreeMap::new(),
        }
    }
}

impl PlanConfig {
    /// Half-days for points (at least 1).
    pub fn points_hd(&self, sp: i64) -> i64 {
        self.points_to_hd.get(&sp).copied().unwrap_or(sp).max(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Zone {
    Red,
    Yellow,
    Green,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferKind {
    Project,
    Feeding,
}

/// A task in the plan. Interval `[start, end)` in half-days; for an umbrella it spans its children.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPlan {
    pub seq: Seq,
    pub code: String,
    pub status: Status,
    pub assignee: Option<String>,
    /// Aggressive (50%) duration, the remainder for doing.
    pub a: Option<i64>,
    /// Full aggressive duration without subtracting the remainder: the baseline records it.
    pub a_full: Option<i64>,
    /// Safe (90%) duration.
    pub s: Option<i64>,
    pub start: Option<HalfDay>,
    pub end: Option<HalfDay>,
    pub on_chain: bool,
    /// Key of the feeding buffer of a task outside the critical chain.
    pub buffer: Option<String>,
    /// Open, all blockers done, `snb <= today`.
    pub ready: bool,
    /// WSJF in hundredths.
    pub wsjf: Option<i64>,
    pub flags: Flags,
    /// Path of codes of the cycle for `in_cycle`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycle: Vec<String>,
}

/// A critical chain link, in order from the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLink {
    pub seq: Seq,
    /// Linked to the previous link by a resource (the same assignee), not by a dependency.
    pub resource_link: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPlan {
    /// `pb`, `fb:<seq of the entry point>` or `fb:finish`.
    pub key: String,
    pub kind: BufferKind,
    /// Entry point into the critical chain (empty for the FB before the finish and for PB).
    pub entry: Option<Seq>,
    /// Buffer tasks: the critical chain for PB, the subnet for FB.
    pub tasks: Vec<Seq>,
    /// Size per rule 8 (or from the baseline).
    pub size: i64,
    /// Actual place after compression.
    pub start: HalfDay,
    pub end: HalfDay,
    pub compressed: bool,
    pub chain_complete_pct: Option<i64>,
    pub consumed_pct: Option<i64>,
    pub zone: Option<Zone>,
    /// Penetration above the green line, hundredths of a percent.
    pub penetration: Option<i64>,
}

/// An edge excluded from the schedule (for example `started_while_blocked`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IgnoredEdge {
    pub src: Seq,
    pub dst: Seq,
    pub reason: String,
}

/// Resource buffer warning to the assignee of the next critical chain link (rule 12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceWarning {
    pub assignee: String,
    pub seq: Seq,
    pub after: Seq,
    /// `prev_half_done` or `starts_soon`.
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub algo_version: u32,
    pub root: Seq,
    pub today: HalfDay,
    /// Projected due date: the end of PB.
    pub end: HalfDay,
    pub late_by: i64,
    /// Share of the critical chain duration with a default estimate, percent.
    pub defaulted_pct: i64,
    /// Approval without `accept_defaults` is allowed.
    pub approvable: bool,
    /// WSJF of the scope root in hundredths.
    pub wsjf_root: Option<i64>,
    pub flags: Flags,
    pub tasks: Vec<TaskPlan>,
    pub chain: Vec<ChainLink>,
    pub buffers: Vec<BufferPlan>,
    pub ignored_edges: Vec<IgnoredEdge>,
    pub resource_warnings: Vec<ResourceWarning>,
}

impl Plan {
    pub fn task(&self, seq: Seq) -> Option<&TaskPlan> {
        self.tasks
            .binary_search_by_key(&seq, |t| t.seq)
            .ok()
            .map(|i| &self.tasks[i])
    }

    pub fn buffer(&self, key: &str) -> Option<&BufferPlan> {
        self.buffers.iter().find(|b| b.key == key)
    }
}

/// A feeding buffer in the approved baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFeeding {
    pub key: String,
    pub tasks: Vec<Seq>,
    pub size: i64,
    /// End of the subnet (its last task) at approval time.
    pub end: HalfDay,
}

/// Approved baseline (`plan.baseline.result`): critical chain members, FB members and sizes, PB size, dates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub chain: Vec<Seq>,
    /// End of the critical chain without PB.
    pub chain_end: HalfDay,
    pub pb_size: i64,
    pub feeding: Vec<BaselineFeeding>,
    /// Full aggressive task durations at approval time (for doing without subtracting the remainder).
    pub a: BTreeMap<Seq, i64>,
    /// All scope tasks at approval time (for `scope_change`).
    pub tasks: Vec<Seq>,
    /// Baseline task intervals.
    pub dates: BTreeMap<Seq, (HalfDay, HalfDay)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_skips_weekends() {
        let mon = NaiveDate::from_ymd_opt(2030, 1, 7).unwrap();
        let fri = NaiveDate::from_ymd_opt(2030, 1, 11).unwrap();
        let sat = NaiveDate::from_ymd_opt(2030, 1, 12).unwrap();
        let next_mon = NaiveDate::from_ymd_opt(2030, 1, 14).unwrap();
        assert_eq!(half_day_of(fri) - half_day_of(mon), 8);
        assert_eq!(half_day_of(sat), half_day_of(next_mon));
        assert_eq!(half_day_of(next_mon) - half_day_of(fri), 2);
        for d in [mon, fri, next_mon] {
            assert_eq!(date_of(half_day_of(d)), d);
            assert_eq!(date_of(half_day_of(d) + 1), d);
        }
        assert_eq!(half_day_of(epoch()), 0);
        let before = NaiveDate::from_ymd_opt(2023, 12, 29).unwrap();
        assert_eq!(half_day_of(before), -2);
        assert_eq!(date_of(-2), before);
    }
}
