//! Forward levelling (SSGS, rule 5) and the final backward placement with buffers (rule 9).

use std::collections::{BTreeMap, BTreeSet};

use super::buffers::Entry;
use super::model::{HalfDay, Seq};
use super::network::Node;

/// Interval `[start, end)` in half-days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub start: HalfDay,
    pub end: HalfDay,
}

impl Slot {
    pub fn new(start: HalfDay, len: i64) -> Self {
        Self {
            start,
            end: start + len,
        }
    }

    pub fn shifted(self, by: i64) -> Self {
        Self {
            start: self.start + by,
            end: self.end + by,
        }
    }
}

/// Topological order of `nodes` (Kahn, ties go to the smaller seq). Predecessors outside `nodes` do not interfere.
fn topo(nodes: &BTreeMap<Seq, Node>, preds: &BTreeMap<Seq, BTreeSet<Seq>>) -> Vec<Seq> {
    let mut indeg: BTreeMap<Seq, usize> = nodes
        .keys()
        .map(|&s| {
            (
                s,
                preds[&s].iter().filter(|p| nodes.contains_key(p)).count(),
            )
        })
        .collect();
    let mut ready: BTreeSet<Seq> = indeg.iter().filter(|e| *e.1 == 0).map(|e| *e.0).collect();
    let mut succ: BTreeMap<Seq, Vec<Seq>> = BTreeMap::new();
    for &y in nodes.keys() {
        for &x in preds[&y].iter().filter(|p| nodes.contains_key(p)) {
            succ.entry(x).or_default().push(y);
        }
    }
    let mut out = Vec::with_capacity(nodes.len());
    while let Some(x) = ready.pop_first() {
        out.push(x);
        for &y in succ.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            let d = indeg.get_mut(&y).expect("indeg");
            *d -= 1;
            if *d == 0 {
                ready.insert(y);
            }
        }
    }
    out
}

/// Serial SSGS: a ready task by the key `(LF, -tail, priority nulls last, seq)`, start
/// `max(EF of predecessors, release, base, free_from[assignee])`. doing tasks go first for their assignee,
/// in seq order. `fixed`: already placed predecessors outside `nodes`. Returns the intervals and the tasks
/// with `multitasking`.
pub fn forward(
    nodes: &BTreeMap<Seq, Node>,
    preds: &BTreeMap<Seq, BTreeSet<Seq>>,
    base: HalfDay,
    free_from: &mut BTreeMap<String, HalfDay>,
    fixed: &BTreeMap<Seq, Slot>,
) -> (BTreeMap<Seq, Slot>, BTreeSet<Seq>) {
    let order = topo(nodes, preds);
    let release = |s: Seq| nodes[&s].release.max(base);
    // CPM without resources: early dates and the tail to the virtual finish.
    let mut ef: BTreeMap<Seq, HalfDay> = BTreeMap::new();
    for &x in &order {
        let es = preds[&x]
            .iter()
            .map(|p| {
                ef.get(p)
                    .copied()
                    .or_else(|| fixed.get(p).map(|s| s.end))
                    .unwrap_or(base)
            })
            .fold(release(x), HalfDay::max);
        ef.insert(x, es + nodes[&x].a);
    }
    let proj = ef.values().copied().max().unwrap_or(base);
    let mut tail: BTreeMap<Seq, i64> = BTreeMap::new();
    let mut succ: BTreeMap<Seq, Vec<Seq>> = BTreeMap::new();
    for &y in &order {
        for &x in preds[&y].iter().filter(|p| nodes.contains_key(p)) {
            succ.entry(x).or_default().push(y);
        }
    }
    for &x in order.iter().rev() {
        let t = succ
            .get(&x)
            .map(|ys| ys.iter().map(|y| nodes[y].a + tail[y]).max().unwrap_or(0))
            .unwrap_or(0);
        tail.insert(x, t);
    }
    let key = |s: Seq| {
        let n = &nodes[&s];
        (
            proj - tail[&s],
            -tail[&s],
            n.priority.is_none(),
            n.priority.unwrap_or(0),
            s,
        )
    };

    let mut slots: BTreeMap<Seq, Slot> = BTreeMap::new();
    let mut multi: BTreeSet<Seq> = BTreeSet::new();
    let mut doing_by: BTreeMap<Option<&str>, Vec<Seq>> = BTreeMap::new();
    for (&s, n) in nodes.iter().filter(|(_, n)| n.doing) {
        doing_by.entry(n.assignee.as_deref()).or_default().push(s);
    }
    for (who, seqs) in &doing_by {
        match who {
            Some(w) => {
                if seqs.len() > 1 {
                    multi.extend(seqs.iter().copied());
                }
                for &s in seqs {
                    let st = release(s).max(free_from.get(*w).copied().unwrap_or(base));
                    let slot = Slot::new(st, nodes[&s].a);
                    free_from.insert(w.to_string(), slot.end);
                    slots.insert(s, slot);
                }
            }
            None => {
                for &s in seqs {
                    slots.insert(s, Slot::new(release(s), nodes[&s].a));
                }
            }
        }
    }

    let end_of =
        |slots: &BTreeMap<Seq, Slot>, p: &Seq| slots.get(p).or_else(|| fixed.get(p)).map(|s| s.end);
    while slots.len() < nodes.len() {
        let pick = nodes
            .keys()
            .copied()
            .filter(|s| !slots.contains_key(s))
            .filter(|s| {
                preds[s]
                    .iter()
                    .all(|p| !nodes.contains_key(p) || slots.contains_key(p))
            })
            .min_by_key(|&s| key(s))
            .expect("an acyclic network always has a ready task");
        let n = &nodes[&pick];
        let mut st = preds[&pick]
            .iter()
            .filter_map(|p| end_of(&slots, p))
            .fold(release(pick), HalfDay::max);
        if let Some(w) = &n.assignee {
            st = st.max(free_from.get(w).copied().unwrap_or(base));
        }
        let slot = Slot::new(st, n.a);
        if let Some(w) = &n.assignee {
            free_from.insert(w.clone(), slot.end);
        }
        slots.insert(pick, slot);
    }
    (slots, multi)
}

