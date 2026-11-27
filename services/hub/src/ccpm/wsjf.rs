//! WSJF (rule 14): cost of delay / effort, in hundredths without f64. A heuristic after Smith's rule
//! (Smith 1956, WSPT): optimal only with a constant cost of delay and the true duration; in SAFe these are relative
//! scores.

use super::model::{Status, TaskIn};

/// Cost of delay of a task: business value + time criticality + risk reduction/opportunity.
pub fn cost_of_delay(t: &TaskIn) -> Option<i64> {
    t.wsjf
        .map(|w| w.business_value + w.time_criticality + w.risk_opportunity)
}

/// WSJF of a task in hundredths: `(bv + tc + rr) * 100 / effort_sp`. Empty without an estimate or WSJF.
pub fn task(t: &TaskIn) -> Option<i64> {
    let sp = t.estimate.as_ref()?.effort_sp;
    (sp > 0).then_some(())?;
    Some(cost_of_delay(t)? * 100 / sp)
}

/// WSJF of a scope root in hundredths: the sum of costs of delay of open leaves / the sum of their points.
pub fn root<'a>(leaves: impl Iterator<Item = &'a TaskIn>) -> Option<i64> {
    let (cod, sp) = leaves
        .filter(|t| matches!(t.status, Status::Open | Status::Doing))
        .filter_map(|t| Some((cost_of_delay(t)?, t.estimate.as_ref()?.effort_sp)))
        .fold((0, 0), |acc, (c, s)| (acc.0 + c, acc.1 + s));
    (sp > 0).then(|| cod * 100 / sp)
}

#[cfg(test)]
mod tests {
    use super::super::model::{Estimate, WsjfIn};
    use super::*;

    fn t(status: Status, sp: Option<i64>, w: Option<(i64, i64, i64)>) -> TaskIn {
        TaskIn {
            seq: 1,
            code: "a".into(),
            status,
            priority: None,
            tags: vec![],
            assignee: None,
            estimate: sp.map(|effort_sp| Estimate {
                effort_sp,
                aggressive_hd: None,
                safe_hd: None,
                remaining_hd: None,
                start_not_before: None,
            }),
            wsjf: w.map(|(b, c, r)| WsjfIn {
                business_value: b,
                time_criticality: c,
                risk_opportunity: r,
            }),
            part_of: None,
            duplicates: None,
            done_at: None,
        }
    }

    #[test]
    fn wsjf_in_hundredths() {
        assert_eq!(task(&t(Status::Open, Some(3), Some((5, 3, 2)))), Some(333));
        assert_eq!(task(&t(Status::Open, Some(8), Some((1, 1, 1)))), Some(37));
        assert_eq!(task(&t(Status::Open, None, Some((1, 1, 1)))), None);
        assert_eq!(task(&t(Status::Open, Some(2), None)), None);
        let ts = [
            t(Status::Open, Some(3), Some((5, 3, 2))),
            t(Status::Doing, Some(5), Some((8, 1, 1))),
            t(Status::Done, Some(1), Some((21, 21, 21))),
            t(Status::Open, None, Some((21, 21, 21))),
        ];
        assert_eq!(root(ts.iter()), Some((10 + 10) * 100 / 8));
        assert_eq!(root(ts[2..].iter()), None);
    }
}
