//! Baseline, fever and zones (rules 10-11).

use std::collections::BTreeMap;

use super::model::{
    Baseline, BaselineFeeding, BufferKind, BufferPlan, HalfDay, Plan, PlanConfig, Seq, Status,
    TaskIn, Zone,
};

/// Zone by `x = chain_complete`: `max(consumed, 0) <= green_a + slope·x` is green, `<= red_a + slope·x` yellow,
/// otherwise red. Comparison in hundredths of a percent, without division.
pub fn zone(cfg: &PlanConfig, chain_complete: i64, consumed: i64) -> Zone {
    let c = consumed.max(0) * 100;
    let slope = cfg.zone_slope_pct * chain_complete;
    if c <= cfg.green_a_pct * 100 + slope {
        Zone::Green
    } else if c <= cfg.red_a_pct * 100 + slope {
        Zone::Yellow
    } else {
        Zone::Red
    }
}

/// Penetration above the green line in hundredths of a percent: `consumed - (green_a + slope·chain_complete)`.
pub fn penetration(cfg: &PlanConfig, chain_complete: i64, consumed: i64) -> i64 {
    consumed * 100 - (cfg.green_a_pct * 100 + cfg.zone_slope_pct * chain_complete)
}

/// Buffer consumption in signed percent; division truncates toward zero.
pub fn consumed(forecast_end: HalfDay, base_end: HalfDay, size: i64) -> i64 {
    (forecast_end - base_end) * 100 / size.max(1)
}

/// Completed share by baseline (full) durations: done in full, doing by `a - remaining`.
/// Cancelled tasks and tasks gone from the scope are in neither the numerator nor the denominator.
pub fn chain_complete(
    tasks: &[Seq],
    a: &BTreeMap<Seq, i64>,
    input: &BTreeMap<Seq, &TaskIn>,
) -> i64 {
    let live = || {
        tasks
            .iter()
            .filter_map(|s| input.get(s).map(|t| (*t, a.get(s).copied().unwrap_or(0))))
            .filter(|(t, _)| t.status != Status::Cancelled)
    };
    let total: i64 = live().map(|(_, base)| base).sum();
    if total == 0 {
        return 0;
    }
    let done: i64 = live()
        .map(|(t, base)| match t.status {
            Status::Done => base,
            Status::Doing => t
                .estimate
                .as_ref()
                .and_then(|e| e.remaining_hd)
                .map_or(0, |r| (base - r).clamp(0, base)),
            _ => 0,
        })
        .sum();
    done * 100 / total
}

/// The last closing date of the set's tasks, otherwise today.
fn done_end(plan: &Plan, tasks: &[Seq], input: &BTreeMap<Seq, &TaskIn>) -> HalfDay {
    tasks
        .iter()
        .filter_map(|s| input.get(s).and_then(|t| t.done_at))
        .max()
        .unwrap_or(plan.today)
}

/// Fever against the baseline: PB by the baseline critical chain, FB by the baseline subnet with the same key.
///
/// Critical chain projection: the PB start in the current placement and the ends of baseline links that are now outside the current chain (drift).
/// FB projection: the largest of `proj` (projected task ends, not tied to the chain shift) over
/// the tasks of the baseline subnet. If all tasks of the set are done, the projection is the last closing date; an FB
/// that is no longer in the plan is added as a point of zero length.
pub fn apply(
    plan: &mut Plan,
    base: &Baseline,
    input: &BTreeMap<Seq, &TaskIn>,
    proj: &BTreeMap<Seq, HalfDay>,
    cfg: &PlanConfig,
) {
    let mut points: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    if let Some(pb) = plan.buffer("pb") {
        let live = (!plan.chain.is_empty()).then_some(pb.start);
        let fc = base
            .chain
            .iter()
            .filter_map(|s| plan.task(*s).and_then(|t| t.end))
            .chain(live)
            .max()
            .unwrap_or_else(|| done_end(plan, &base.chain, input));
        points.insert(
            "pb".into(),
            (
                chain_complete(&base.chain, &base.a, input),
                consumed(fc, base.chain_end, base.pb_size),
            ),
        );
    }
    for f in &base.feeding {
        let planned = f.tasks.iter().filter_map(|s| proj.get(s)).copied().max();
        if plan.buffer(&f.key).is_none() {
            // The subnet is done in full: the last fever point. Otherwise its tasks moved to other subnets.
            if planned.is_some() {
                continue;
            }
            let fc = done_end(plan, &f.tasks, input);
            plan.buffers.push(BufferPlan {
                key: f.key.clone(),
                kind: BufferKind::Feeding,
                entry: f.key.strip_prefix("fb:").and_then(|x| x.parse().ok()),
                tasks: Vec::new(),
                size: f.size.max(1),
                start: fc,
                end: fc,
                compressed: false,
                chain_complete_pct: None,
                consumed_pct: None,
                zone: None,
                penetration: None,
            });
        }
        let fc = planned.unwrap_or_else(|| done_end(plan, &f.tasks, input));
        points.insert(
            f.key.clone(),
            (
                chain_complete(&f.tasks, &base.a, input),
                consumed(fc, f.end, f.size),
            ),
        );
    }
    for b in &mut plan.buffers {
        if let Some(&(cc, cons)) = points.get(&b.key) {
            b.chain_complete_pct = Some(cc);
            b.consumed_pct = Some(cons);
            b.zone = Some(zone(cfg, cc, cons));
            b.penetration = Some(penetration(cfg, cc, cons));
        }
    }
}

/// Baseline from the current plan: what is written to `plan.baseline.result` on approval.
pub fn baseline(plan: &Plan) -> Baseline {
    let pb = plan.buffer("pb");
    let end_of = |tasks: &[Seq]| tasks.iter().filter_map(|s| plan.task(*s)?.end).max();
    Baseline {
        chain: plan.chain.iter().map(|l| l.seq).collect(),
        chain_end: pb.map_or(plan.end, |b| b.start),
        pb_size: pb.map_or(0, |b| b.size),
        feeding: plan
            .buffers
            .iter()
            .filter(|b| b.kind == BufferKind::Feeding)
            .map(|b| BaselineFeeding {
                key: b.key.clone(),
                tasks: b.tasks.clone(),
                size: b.size,
                end: end_of(&b.tasks).unwrap_or(b.start),
            })
            .collect(),
        a: plan
            .tasks
            .iter()
            .filter_map(|t| Some((t.seq, t.a_full.or(t.a)?)))
            .collect(),
        tasks: plan
            .tasks
            .iter()
            .filter(|t| t.a.is_some())
            .map(|t| t.seq)
            .collect(),
        dates: plan
            .tasks
            .iter()
            .filter_map(|t| Some((t.seq, (t.start?, t.end?))))
            .collect(),
    }
}

/// Approval is allowed if the share of the critical chain with a default estimate is within the threshold or the threshold is explicitly accepted.
pub fn can_approve(plan: &Plan, accept_defaults: bool) -> bool {
    plan.approvable || accept_defaults
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table of zone boundary points is in `fixtures/zones.table.json` (test in `ccpm::tests`).
    #[test]
    fn penetration_and_consumed() {
        let cfg = PlanConfig::default();
        assert_eq!(penetration(&cfg, 50, 40), 0);
        assert_eq!(penetration(&cfg, 50, 41), 100);
        assert_eq!(penetration(&cfg, 0, -20), -3000);
        assert_eq!(consumed(12, 10, 4), 50);
        assert_eq!(consumed(9, 10, 3), -33);
    }
}
