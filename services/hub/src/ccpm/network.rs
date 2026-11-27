//! Task network (rules 2-4): umbrella expansion, duplicates, external blockers, durations, cycles (Tarjan).

use std::collections::{BTreeMap, BTreeSet};

use super::model::{Flags, HalfDay, IgnoredEdge, PlanConfig, PlanInput, Seq, Status, TaskIn};

/// A planned task (a leaf in status open or doing outside a cycle).
#[derive(Clone, Debug)]
pub struct Node {
    pub a: i64,
    pub s: i64,
    pub assignee: Option<String>,
    pub doing: bool,
    pub priority: Option<i64>,
    /// Lower bound of the start: `max(today, snb, milestones of external blockers)`; `today` for doing.
    pub release: HalfDay,
}

/// What is known about any input ticket, including unplanned ones.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub a: Option<i64>,
    /// Full aggressive duration (for doing without subtracting the remainder): for the baseline and `chain_complete`.
    pub a_full: Option<i64>,
    pub s: Option<i64>,
    pub ready: bool,
    pub flags: Flags,
    pub cycle: Vec<String>,
    pub umbrella: bool,
}

#[derive(Debug, Default)]
pub struct Net {
    /// Tasks of the main schedule.
    pub nodes: BTreeMap<Seq, Node>,
    /// Dependencies among `nodes`.
    pub preds: BTreeMap<Seq, BTreeSet<Seq>>,
    pub succs: BTreeMap<Seq, BTreeSet<Seq>>,
    pub info: BTreeMap<Seq, Info>,
    pub ignored: Vec<IgnoredEdge>,
    /// Umbrella -> its leaves (for the summary bar).
    pub umbrellas: BTreeMap<Seq, BTreeSet<Seq>>,
}

pub fn flag(flags: &mut Flags, f: &str) {
    flags.insert(f.to_string());
}

/// Durations per rule 3: `(a, s, flags)`.
pub fn durations(t: &TaskIn, cfg: &PlanConfig) -> (i64, i64, Vec<&'static str>) {
    let mut flags = Vec::new();
    let (full_a, full_s) = match &t.estimate {
        Some(e) => {
            let a = e
                .aggressive_hd
                .unwrap_or_else(|| cfg.points_hd(e.effort_sp))
                .max(1);
            (a, e.safe_hd.unwrap_or(2 * a).max(a))
        }
        None => {
            flags.push("estimate_defaulted");
            let sp = t
                .tags
                .iter()
                .filter_map(|tag| cfg.type_default_sp.get(tag))
                .max()
                .copied()
                .unwrap_or(cfg.other_default_sp);
            let a = cfg.points_hd(sp);
            (a, 2 * a)
        }
    };
    if t.status != Status::Doing {
        return (full_a, full_s, flags);
    }
    match t.estimate.as_ref().and_then(|e| e.remaining_hd) {
        // A remainder below a half-day is rounded up to a half-day: the task still occupies the assignee.
        Some(r) => {
            let a = r.max(1);
            (a, (full_s - full_a + a).max(a), flags)
        }
        None => {
            flags.push("remaining_unknown");
            (full_a, full_s, flags)
        }
    }
}

