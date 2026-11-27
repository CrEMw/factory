//! Critical chain (CCPM) of a scope: a pure computation without I/O or clocks (spec "Critical
//! chain", rules 1-14). The `plan.*` tables, recomputation and routes are wired in by the next step of stage 9.
//!
//! Computation order: network (`network`), forward SSGS (`schedule::forward`), critical chain (`chain`), subnets and buffers
//! (`buffers`), final as-late-as-possible placement with buffers (`schedule::place`), baseline and fever
//! (`fever`), WSJF (`wsjf`). The output is deterministic: only `BTreeMap`, sorted by seq.

pub mod buffers;
pub mod chain;
pub mod fever;
pub mod model;
pub mod network;
pub mod next;
pub mod schedule;
pub mod wsjf;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use buffers::Entry;
use model::{
    Baseline, BufferKind, BufferPlan, Flags, HalfDay, Mode, Plan, PlanConfig, PlanInput, Seq,
    TaskIn, TaskPlan,
};
use network::flag;
use schedule::{Layout, Slot};

/// Algorithm version: part of `input_hash`, changes with any change of the computed result.
pub const CCPM_ALGO_VERSION: u32 = 2;

pub fn plan(
    input: &PlanInput,
    baseline: Option<&Baseline>,
    today: HalfDay,
    cfg: &PlanConfig,
) -> Plan {
    let mut net = network::build(input, today, cfg);
    let mut by_seq: BTreeMap<Seq, &TaskIn> = BTreeMap::new();
    for t in &input.tasks {
        by_seq.entry(t.seq).or_insert(t);
    }

    // Forward SSGS and resource sequences.
    let mut free: BTreeMap<String, HalfDay> = BTreeMap::new();
    let (fwd, multi) =
        schedule::forward(&net.nodes, &net.preds, today, &mut free, &BTreeMap::new());
    let res = schedule::resource_order(&net.nodes, &fwd);
    let mut res_prev: BTreeMap<Seq, Seq> = BTreeMap::new();
    let mut gsucc: BTreeMap<Seq, BTreeSet<Seq>> = BTreeMap::new();
    let mut gpred: BTreeMap<Seq, BTreeSet<Seq>> = BTreeMap::new();
    for &s in net.nodes.keys() {
        gsucc.insert(
            s,
            net.succs[&s]
                .iter()
                .copied()
                .filter(|y| net.nodes.contains_key(y))
                .collect(),
        );
        gpred.insert(s, net.preds[&s].clone());
    }
    for seqs in res.values() {
        for w in seqs.windows(2) {
            res_prev.insert(w[1], w[0]);
            gsucc.get_mut(&w[0]).expect("gsucc").insert(w[1]);
            gpred.get_mut(&w[1]).expect("gpred").insert(w[0]);
        }
    }
    let mut order: Vec<Seq> = net.nodes.keys().copied().collect();
    order.sort_by_key(|s| (fwd[s].start, *s));

    // Critical chain: fresh from the forward schedule; with a baseline, its members come from the baseline.
    let fresh = chain::extract(&fwd, &net.preds, &res_prev);
    let mut flags = Flags::new();
    let links = match baseline {
        None => fresh.clone(),
        Some(b) => {
            let mut seqs: Vec<Seq> = b
                .chain
                .iter()
                .copied()
                .filter(|s| net.nodes.contains_key(s))
                .collect();
            seqs.sort_by_key(|s| (fwd[s].start, *s));
            let fresh_set: BTreeSet<Seq> = fresh.iter().map(|l| l.seq).collect();
            // An unfinished baseline link dropped out of the schedule (in a cycle, became an umbrella):
            // the chain is no longer the approved one.
            let dropped = b.chain.iter().any(|s| {
                !net.nodes.contains_key(s) && by_seq.get(s).is_some_and(|t| !t.status.finished())
            });
            if dropped || fresh_set != seqs.iter().copied().collect() {
                flag(&mut flags, "chain_drift");
            }
            chain::links_for(&seqs, &net.preds, |s| net.nodes[&s].assignee.clone())
        }
    };
    let chain_set: BTreeSet<Seq> = links.iter().map(|l| l.seq).collect();
    // The largest end of the links: with a baseline the links may not be back to back (drift, doing in place).
    let chain_end = links
        .iter()
        .map(|l| fwd[&l.seq].end)
        .max()
        .or_else(|| fwd.values().map(|s| s.end).max())
        .unwrap_or(today);

    // Subnets and buffer sizes.
    let member = buffers::membership(&order, &chain_set, &gsucc, &fwd);
    let mut groups: BTreeMap<Entry, Vec<Seq>> = BTreeMap::new();
    for &x in &order {
        if let Some(&e) = member.get(&x) {
            groups.entry(e).or_default().push(x);
        }
    }
    let a: BTreeMap<Seq, i64> = net.nodes.iter().map(|(&s, n)| (s, n.a)).collect();
    let d = |path: &[Seq]| {
        path.iter()
            .map(|s| net.nodes[s].s - net.nodes[s].a)
            .collect::<Vec<_>>()
    };
    let fb_size: BTreeMap<Entry, i64> = groups
        .iter()
        .map(|(&e, g)| {
            let from_base = baseline
                .and_then(|b| b.feeding.iter().find(|f| f.key == e.key()))
                .map(|f| f.size);
            let size = from_base.unwrap_or_else(|| {
                buffers::size(cfg.buffer_method, &d(&buffers::longest_path(g, &gpred, &a)))
            });
            (e, size)
        })
        .collect();
    let chain_seqs: Vec<Seq> = links.iter().map(|l| l.seq).collect();
    let pb = match baseline {
        Some(b) if b.pb_size > 0 => b.pb_size,
        _ if chain_seqs.is_empty() => 0,
        _ => buffers::size(cfg.buffer_method, &d(&chain_seqs)),
    };

    // Final placement (in backward mode from the due date).
    let mut shift0 = 0;
    if let (Mode::Backward, Some(due)) = (cfg.mode, cfg.due) {
        shift0 = due - (chain_end + pb);
    }
    let layout = Layout {
        nodes: &net.nodes,
        order: &order,
        fwd: &fwd,
        pred: &gpred,
        succ: &gsucc,
        chain: &chain_set,
        chain_end,
        member: &member,
        fb_size: &fb_size,
        pb,
    };
    let placed = schedule::place(&layout, shift0);
    if !placed.converged {
        flag(&mut flags, "placement_unconverged");
    }
    // The whole scope is done and there is a baseline: the PB fever point at the actual end of the chain.
    let done_pb = baseline
        .filter(|b| net.nodes.is_empty() && b.pb_size > 0)
        .map(|b| {
            let fc = b
                .chain
                .iter()
                .filter_map(|s| by_seq.get(s).and_then(|t| t.done_at))
                .max()
                .unwrap_or(today);
            (b.pb_size, fc)
        });
    let end = match (net.nodes.is_empty(), done_pb) {
        (true, Some((_, fc))) => fc,
        (true, None) => today,
        (false, _) => placed.pb.end,
    };
    let late_by = match (cfg.mode, cfg.due) {
        (Mode::Backward, Some(due)) => (end - due).max(0),
        // Critical chain shift or a PB pushed by the finish subnet: how much later PB is than the chain end in the forward schedule.
        _ => placed.shift.max(placed.pb.start - chain_end).max(0),
    };
    if late_by > 0 {
        flag(&mut flags, "late");
    }

    let slots = &placed.slots;
    let mut gaps: BTreeSet<Seq> = BTreeSet::new();
    for w in chain_seqs.windows(2) {
        if slots[&w[1]].start > slots[&w[0]].end {
            gaps.insert(w[1]);
        }
    }

    // Projected task ends for FB fever: start no earlier than the baseline (the subnet sits as late as possible
    // and would otherwise move with the critical chain), no earlier than the forward schedule and the ends of predecessors.
    let mut proj: BTreeMap<Seq, HalfDay> = BTreeMap::new();
    if let Some(b) = baseline {
        for &x in &order {
            let n = &net.nodes[&x];
            let f = fwd[&x];
            let mut st = f.start;
            if !n.doing {
                st = st.max(b.dates.get(&x).map_or(f.start, |d| d.0));
            }
            st = gpred[&x]
                .iter()
                .filter_map(|p| proj.get(p))
                .fold(st, |m, &e| m.max(e));
            proj.insert(x, st + n.a);
        }
    }

    // Buffers.
    let mut bufs: Vec<BufferPlan> = Vec::new();
    let pb_slot = match done_pb {
        Some((size, fc)) => Some((size, Slot { start: fc, end: fc })),
        None if !net.nodes.is_empty() => Some((pb, placed.pb)),
        None => None,
    };
    if let Some((size, slot)) = pb_slot {
        bufs.push(BufferPlan {
            key: "pb".into(),
            kind: BufferKind::Project,
            entry: None,
            tasks: chain_seqs.clone(),
            size,
            start: slot.start,
            end: slot.end,
            compressed: false,
            chain_complete_pct: None,
            consumed_pct: None,
            zone: None,
            penetration: None,
        });
    }
    for (&e, g) in &groups {
        let slot = placed.fb[&e];
        let size = fb_size[&e];
        let mut tasks = g.clone();
        tasks.sort();
        let compressed = slot.end - slot.start < size;
        if compressed {
            flag(&mut flags, "compressed");
        }
        bufs.push(BufferPlan {
            key: e.key(),
            kind: BufferKind::Feeding,
            entry: match e {
                Entry::Chain(s) => Some(s),
                Entry::Finish => None,
            },
            tasks,
            size,
            start: slot.start,
            end: slot.end,
            compressed,
            chain_complete_pct: None,
            consumed_pct: None,
            zone: None,
            penetration: None,
        });
    }

    // Tasks.
    let base_tasks: Option<BTreeSet<Seq>> = baseline.map(|b| b.tasks.iter().copied().collect());
    let mut tasks: Vec<TaskPlan> = Vec::new();
    for (&seq, t) in &by_seq {
        let info = net.info.remove(&seq).unwrap_or_default();
        let mut fl = info.flags;
        let slot = if info.umbrella {
            let ls = &net.umbrellas[&seq];
            let st = ls
                .iter()
                .filter_map(|l| slots.get(l))
                .map(|s| s.start)
                .min();
            let en = ls.iter().filter_map(|l| slots.get(l)).map(|s| s.end).max();
            st.zip(en).map(|(start, end)| Slot { start, end })
        } else {
            slots.get(&seq).copied()
        };
        if multi.contains(&seq) {
            flag(&mut fl, "multitasking");
        }
        if gaps.contains(&seq) {
            flag(&mut fl, "chain_gap");
        }
        let wsjf = wsjf::task(t);
        if info.a.is_some() && !t.status.finished() && wsjf.is_none() {
            flag(&mut fl, "wsjf_missing");
        }
        if let Some(bt) = &base_tasks
            && info.a.is_some()
            && !bt.contains(&seq)
        {
            flag(&mut fl, "scope_change");
        }
        tasks.push(TaskPlan {
            seq,
            code: t.code.clone(),
            status: t.status,
            assignee: t.assignee.clone(),
            a: info.a,
            a_full: info.a_full,
            s: info.s,
            start: slot.map(|s| s.start),
            end: slot.map(|s| s.end),
            on_chain: chain_set.contains(&seq),
            buffer: member.get(&seq).map(|e| e.key()),
            ready: info.ready,
            wsjf,
            flags: fl,
            cycle: info.cycle,
        });
    }

    let chain_a: i64 = chain_seqs.iter().map(|s| net.nodes[s].a).sum();
    let defaulted_a: i64 = tasks
        .iter()
        .filter(|t| t.on_chain && t.flags.contains("estimate_defaulted"))
        .filter_map(|t| t.a)
        .sum();
    let defaulted_pct = if chain_a > 0 {
        defaulted_a * 100 / chain_a
    } else {
        0
    };
    let leaves = by_seq.values().copied().filter(|t| {
        tasks
            .binary_search_by_key(&t.seq, |p| p.seq)
            .is_ok_and(|i| tasks[i].a.is_some())
    });

    let mut ignored = std::mem::take(&mut net.ignored);
    ignored.sort();
    let mut out = Plan {
        algo_version: CCPM_ALGO_VERSION,
        root: input.root,
        today,
        end,
        late_by,
        defaulted_pct,
        approvable: defaulted_pct <= cfg.max_defaulted_pct,
        wsjf_root: wsjf::root(leaves),
        flags,
        tasks,
        chain: links,
        buffers: bufs,
        ignored_edges: ignored,
        resource_warnings: Vec::new(),
    };
    if let Some(b) = baseline {
        fever::apply(&mut out, b, &by_seq, &proj, cfg);
    }
    out.resource_warnings = next::resource_warnings(&out, &by_seq, cfg);
    out
}