/// Sequence of each assignee's tasks by start.
pub fn resource_order(
    nodes: &BTreeMap<Seq, Node>,
    slots: &BTreeMap<Seq, Slot>,
) -> BTreeMap<String, Vec<Seq>> {
    let mut out: BTreeMap<String, Vec<Seq>> = BTreeMap::new();
    for (&s, n) in nodes {
        if let Some(w) = &n.assignee {
            out.entry(w.clone()).or_default().push(s);
        }
    }
    for seqs in out.values_mut() {
        seqs.sort_by_key(|s| (slots[s].start, *s));
    }
    out
}

/// Network for the final placement: tasks by forward start, dependencies together with resource links,
/// the critical chain, subnets and buffer sizes.
pub struct Layout<'a> {
    pub nodes: &'a BTreeMap<Seq, Node>,
    pub order: &'a [Seq],
    pub fwd: &'a BTreeMap<Seq, Slot>,
    pub pred: &'a BTreeMap<Seq, BTreeSet<Seq>>,
    pub succ: &'a BTreeMap<Seq, BTreeSet<Seq>>,
    pub chain: &'a BTreeSet<Seq>,
    /// End of the critical chain in the forward schedule: the largest end of its links (PB start before the shift).
    pub chain_end: HalfDay,
    pub member: &'a BTreeMap<Seq, Entry>,
    pub fb_size: &'a BTreeMap<Entry, i64>,
    pub pb: i64,
}

#[derive(Debug)]
pub struct Placed {
    pub slots: BTreeMap<Seq, Slot>,
    /// Actual place of the FB after compression.
    pub fb: BTreeMap<Entry, Slot>,
    pub pb: Slot,
    /// Shift of the critical chain relative to the forward schedule.
    pub shift: i64,
    /// The computation converged (`false` = step limit exhausted, the schedule may violate bounds).
    pub converged: bool,
}

