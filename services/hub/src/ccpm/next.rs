//! An assignee's next task across all scopes (rule 13) and the resource buffer (rule 12).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{Flags, HalfDay, Plan, PlanConfig, ResourceWarning, Seq, Status, TaskIn, Zone};
use super::network::{durations, flag};

/// A task outside scopes: the caller computes readiness over the whole graph (with `snb`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideTask {
    pub seq: Seq,
    pub code: String,
    pub assignee: String,
    pub status: Status,
    pub ready: bool,
    pub wsjf: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextTask {
    pub seq: Option<Seq>,
    pub code: Option<String>,
    /// Root of the task's scope (empty outside scopes).
    pub scope: Option<Seq>,
    pub flags: Flags,
}

struct Cand<'a> {
    seq: Seq,
    code: &'a str,
    scope: Option<Seq>,
    zone: Option<Zone>,
    penetration: Option<i64>,
    on_chain: bool,
    start: Option<HalfDay>,
    wsjf: Option<i64>,
}

impl Cand<'_> {
    /// Zone (red, yellow, green, none), penetration descending; within a zone critical chain before feeding, planned start,
    /// WSJF descending; without a zone (no baseline or outside scopes) straight to WSJF descending, then critical chain and start.
    /// Without WSJF to the end, last by seq.
    fn key(&self) -> impl Ord {
        let wsjf = self.wsjf.map_or(i64::MAX, |w| -w);
        let chain = i64::from(!self.on_chain);
        let start = self.start.unwrap_or(HalfDay::MAX);
        let mid = if self.zone.is_some() {
            [chain, start, wsjf]
        } else {
            [wsjf, chain, start]
        };
        (
            self.zone.map_or(3, |z| z as u8),
            self.penetration.map_or((1, 0), |p| (0, -p)),
            mid,
            self.seq,
        )
    }
}

/// Next task: doing (several = `multitasking`, by seq), otherwise the best ready open task.
pub fn next_task(assignee: &str, plans: &[Plan], outside: &[OutsideTask]) -> NextTask {
    let mut cands: Vec<Cand> = Vec::new();
    let mut doing: BTreeMap<Seq, (&str, Option<Seq>)> = BTreeMap::new();
    for p in plans {
        for t in &p.tasks {
            if t.assignee.as_deref() != Some(assignee) {
                continue;
            }
            if t.status == Status::Doing {
                doing.entry(t.seq).or_insert((&t.code, Some(p.root)));
            } else if t.ready && t.status == Status::Open {
                let buf = if t.on_chain {
                    p.buffer("pb")
                } else {
                    t.buffer.as_deref().and_then(|k| p.buffer(k))
                };
                cands.push(Cand {
                    seq: t.seq,
                    code: &t.code,
                    scope: Some(p.root),
                    zone: buf.and_then(|b| b.zone),
                    penetration: buf.and_then(|b| b.penetration),
                    on_chain: t.on_chain,
                    start: t.start,
                    wsjf: t.wsjf,
                });
            }
        }
    }
    for t in outside.iter().filter(|t| t.assignee == assignee) {
        if t.status == Status::Doing {
            doing.entry(t.seq).or_insert((&t.code, None));
        } else if t.ready && t.status == Status::Open {
            cands.push(Cand {
                seq: t.seq,
                code: &t.code,
                scope: None,
                zone: None,
                penetration: None,
                on_chain: false,
                start: None,
                wsjf: t.wsjf,
            });
        }
    }
    let mut out = NextTask::default();
    if let Some((&seq, &(code, scope))) = doing.iter().next() {
        out.seq = Some(seq);
        out.code = Some(code.to_string());
        out.scope = scope;
        if doing.len() > 1 {
            flag(&mut out.flags, "multitasking");
        }
    } else if let Some(c) = cands.iter().min_by_key(|c| c.key()) {
        out.seq = Some(c.seq);
        out.code = Some(c.code.to_string());
        out.scope = c.scope;
    }
    if let Some(seq) = out.seq
        && overlaps_other_scope(assignee, seq, out.scope, plans)
    {
        flag(&mut out.flags, "cross_scope_overlap");
    }
    out
}

/// The task interval overlaps a task of the same assignee in another scope (there is no drum).
fn overlaps_other_scope(assignee: &str, seq: Seq, scope: Option<Seq>, plans: &[Plan]) -> bool {
    let Some(scope) = scope else { return false };
    let Some(mine) = plans
        .iter()
        .find(|p| p.root == scope)
        .and_then(|p| p.task(seq))
    else {
        return false;
    };
    let (Some(s), Some(e)) = (mine.start, mine.end) else {
        return false;
    };
    plans.iter().filter(|p| p.root != scope).any(|p| {
        p.tasks.iter().any(|t| {
            t.assignee.as_deref() == Some(assignee)
                && t.seq != seq
                && t.start.zip(t.end).is_some_and(|(ts, te)| ts < e && s < te)
        })
    })
}

/// Resource buffer warnings: to the assignee of the next critical chain link when the previous link is 50% done
/// (`prev_half_done`) or no more than 2 working days remain before the link starts (`starts_soon`).
pub fn resource_warnings(
    plan: &Plan,
    input: &BTreeMap<Seq, &TaskIn>,
    cfg: &PlanConfig,
) -> Vec<ResourceWarning> {
    let mut out = Vec::new();
    for w in plan.chain.windows(2) {
        let (prev, cur) = (w[0].seq, w[1].seq);
        let (Some(pt), Some(ct)) = (plan.task(prev), plan.task(cur)) else {
            continue;
        };
        let Some(who) = ct.assignee.clone() else {
            continue;
        };
        if ct.status != Status::Open || pt.assignee.as_deref() == Some(&who) {
            continue;
        }
        let half = input.get(&prev).is_some_and(|t| {
            t.status == Status::Doing
                && t.estimate
                    .as_ref()
                    .and_then(|e| e.remaining_hd)
                    .is_some_and(|r| {
                        let mut open = (*t).clone();
                        open.status = Status::Open;
                        let full = durations(&open, cfg).0;
                        (full - r) * 2 >= full
                    })
        });
        let soon = ct.start.is_some_and(|s| s - plan.today <= 4);
        let reason = if half {
            "prev_half_done"
        } else if soon {
            "starts_soon"
        } else {
            continue;
        };
        out.push(ResourceWarning {
            assignee: who,
            seq: cur,
            after: prev,
            reason: reason.into(),
        });
    }
    out
}