/// Builds the network. `today` is needed for readiness (`snb <= today`) and the lower bound of the start.
pub fn build(input: &PlanInput, today: HalfDay, cfg: &PlanConfig) -> Net {
    let mut net = Net::default();
    // The first row with a given seq; the input order does not matter, a repeated seq in the input is not expected.
    let mut tasks: BTreeMap<Seq, &TaskIn> = BTreeMap::new();
    for t in &input.tasks {
        tasks.entry(t.seq).or_insert(t);
    }
    let external: BTreeMap<Seq, _> = input.external.iter().map(|e| (e.seq, e)).collect();

    let dup: BTreeSet<Seq> = tasks
        .values()
        .filter(|t| t.duplicates.is_some())
        .map(|t| t.seq)
        .collect();
    let mut children: BTreeMap<Seq, BTreeSet<Seq>> = BTreeMap::new();
    for t in tasks.values() {
        if let Some(p) = t.part_of
            && tasks.contains_key(&p)
            && !dup.contains(&t.seq)
            && p != t.seq
        {
            children.entry(p).or_default().insert(t.seq);
        }
    }
    let parent = |s: Seq| {
        tasks
            .get(&s)
            .and_then(|t| t.part_of)
            .filter(|p| tasks.contains_key(p))
    };
    let ancestors = |s: Seq| {
        let mut out = BTreeSet::new();
        let mut cur = parent(s);
        while let Some(p) = cur {
            if !out.insert(p) {
                break;
            }
            cur = parent(p);
        }
        out
    };
    let leaves = |u: Seq| {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![u];
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            match children.get(&x) {
                Some(cs) => stack.extend(cs.iter().copied()),
                None if x != u => {
                    out.insert(x);
                }
                None => {}
            }
        }
        out
    };
    let is_leaf =
        |s: Seq| tasks.contains_key(&s) && !children.contains_key(&s) && !dup.contains(&s);
    let active = |s: Seq| is_leaf(s) && !tasks[&s].status.finished();

    for (&seq, t) in &tasks {
        let mut info = Info::default();
        if dup.contains(&seq) {
            flag(&mut info.flags, "duplicate");
        } else if children.contains_key(&seq) {
            info.umbrella = true;
            flag(&mut info.flags, "umbrella");
            net.umbrellas.insert(seq, leaves(seq));
        } else {
            let (a, s, fl) = durations(t, cfg);
            info.a = Some(a);
            let mut open = (*t).clone();
            open.status = Status::Open;
            info.a_full = Some(durations(&open, cfg).0);
            info.s = Some(s);
            for f in fl {
                flag(&mut info.flags, f);
            }
        }
        net.info.insert(seq, info);
    }

    // Edges between leaves that go into the schedule (without edges into doing: they are excluded as
    // `started_while_blocked`); umbrella entries and exits are found by them.
    let raw: BTreeSet<(Seq, Seq)> = input
        .blocks
        .iter()
        .map(|e| (e.src, e.dst))
        .filter(|(a, b)| a != b)
        .collect();
    let leaf_edges: BTreeSet<(Seq, Seq)> = raw
        .iter()
        .copied()
        .filter(|&(a, b)| active(a) && active(b) && tasks[&b].status != Status::Doing)
        .collect();
    let side = |u: Seq, entries: bool| -> BTreeSet<Seq> {
        let ls: BTreeSet<Seq> = net.umbrellas[&u]
            .iter()
            .copied()
            .filter(|&l| active(l))
            .collect();
        ls.iter()
            .copied()
            .filter(|&l| {
                !leaf_edges.iter().any(|&(a, b)| {
                    if entries {
                        b == l && ls.contains(&a)
                    } else {
                        a == l && ls.contains(&b)
                    }
                })
            })
            .collect()
    };

    // An edge to an umbrella expands to its entries, an edge from an umbrella to its exits.
    let mut edges: BTreeSet<(Seq, Seq)> = BTreeSet::new();
    let mut ext_edges: BTreeSet<(Seq, Seq)> = BTreeSet::new();
    // All blockers of a leaf, including finished ones: for readiness.
    let mut blockers: BTreeMap<Seq, BTreeSet<Seq>> = BTreeMap::new();
    for &(src, dst) in &raw {
        if !tasks.contains_key(&dst) || dup.contains(&dst) {
            continue;
        }
        if tasks.contains_key(&src)
            && (ancestors(dst).contains(&src) || ancestors(src).contains(&dst))
        {
            continue;
        }
        let dsts: BTreeSet<Seq> = if children.contains_key(&dst) {
            side(dst, true)
        } else {
            [dst].into()
        };
        if let Some(t) = tasks.get(&src) {
            if dup.contains(&t.seq) {
                continue;
            }
            let srcs: BTreeSet<Seq> = if children.contains_key(&src) {
                side(src, false)
            } else {
                [src].into()
            };
            // Readiness waits for all leaves of the source umbrella, not only its exits.
            let all_srcs: BTreeSet<Seq> = if children.contains_key(&src) {
                net.umbrellas[&src].clone()
            } else {
                [src].into()
            };
            for &y in &dsts {
                for &x in &all_srcs {
                    if x != y {
                        blockers.entry(y).or_default().insert(x);
                    }
                }
                for &x in &srcs {
                    if x != y && active(x) && active(y) {
                        edges.insert((x, y));
                    }
                }
            }
        } else if external.contains_key(&src) {
            for &y in &dsts {
                blockers.entry(y).or_default().insert(src);
                ext_edges.insert((src, y));
            }
        }
    }

    // Readiness by all blockers (inside the scope and outside it).
    let status_of = |s: Seq| {
        tasks
            .get(&s)
            .map(|t| t.status)
            .or_else(|| external.get(&s).map(|e| e.status))
    };
    for (&seq, t) in &tasks {
        let info = net.info.get_mut(&seq).expect("info");
        let snb = t.estimate.as_ref().and_then(|e| e.start_not_before);
        let open_blockers = blockers.get(&seq).is_some_and(|bs| {
            bs.iter()
                .any(|&b| !status_of(b).is_some_and(Status::finished))
        });
        info.ready = is_leaf(seq)
            && t.status == Status::Open
            && !open_blockers
            && snb.is_none_or(|d| d <= today);
    }

    // An edge into doing from an unfinished blocker is excluded from the schedule.
    let mut sched: BTreeSet<(Seq, Seq)> = BTreeSet::new();
    for &(x, y) in &edges {
        if tasks[&y].status == Status::Doing {
            net.ignored.push(IgnoredEdge {
                src: x,
                dst: y,
                reason: "started_while_blocked".into(),
            });
            flag(
                &mut net.info.get_mut(&y).expect("info").flags,
                "started_while_blocked",
            );
        } else {
            sched.insert((x, y));
        }
    }
    let mut release: BTreeMap<Seq, HalfDay> = BTreeMap::new();
    for &(e, y) in &ext_edges {
        let ext = external[&e];
        if ext.status.finished() || !active(y) {
            continue;
        }
        let info = net.info.get_mut(&y).expect("info");
        if tasks[&y].status == Status::Doing {
            net.ignored.push(IgnoredEdge {
                src: e,
                dst: y,
                reason: "started_while_blocked".into(),
            });
            flag(&mut info.flags, "started_while_blocked");
        } else if let Some(d) = ext.forecast_end {
            let r = release.entry(y).or_insert(d);
            *r = (*r).max(d);
            flag(&mut info.flags, "external_milestone");
        } else {
            // A milestone without a date sits at today: the task stays in the network, the critical chain and the buffers, and every
            // day of waiting for the blocker moves it together with today and consumes the buffer instead of pushing it past PB.
            flag(&mut info.flags, "external_blocker");
        }
    }

    // Cycles: tasks of non-trivial components are excluded, the rest of the network is planned.
    let act: BTreeSet<Seq> = tasks.keys().copied().filter(|&s| active(s)).collect();
    let mut succ: BTreeMap<Seq, BTreeSet<Seq>> =
        act.iter().map(|&s| (s, BTreeSet::new())).collect();
    for &(x, y) in &sched {
        succ.get_mut(&x).expect("succ").insert(y);
    }
    let mut cyc: BTreeSet<Seq> = BTreeSet::new();
    for comp in tarjan(&succ) {
        let nontrivial = comp.len() > 1 || succ[&comp[0]].contains(&comp[0]);
        if !nontrivial {
            continue;
        }
        let set: BTreeSet<Seq> = comp.iter().copied().collect();
        let path: Vec<String> = cycle_path(&succ, &set)
            .iter()
            .map(|s| tasks[s].code.clone())
            .collect();
        for &s in &set {
            let info = net.info.get_mut(&s).expect("info");
            flag(&mut info.flags, "in_cycle");
            info.cycle = path.clone();
            info.ready = false;
        }
        cyc.extend(set);
    }
    for &(x, y) in &sched {
        if cyc.contains(&x) && !cyc.contains(&y) {
            flag(
                &mut net.info.get_mut(&y).expect("info").flags,
                "blocked_by_cycle",
            );
        }
    }

    for &seq in act.difference(&cyc) {
        let t = tasks[&seq];
        let info = &net.info[&seq];
        let snb = t.estimate.as_ref().and_then(|e| e.start_not_before);
        let doing = t.status == Status::Doing;
        let rel = if doing {
            today
        } else {
            today
                .max(snb.unwrap_or(today))
                .max(release.get(&seq).copied().unwrap_or(today))
        };
        let node = Node {
            a: info.a.expect("a"),
            s: info.s.expect("s"),
            assignee: t.assignee.clone(),
            doing,
            priority: t.priority,
            release: rel,
        };
        if t.assignee.is_none() {
            flag(
                &mut net.info.get_mut(&seq).expect("info").flags,
                "unassigned",
            );
        }
        net.preds.insert(seq, BTreeSet::new());
        net.succs.insert(seq, BTreeSet::new());
        net.nodes.insert(seq, node);
    }
    for &(x, y) in &sched {
        if cyc.contains(&x) || cyc.contains(&y) {
            continue;
        }
        net.succs.get_mut(&x).expect("succs").insert(y);
        net.preds.get_mut(&y).expect("preds").insert(x);
    }
    net.ignored.sort();
    net
}