/// Final schedule (rule 9). The critical chain lies as in the forward schedule with the shift `shift` (doing in place),
/// PB from the largest end of the placed critical chain links (doing in place, the rest shifted), but no earlier than
/// the end of the finish subnet: its tasks after the last critical chain link (a baseline with drift) cannot be fixed by the shift, so
/// an overrun of the finish subnet pushes only PB.
/// Tasks outside the critical chain are placed as late as possible by a backward pass up to their FB (the FB up to the entry point, the finish one up to PB),
/// then shifted right to `max(release, end of predecessors)`; doing tasks stay in place. An overrun
/// first eats the FB (`compressed`), and if the FB is not enough or a chain link is earlier than its bound, the whole plan
/// shifts right and the computation repeats.
pub fn place(l: &Layout, shift0: i64) -> Placed {
    let mut shift = shift0;
    let mut pb_floor = HalfDay::MIN;
    // Converges in one or two steps; the limit guards against bad data.
    let limit = l.order.len() + 8;
    for round in 0..limit {
        let mut slots: BTreeMap<Seq, Slot> = BTreeMap::new();
        // doing tasks are already running and stay in place; a gap after them is flagged `chain_gap`.
        for &c in l.chain {
            let by = if l.nodes[&c].doing { 0 } else { shift };
            slots.insert(c, l.fwd[&c].shifted(by));
        }
        // PB right after the largest end of the placed links: the last doing link does not shift, and
        // `chain_end + shift` would leave slack outside the buffer between it and PB.
        let pb_start = l
            .chain
            .iter()
            .map(|c| slots[c].end)
            .max()
            .unwrap_or(l.chain_end + shift)
            .max(pb_floor);
        let fb_end = |slots: &BTreeMap<Seq, Slot>, e: Entry| match e {
            Entry::Chain(s) => slots[&s].start,
            Entry::Finish => pb_start,
        };
        let fb_start = |slots: &BTreeMap<Seq, Slot>, e: Entry| fb_end(slots, e) - l.fb_size[&e];

        let mut late_start: BTreeMap<Seq, HalfDay> = BTreeMap::new();
        for &x in l.order.iter().rev() {
            if l.chain.contains(&x) {
                continue;
            }
            let ys = &l.succ[&x];
            let mut lf = if ys.is_empty() {
                fb_start(&slots, Entry::Finish)
            } else {
                HalfDay::MAX
            };
            for y in ys {
                let v = if l.chain.contains(y) {
                    fb_start(&slots, l.member[&x])
                } else {
                    late_start[y]
                };
                lf = lf.min(v);
            }
            late_start.insert(x, lf - l.nodes[&x].a);
        }
        for &x in l.order {
            if l.chain.contains(&x) {
                continue;
            }
            let n = &l.nodes[&x];
            let st = if n.doing {
                l.fwd[&x].start
            } else {
                l.pred[&x]
                    .iter()
                    .map(|p| slots[p].end)
                    .fold(late_start[&x].max(n.release), HalfDay::max)
            };
            slots.insert(x, Slot::new(st, n.a));
        }

        let mut need = 0i64;
        let mut floor_moved = false;
        let mut fb: BTreeMap<Entry, Slot> = BTreeMap::new();
        for &e in l.fb_size.keys() {
            let feeds = |x: &Seq| match e {
                Entry::Chain(_) => l.succ[x].iter().any(|y| l.chain.contains(y)),
                Entry::Finish => l.succ[x].is_empty(),
            };
            let reach = l
                .member
                .iter()
                .filter(|&(x, m)| *m == e && feeds(x))
                .map(|(x, _)| slots[x].end)
                .max()
                .unwrap_or(HalfDay::MIN);
            let (ls, le) = (fb_start(&slots, e), fb_end(&slots, e));
            fb.insert(
                e,
                Slot {
                    start: ls.max(reach),
                    end: le.max(reach),
                },
            );
            if e == Entry::Finish && reach > le {
                pb_floor = reach;
                floor_moved = true;
            } else {
                need = need.max(reach - le);
            }
        }
        // A critical chain link no earlier than its bound and no earlier than the end of the previous links (with a negative shift
        // in backward mode or with doing in place).
        for &c in l.chain {
            let n = &l.nodes[&c];
            need = need.max(n.release - slots[&c].start);
            for p in l.pred[&c].iter().filter(|p| l.chain.contains(p)) {
                need = need.max(slots[p].end - slots[&c].start);
            }
        }
        let done = need <= 0 && !floor_moved;
        if done || round + 1 == limit {
            return Placed {
                slots,
                fb,
                pb: Slot::new(pb_start, l.pb),
                shift,
                converged: done,
            };
        }
        shift += need.max(0);
    }
    unreachable!("the loop above always returns a result")
}