/// Strongly connected components (Tarjan, without recursion). Components in the order they are found.
pub fn tarjan(succ: &BTreeMap<Seq, BTreeSet<Seq>>) -> Vec<Vec<Seq>> {
    let mut index: BTreeMap<Seq, usize> = BTreeMap::new();
    let mut low: BTreeMap<Seq, usize> = BTreeMap::new();
    let mut on_stack: BTreeSet<Seq> = BTreeSet::new();
    let mut stack: Vec<Seq> = Vec::new();
    let mut out = Vec::new();
    let mut next = 0usize;
    for &root in succ.keys() {
        if index.contains_key(&root) {
            continue;
        }
        // Frame: a vertex and the list of its successors with a position.
        let mut frames: Vec<(Seq, Vec<Seq>, usize)> = Vec::new();
        index.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        frames.push((root, succ[&root].iter().copied().collect(), 0));
        while let Some((v, ws, i)) = frames.last_mut() {
            let v = *v;
            if *i < ws.len() {
                let w = ws[*i];
                *i += 1;
                if let std::collections::btree_map::Entry::Vacant(e) = index.entry(w) {
                    e.insert(next);
                    low.insert(w, next);
                    next += 1;
                    stack.push(w);
                    on_stack.insert(w);
                    frames.push((w, succ[&w].iter().copied().collect(), 0));
                } else if on_stack.contains(&w) {
                    let lw = index[&w].min(low[&v]);
                    low.insert(v, lw);
                }
                continue;
            }
            frames.pop();
            if let Some((p, _, _)) = frames.last() {
                let lp = low[p].min(low[&v]);
                low.insert(*p, lp);
            }
            if low[&v] == index[&v] {
                let mut comp = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort();
                out.push(comp);
            }
        }
    }
    out
}

/// A cycle inside a component from the smallest seq back to it (the shortest by BFS).
fn cycle_path(succ: &BTreeMap<Seq, BTreeSet<Seq>>, comp: &BTreeSet<Seq>) -> Vec<Seq> {
    let start = *comp.iter().next().expect("component is not empty");
    let mut prev: BTreeMap<Seq, Seq> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(x) = queue.pop_front() {
        for &y in &succ[&x] {
            if !comp.contains(&y) {
                continue;
            }
            if y == start {
                let mut path = vec![x];
                let mut cur = x;
                while cur != start {
                    cur = prev[&cur];
                    path.push(cur);
                }
                path.reverse();
                path.push(start);
                return path;
            }
            if let std::collections::btree_map::Entry::Vacant(e) = prev.entry(y) {
                e.insert(x);
                queue.push_back(y);
            }
        }
    }
    vec![start, start]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(edges: &[(Seq, Seq)], n: Seq) -> BTreeMap<Seq, BTreeSet<Seq>> {
        let mut m: BTreeMap<Seq, BTreeSet<Seq>> = (1..=n).map(|s| (s, BTreeSet::new())).collect();
        for &(a, b) in edges {
            m.get_mut(&a).unwrap().insert(b);
        }
        m
    }

    #[test]
    fn tarjan_finds_components() {
        let m = g(&[(1, 2), (2, 3), (3, 1), (3, 4), (4, 5), (5, 4), (6, 6)], 7);
        let mut comps = tarjan(&m);
        comps.sort();
        assert_eq!(comps, vec![vec![1, 2, 3], vec![4, 5], vec![6], vec![7]]);
    }

    #[test]
    fn cycle_path_returns_to_start() {
        let m = g(&[(1, 2), (2, 3), (3, 1)], 3);
        let comp: BTreeSet<Seq> = [1, 2, 3].into();
        assert_eq!(cycle_path(&m, &comp), vec![1, 2, 3, 1]);
        let m = g(&[(2, 2)], 2);
        assert_eq!(cycle_path(&m, &[2].into()), vec![2, 2]);
    }
}
