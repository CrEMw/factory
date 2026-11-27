//! Golden fixtures (`fixtures/*.json`, computed by hand, the computation is in the `comment` field) and schedule properties
//! on random networks (proptest).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use proptest::prelude::*;
use serde::Deserialize;

use super::model::{
    BlockEdge, Estimate, HalfDay, Plan, PlanConfig, PlanInput, Seq, Status, TaskIn, Zone,
};
use super::next::{OutsideTask, next_task};
use super::{fever, plan};

#[derive(Deserialize)]
struct Fixture {
    name: String,
    today: HalfDay,
    #[serde(default)]
    cfg: PlanConfig,
    input: PlanInput,
    #[serde(default)]
    baseline: Option<BaselineFrom>,
    expect: Expect,
    #[serde(default)]
    next: Vec<NextExpect>,
}

#[derive(Deserialize)]
struct BaselineFrom {
    today: HalfDay,
    input: PlanInput,
    expect: BaselineExpect,
}

#[derive(Deserialize)]
struct BaselineExpect {
    chain: Vec<Seq>,
    chain_end: HalfDay,
    pb_size: i64,
    feeding: Vec<super::model::BaselineFeeding>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Expect {
    chain: Option<Vec<Seq>>,
    resource_links: Option<Vec<Seq>>,
    end: Option<HalfDay>,
    late_by: Option<i64>,
    defaulted_pct: Option<i64>,
    approvable: Option<bool>,
    wsjf_root: Option<i64>,
    tasks: BTreeMap<Seq, Option<(HalfDay, HalfDay)>>,
    a: BTreeMap<Seq, i64>,
    wsjf: BTreeMap<Seq, Option<i64>>,
    buffers: BTreeMap<String, BufExpect>,
    flags: BTreeMap<Seq, Vec<String>>,
    plan_flags: Option<Vec<String>>,
    cycle: BTreeMap<Seq, Vec<String>>,
    ignored: Option<Vec<(Seq, Seq)>>,
    ready: Option<Vec<Seq>>,
    no_buffer: Vec<Seq>,
}

#[derive(Deserialize)]
struct BufExpect {
    size: Option<i64>,
    start: Option<HalfDay>,
    end: Option<HalfDay>,
    tasks: Option<Vec<Seq>>,
    compressed: Option<bool>,
    zone: Option<Zone>,
    chain_complete_pct: Option<i64>,
    consumed_pct: Option<i64>,
    penetration: Option<i64>,
}

#[derive(Deserialize)]
struct NextExpect {
    assignee: String,
    seq: Option<Seq>,
    flags: Vec<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ccpm/fixtures")
}

fn check(f: &Fixture) {
    let base = f.baseline.as_ref().map(|b| {
        let p = plan(&b.input, None, b.today, &f.cfg);
        let base = fever::baseline(&p);
        assert_eq!(base.chain, b.expect.chain, "{}: baseline chain", f.name);
        assert_eq!(
            base.chain_end, b.expect.chain_end,
            "{}: baseline chain end",
            f.name
        );
        assert_eq!(base.pb_size, b.expect.pb_size, "{}: baseline PB", f.name);
        assert_eq!(base.feeding, b.expect.feeding, "{}: baseline FB", f.name);
        base
    });
    let p = plan(&f.input, base.as_ref(), f.today, &f.cfg);
    let n = &f.name;
    let e = &f.expect;
    let dump = serde_json::to_string_pretty(&p).unwrap();
    let ctx = |what: &str| format!("{n}: {what}\n{dump}");
    if let Some(c) = &e.chain {
        assert_eq!(
            &p.chain.iter().map(|l| l.seq).collect::<Vec<_>>(),
            c,
            "{}",
            ctx("chain")
        );
    }
    if let Some(r) = &e.resource_links {
        let got: Vec<Seq> = p
            .chain
            .iter()
            .filter(|l| l.resource_link)
            .map(|l| l.seq)
            .collect();
        assert_eq!(&got, r, "{}", ctx("resource links"));
    }
    if let Some(v) = e.end {
        assert_eq!(p.end, v, "{}", ctx("due date"));
    }
    if let Some(v) = e.late_by {
        assert_eq!(p.late_by, v, "{}", ctx("late_by"));
    }
    if let Some(v) = e.defaulted_pct {
        assert_eq!(p.defaulted_pct, v, "{}", ctx("defaulted_pct"));
    }
    if let Some(v) = e.approvable {
        assert_eq!(p.approvable, v, "{}", ctx("approvable"));
    }
    if let Some(v) = e.wsjf_root {
        assert_eq!(p.wsjf_root, Some(v), "{}", ctx("wsjf_root"));
    }
    for (seq, want) in &e.tasks {
        let t = p
            .task(*seq)
            .unwrap_or_else(|| panic!("{}", ctx(&format!("no task {seq}"))));
        assert_eq!(
            t.start.zip(t.end),
            *want,
            "{}",
            ctx(&format!("interval {seq}"))
        );
    }
    for (seq, want) in &e.a {
        assert_eq!(
            p.task(*seq).unwrap().a,
            Some(*want),
            "{}",
            ctx(&format!("a {seq}"))
        );
    }
    for (seq, want) in &e.wsjf {
        assert_eq!(
            p.task(*seq).unwrap().wsjf,
            *want,
            "{}",
            ctx(&format!("wsjf {seq}"))
        );
    }
    for (key, b) in &e.buffers {
        let got = p
            .buffer(key)
            .unwrap_or_else(|| panic!("{}", ctx(&format!("no buffer {key}"))));
        let pairs = [
            (b.size, Some(got.size), "size"),
            (b.start, Some(got.start), "start"),
            (b.end, Some(got.end), "end"),
            (
                b.chain_complete_pct,
                got.chain_complete_pct,
                "chain_complete_pct",
            ),
            (b.consumed_pct, got.consumed_pct, "consumed_pct"),
            (b.penetration, got.penetration, "penetration"),
        ];
        for (want, have, what) in pairs {
            if want.is_some() {
                assert_eq!(have, want, "{}", ctx(&format!("{key}.{what}")));
            }
        }
        if let Some(t) = &b.tasks {
            assert_eq!(&got.tasks, t, "{}", ctx(&format!("{key}.tasks")));
        }
        if let Some(c) = b.compressed {
            assert_eq!(got.compressed, c, "{}", ctx(&format!("{key}.compressed")));
        }
        if b.zone.is_some() {
            assert_eq!(got.zone, b.zone, "{}", ctx(&format!("{key}.zone")));
        }
    }
    for (seq, fl) in &e.flags {
        let t = p.task(*seq).unwrap();
        for x in fl {
            assert!(
                t.flags.contains(x),
                "{}",
                ctx(&format!("flag {x} on {seq}"))
            );
        }
    }
    if let Some(fl) = &e.plan_flags {
        let want: BTreeSet<String> = fl.iter().cloned().collect();
        assert_eq!(p.flags, want, "{}", ctx("plan flags"));
    }
    for (seq, path) in &e.cycle {
        assert_eq!(&p.task(*seq).unwrap().cycle, path, "{}", ctx("cycle path"));
    }
    if let Some(ig) = &e.ignored {
        let got: Vec<(Seq, Seq)> = p.ignored_edges.iter().map(|x| (x.src, x.dst)).collect();
        assert_eq!(&got, ig, "{}", ctx("excluded edges"));
    }
    if let Some(r) = &e.ready {
        let got: Vec<Seq> = p.tasks.iter().filter(|t| t.ready).map(|t| t.seq).collect();
        assert_eq!(&got, r, "{}", ctx("ready"));
    }
    for seq in &e.no_buffer {
        assert_eq!(p.task(*seq).unwrap().buffer, None, "{}", ctx("buffer"));
    }
    for nx in &f.next {
        let got = next_task(&nx.assignee, std::slice::from_ref(&p), &[]);
        assert_eq!(
            got.seq,
            nx.seq,
            "{}",
            ctx(&format!("next_task {}", nx.assignee))
        );
        let want: BTreeSet<String> = nx.flags.iter().cloned().collect();
        assert_eq!(got.flags, want, "{}", ctx("next_task flags"));
    }
    check_invariants(&f.input, &p, f.today, true);
}

#[test]
fn golden_fixtures() {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.ends_with(".table.json") {
            continue;
        }
        let f: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format!("{}.json", f.name), name);
        check(&f);
        names.push(f.name);
    }
    names.sort();
    assert_eq!(
        names,
        [
            "backward_due",
            "backward_late",
            "baseline_drift",
            "cycle",
            "defaults_and_wsjf",
            "external_blocker",
            "external_tail_open_blocker",
            "feeding_diamond",
            "resource_conflict",
            "sink_branch",
            "two_doing",
            "umbrella"
        ]
    );
}

#[test]
fn zone_table() {
    #[derive(Deserialize)]
    struct Table {
        rows: Vec<(i64, i64, Zone)>,
    }
    let t: Table = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("zones.table.json")).unwrap(),
    )
    .unwrap();
    let cfg = PlanConfig::default();
    for (x, c, z) in t.rows {
        assert_eq!(fever::zone(&cfg, x, c), z, "x={x} consumed={c}");
    }
}

#[test]
fn next_task_orders_by_zone_then_wsjf() {
    // Outside scopes: equal by buffer, ordered by WSJF descending, without WSJF last.
    let out = |seq, wsjf| OutsideTask {
        seq,
        code: format!("T{seq}"),
        assignee: "alice".into(),
        status: Status::Open,
        ready: true,
        wsjf,
    };
    let got = next_task(
        "alice",
        &[],
        &[out(1, None), out(2, Some(150)), out(3, Some(333))],
    );
    assert_eq!(got.seq, Some(3));
    let got = next_task("alice", &[], &[out(1, None), out(4, None)]);
    assert_eq!(got.seq, Some(1));
    let mut not_ready = out(5, Some(999));
    not_ready.ready = false;
    assert_eq!(next_task("alice", &[], &[not_ready.clone()]).seq, None);
    // The red zone of a baseline overtakes a task outside scopes with any WSJF.
    let f: Fixture = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("baseline_drift.json")).unwrap(),
    )
    .unwrap();
    let b = f.baseline.as_ref().unwrap();
    let base = fever::baseline(&plan(&b.input, None, b.today, &f.cfg));
    let p = plan(&f.input, Some(&base), f.today, &f.cfg);
    let mut o = out(9, Some(9999));
    o.assignee = "carol".into();
    let got = next_task("carol", &[p], &[o]);
    assert_eq!(got.seq, Some(4), "N in the red FB");
}

/// A task for short scenarios: open, an estimate of `a` half-days, no WSJF.
fn t(seq: Seq, who: &str, a: i64) -> TaskIn {
    TaskIn {
        seq,
        code: format!("T{seq}"),
        status: Status::Open,
        priority: None,
        tags: vec![],
        assignee: Some(who.to_string()),
        estimate: Some(Estimate {
            effort_sp: 1,
            aggressive_hd: Some(a),
            safe_hd: None,
            remaining_hd: None,
            start_not_before: None,
        }),
        wsjf: None,
        part_of: None,
        duplicates: None,
        done_at: None,
    }
}

fn doing(mut x: TaskIn, remaining: i64) -> TaskIn {
    x.status = Status::Doing;
    x.estimate.as_mut().unwrap().remaining_hd = Some(remaining);
    x
}

fn net(root: Seq, tasks: Vec<TaskIn>, edges: &[(Seq, Seq)]) -> PlanInput {
    PlanInput {
        root,
        tasks,
        blocks: edges
            .iter()
            .map(|&(src, dst)| BlockEdge { src, dst })
            .collect(),
        external: vec![],
    }
}

fn approve(input: &PlanInput, today: HalfDay) -> super::model::Baseline {
    fever::baseline(&plan(input, None, today, &PlanConfig::default()))
}

/// Core review remarks (scenarios computed by hand in the comments).
#[test]
fn fb_fever_ignores_chain_slip() {
    // A(alice,10) -> B(alice,4), C(bob,2) -> B. Baseline: C [7,9), FB [9,10), subnet end 9.
    let input = net(
        1,
        vec![t(1, "alice", 10), t(2, "alice", 4), t(3, "bob", 2)],
        &[(1, 2), (3, 2)],
    );
    let base = approve(&input, 0);
    assert_eq!(base.feeding[0].end, 9);
    // today 4, A started with 10 remaining: the critical chain slipped by 4, C unchanged, the FB is not consumed.
    let mut moved = input.clone();
    moved.tasks[0] = doing(t(1, "alice", 10), 10);
    let p = plan(&moved, Some(&base), 4, &PlanConfig::default());
    let fb = p.buffer("fb:2").unwrap();
    assert_eq!(fb.consumed_pct, Some(0));
    assert_eq!(fb.zone, Some(Zone::Green));
    assert!(p.buffer("pb").unwrap().consumed_pct.unwrap() > 0);
}

#[test]
fn pb_starts_after_latest_chain_task() {
    // Baseline: 1(alice,4) -> 2(bob,3), PB 4. The edge is removed, 1 grew to 10: PB from 10, not from the end of 2.
    let input = net(1, vec![t(1, "alice", 4), t(2, "bob", 3)], &[(1, 2)]);
    let base = approve(&input, 0);
    assert_eq!(
        (base.chain.clone(), base.chain_end, base.pb_size),
        (vec![1, 2], 7, 4)
    );
    let moved = net(1, vec![t(1, "alice", 10), t(2, "bob", 3)], &[]);
    let p = plan(&moved, Some(&base), 0, &PlanConfig::default());
    let pb = p.buffer("pb").unwrap();
    assert_eq!((pb.start, pb.end, p.end), (10, 14, 14));
    assert_eq!(pb.consumed_pct, Some(75));
    assert!(p.flags.contains("chain_drift"));
}

#[test]
fn dropped_chain_task_is_drift() {
    // Baseline [1,2] (1 -> 2), chain end 7, PB 4. Then 2 got into the cycle 2 -> 3 -> 2: a baseline link outside the schedule,
    // `chain_drift`.
    let input = net(1, vec![t(1, "alice", 4), t(2, "bob", 3)], &[(1, 2)]);
    let base = approve(&input, 0);
    let moved = net(
        1,
        vec![t(1, "alice", 4), t(2, "bob", 3), t(3, "carol", 1)],
        &[(1, 2), (2, 3), (3, 2)],
    );
    let p = plan(&moved, Some(&base), 0, &PlanConfig::default());
    assert!(p.flags.contains("chain_drift"));
    assert_eq!(p.task(2).unwrap().start, None);
    assert!(p.task(2).unwrap().flags.contains("in_cycle"));
}

#[test]
fn undated_external_blocker_consumes_buffer() {
    // Baseline [1,2] (1 -> 2), chain end 7, PB 4. Then 1 has an open external blocker without a date, after
    // 3 half-days: the milestone is at today, 1 [3,7), 2 [7,10), the same chain (no `chain_drift`), PB [10,14) and plan end 14
    // no earlier than the end of any task; consumption (10 - 7) / 4 = 75%.
    let input = net(1, vec![t(1, "alice", 4), t(2, "bob", 3)], &[(1, 2)]);
    let base = approve(&input, 0);
    let mut moved = input.clone();
    moved.blocks.push(BlockEdge { src: 77, dst: 1 });
    moved.external.push(super::model::ExternalIn {
        seq: 77,
        code: "X".into(),
        status: Status::Open,
        forecast_end: None,
    });
    let p = plan(&moved, Some(&base), 3, &PlanConfig::default());
    assert!(!p.flags.contains("chain_drift"));
    let t1 = p.task(1).unwrap();
    assert!(t1.flags.contains("external_blocker") && t1.on_chain && !t1.ready);
    assert_eq!(t1.start.zip(t1.end), Some((3, 7)));
    let pb = p.buffer("pb").unwrap();
    assert_eq!((pb.start, pb.end, p.end), (10, 14, 14));
    assert_eq!(pb.consumed_pct, Some(75));
    check_invariants(&moved, &p, 3, true);
}

#[test]
fn umbrella_exits_skip_edges_into_doing() {
    // Umbrella 9 of 6 (open, 10) and 7 (doing), edge 6 -> 7 excluded, exits 6 and 7; 9 -> 10.
    let mut leaves = vec![
        t(6, "alice", 10),
        doing(t(7, "bob", 3), 3),
        t(9, "alice", 1),
        t(10, "carol", 1),
    ];
    leaves[0].part_of = Some(9);
    leaves[1].part_of = Some(9);
    let input = net(1, leaves, &[(6, 7), (9, 10)]);
    let p = plan(&input, None, 0, &PlanConfig::default());
    let (six, ten) = (p.task(6).unwrap(), p.task(10).unwrap());
    assert!(
        six.end.unwrap() <= ten.start.unwrap(),
        "10 before the end of 6"
    );
    assert!(!ten.ready);
    // 7 is done, 6 is still open: 10 is not ready.
    let mut moved = input.clone();
    moved.tasks[1].status = Status::Done;
    let p = plan(&moved, None, 0, &PlanConfig::default());
    assert!(!p.task(10).unwrap().ready);
}

#[test]
fn nested_scopes_do_not_overlap_with_self() {
    // Task 1 in umbrella 50 under root 100, plans for 100 and 50: a task does not overlap itself.
    let mut one = t(1, "alice", 2);
    one.part_of = Some(50);
    let mut mid = t(50, "alice", 1);
    mid.part_of = Some(100);
    let outer = plan(
        &net(100, vec![one.clone(), mid], &[]),
        None,
        0,
        &PlanConfig::default(),
    );
    let inner = plan(&net(50, vec![one], &[]), None, 0, &PlanConfig::default());
    let got = next_task("alice", &[outer, inner], &[]);
    assert_eq!(got.seq, Some(1));
    assert!(!got.flags.contains("cross_scope_overlap"));
}

#[test]
fn cancelled_chain_task_and_done_scope() {
    // Baseline [1,2] (alice, 2 each), chain end 4, PB 2. 1 done at 3, 2 cancelled: the chain is 100% complete,
    // the PB point exists even with an empty network, projection 3, consumption (3 - 4) / 2 = -50%.
    let input = net(1, vec![t(1, "alice", 2), t(2, "alice", 2)], &[(1, 2)]);
    let base = approve(&input, 0);
    let mut moved = input.clone();
    moved.tasks[0].status = Status::Done;
    moved.tasks[0].done_at = Some(3);
    moved.tasks[1].status = Status::Cancelled;
    let p = plan(&moved, Some(&base), 5, &PlanConfig::default());
    let pb = p.buffer("pb").unwrap();
    assert_eq!(
        (pb.chain_complete_pct, pb.consumed_pct),
        (Some(100), Some(-50))
    );
    assert_eq!((pb.start, pb.end, p.end), (3, 3, 3));
    assert!(!p.task(1).unwrap().flags.contains("wsjf_missing"));
    assert!(!p.task(2).unwrap().flags.contains("wsjf_missing"));
}

#[test]
fn baseline_keeps_full_estimate_of_doing() {
    // doing 10 with 2 remaining at approval: the baseline remembers 10, later a remainder of 1 gives 90%.
    let input = net(1, vec![doing(t(1, "alice", 10), 2)], &[]);
    let base = approve(&input, 0);
    assert_eq!(base.a[&1], 10);
    let moved = net(1, vec![doing(t(1, "alice", 10), 1)], &[]);
    let p = plan(&moved, Some(&base), 1, &PlanConfig::default());
    assert_eq!(p.buffer("pb").unwrap().chain_complete_pct, Some(90));
}

#[test]
fn next_task_without_zone_prefers_wsjf() {
    // Without a baseline there is no zone: the task with the larger WSJF goes first, inside the scope and outside it.
    let w = |b| super::model::WsjfIn {
        business_value: b,
        time_criticality: b,
        risk_opportunity: b,
    };
    let mut low = t(1, "alice", 1);
    low.wsjf = Some(w(1));
    let mut high = t(2, "alice", 1);
    high.wsjf = Some(w(8));
    let p = plan(
        &net(100, vec![low.clone(), high], &[]),
        None,
        0,
        &PlanConfig::default(),
    );
    assert_eq!(
        next_task("alice", std::slice::from_ref(&p), &[]).seq,
        Some(2)
    );
    let p = plan(&net(100, vec![low], &[]), None, 0, &PlanConfig::default());
    let outside = OutsideTask {
        seq: 3,
        code: "T3".into(),
        assignee: "alice".into(),
        status: Status::Open,
        ready: true,
        wsjf: Some(1000),
    };
    assert_eq!(next_task("alice", &[p], &[outside]).seq, Some(3));
}

/// Properties of the final schedule (spec "Verification", cargo, proptest). `strict`: the chain is contiguous and FB is before
/// the entry point (without a baseline; with a baseline, after progress the links may drift apart, that is `chain_drift`).
fn check_invariants(input: &PlanInput, p: &Plan, today: HalfDay, strict: bool) {
    assert!(
        !p.flags.contains("placement_unconverged"),
        "placement did not converge"
    );
    let by: BTreeMap<Seq, &TaskIn> = input.tasks.iter().map(|t| (t.seq, t)).collect();
    let ignored: BTreeSet<(Seq, Seq)> = p.ignored_edges.iter().map(|e| (e.src, e.dst)).collect();
    let leaf = |s: Seq| p.task(s).filter(|t| !t.flags.contains("umbrella"));
    // Precedence (except started_while_blocked); umbrella edges are checked through leaves in the fixtures.
    for e in &input.blocks {
        if ignored.contains(&(e.src, e.dst)) {
            continue;
        }
        if let (Some(x), Some(y)) = (leaf(e.src), leaf(e.dst))
            && let (Some(xe), Some(ys)) = (x.end, y.start)
        {
            assert!(xe <= ys, "precedence {} -> {}: {xe} > {ys}", e.src, e.dst);
        }
    }
    // An assignee has no overlaps.
    let mut by_who: BTreeMap<&str, Vec<(HalfDay, HalfDay, Seq)>> = BTreeMap::new();
    for t in &p.tasks {
        if let (Some(w), Some(s), Some(e), None) =
            (&t.assignee, t.start, t.end, t.flags.get("umbrella"))
        {
            by_who.entry(w).or_default().push((s, e, t.seq));
        }
    }
    for (w, mut v) in by_who {
        v.sort();
        for pair in v.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0,
                "{w}: overlap {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
    }
    // Start no earlier than today and snb.
    for t in p.tasks.iter().filter(|t| !t.flags.contains("umbrella")) {
        if let Some(s) = t.start {
            assert!(s >= today, "{} before today", t.seq);
            let snb = by[&t.seq]
                .estimate
                .as_ref()
                .and_then(|e| e.start_not_before);
            if t.status == Status::Open
                && let Some(d) = snb
            {
                assert!(s >= d, "{} before snb", t.seq);
            }
            assert_eq!(Some(s + t.a.unwrap()), t.end, "{} length", t.seq);
        }
    }
    // Every task outside the critical chain is in exactly one FB; the critical chain is in PB.
    let chain: Vec<Seq> = p.chain.iter().map(|l| l.seq).collect();
    let mut owner: BTreeMap<Seq, usize> = BTreeMap::new();
    for b in &p.buffers {
        assert!(b.size >= 1, "buffer {} below 1", b.key);
        assert!(b.start <= b.end, "buffer {} reversed", b.key);
        if b.key != "pb" {
            for s in &b.tasks {
                *owner.entry(*s).or_default() += 1;
                assert_eq!(p.task(*s).unwrap().buffer.as_deref(), Some(b.key.as_str()));
            }
        }
    }
    for t in &p.tasks {
        let planned = t.start.is_some() && !t.flags.contains("umbrella");
        if !planned {
            assert!(!owner.contains_key(&t.seq));
            continue;
        }
        if t.on_chain {
            assert!(
                !owner.contains_key(&t.seq),
                "{} in the critical chain and in an FB",
                t.seq
            );
        } else {
            assert_eq!(
                owner.get(&t.seq),
                Some(&1),
                "{} not in exactly one FB",
                t.seq
            );
        }
    }
    // A cycle gives in_cycle and an empty interval, not an error.
    for t in &p.tasks {
        if t.flags.contains("in_cycle") {
            assert!(t.start.is_none() && !t.cycle.is_empty() && !t.ready);
        }
    }
    // Critical chain without gaps (or the chain_gap flag), PB right after the chain (or the late flag), PB end = due date.
    if let Some(pb) = p.buffer("pb") {
        assert_eq!(pb.end, p.end);
        assert_eq!(pb.tasks, chain);
        // Plan end (PB end, end_date of the scope) no earlier than the end of any scope task.
        for t in &p.tasks {
            if let Some(e) = t.end {
                assert!(e <= p.end, "{} ends after the plan end", t.seq);
            }
        }
        // PB no earlier than the end of any link.
        let chain_end = chain.iter().filter_map(|s| p.task(*s).unwrap().end).max();
        if let Some(ce) = chain_end {
            assert!(pb.start >= ce, "PB before the end of a chain link");
        }
        if !strict {
            return;
        }
        for w in chain.windows(2) {
            let (a, b) = (p.task(w[0]).unwrap(), p.task(w[1]).unwrap());
            assert!(
                a.end == b.start || b.flags.contains("chain_gap"),
                "critical chain gap {} -> {}",
                w[0],
                w[1]
            );
        }
        if let Some(ce) = chain_end {
            assert!(pb.start == ce || p.flags.contains("late"));
        }
        for b in p.buffers.iter().filter(|b| b.key != "pb") {
            let limit = b
                .entry
                .and_then(|e| p.task(e).and_then(|t| t.start))
                .unwrap_or(pb.start);
            assert!(b.end <= limit, "FB {} past the entry point", b.key);
            for s in &b.tasks {
                assert!(
                    p.task(*s).unwrap().end.unwrap() <= limit,
                    "subnet task {s} past the entry point"
                );
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Gen {
    status: u8,
    who: u8,
    a: i64,
    extra: i64,
    remaining: Option<i64>,
    snb: Option<i64>,
    priority: Option<i64>,
    estimated: bool,
    wsjf: Option<(i64, i64, i64)>,
}

fn gen_task() -> impl Strategy<Value = Gen> {
    (
        0u8..12,
        0u8..7,
        1i64..9,
        0i64..9,
        proptest::option::of(0i64..6),
        proptest::option::weighted(0.2, 0i64..30),
        proptest::option::of(0i64..4),
        proptest::bool::weighted(0.8),
        proptest::option::of((1i64..22, 1i64..22, 1i64..22)),
    )
        .prop_map(
            |(status, who, a, extra, remaining, snb, priority, estimated, wsjf)| Gen {
                status,
                who,
                a,
                extra,
                remaining,
                snb,
                priority,
                estimated,
                wsjf,
            },
        )
}

fn to_input(gs: &[Gen], edges: &[(usize, usize)], dag: bool) -> PlanInput {
    let tasks = gs
        .iter()
        .enumerate()
        .map(|(i, g)| TaskIn {
            seq: i as Seq + 1,
            code: format!("T{}", i + 1),
            status: match g.status {
                0 => Status::Doing,
                1 => Status::Done,
                2 => Status::Cancelled,
                _ => Status::Open,
            },
            priority: g.priority,
            tags: if g.a % 2 == 0 {
                vec!["bug".into()]
            } else {
                vec![]
            },
            assignee: (g.who < 6).then(|| {
                ["alice", "bob", "carol", "dave", "eve", "frank"][g.who as usize].to_string()
            }),
            estimate: g.estimated.then(|| Estimate {
                effort_sp: [1, 2, 3, 5, 8, 13, 21][(g.a as usize) % 7],
                aggressive_hd: (g.a % 3 != 0).then_some(g.a),
                safe_hd: (g.extra % 2 == 0).then_some(g.a + g.extra),
                remaining_hd: g.remaining,
                start_not_before: g.snb,
            }),
            wsjf: g.wsjf.map(|(b, c, r)| super::model::WsjfIn {
                business_value: b,
                time_criticality: c,
                risk_opportunity: r,
            }),
            part_of: None,
            duplicates: None,
            done_at: None,
        })
        .collect();
    let blocks = edges
        .iter()
        .filter(|(a, b)| a != b)
        .map(|&(a, b)| {
            let (s, d) = if dag { (a.min(b), a.max(b)) } else { (a, b) };
            BlockEdge {
                src: s as Seq + 1,
                dst: d as Seq + 1,
            }
        })
        .collect();
    PlanInput {
        root: 0,
        tasks,
        blocks,
        external: vec![],
    }
}

fn net_strategy(max: usize) -> impl Strategy<Value = (Vec<Gen>, Vec<(usize, usize)>)> {
    (1usize..=max).prop_flat_map(|n| {
        (
            proptest::collection::vec(gen_task(), n),
            proptest::collection::vec((0..n, 0..n), 0..=2 * n),
        )
    })
}

fn shuffled(input: &PlanInput, k: usize) -> PlanInput {
    let mut x = input.clone();
    x.tasks.reverse();
    let n = x.tasks.len();
    x.tasks.rotate_left(k % n.max(1));
    x.blocks.reverse();
    let m = x.blocks.len();
    x.blocks.rotate_left(k % m.max(1));
    x
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn schedule_properties((gs, edges) in net_strategy(200), today in 0i64..10, k in 0usize..50, backward in proptest::bool::weighted(0.25), due in 0i64..120) {
        let input = to_input(&gs, &edges, true);
        let mut cfg = PlanConfig::default();
        if backward {
            cfg.mode = super::model::Mode::Backward;
            cfg.due = Some(due);
        }
        let p = plan(&input, None, today, &cfg);
        check_invariants(&input, &p, today, true);
        // A shuffled input gives byte-for-byte the same JSON.
        let again = plan(&shuffled(&input, k), None, today, &cfg);
        prop_assert_eq!(serde_json::to_string(&p).unwrap(), serde_json::to_string(&again).unwrap());
        // With a baseline from this same plan: the same properties, the chain does not drift.
        let base = fever::baseline(&p);
        let pb = plan(&input, Some(&base), today, &cfg);
        check_invariants(&input, &pb, today, true);
        prop_assert!(!pb.flags.contains("chain_drift"));
        // next_task returns only a ready task or doing.
        for who in ["alice", "bob", "carol", "dave", "eve", "frank"] {
            let nt = next_task(who, std::slice::from_ref(&p), &[]);
            if let Some(seq) = nt.seq {
                let t = p.task(seq).unwrap();
                prop_assert_eq!(t.assignee.as_deref(), Some(who));
                prop_assert!(t.status == Status::Doing || t.ready, "{} not ready", seq);
            }
        }
    }

    #[test]
    fn cycles_are_flagged_not_errors((gs, edges) in net_strategy(40), today in 0i64..5) {
        let gs: Vec<Gen> = gs.into_iter().map(|mut g| { g.status = 5; g }).collect();
        let input = to_input(&gs, &edges, false);
        let p = plan(&input, None, today, &PlanConfig::default());
        check_invariants(&input, &p, today, true);
        let mut succ: BTreeMap<Seq, BTreeSet<Seq>> = BTreeMap::new();
        for e in &input.blocks {
            succ.entry(e.src).or_default().insert(e.dst);
        }
        for t in &p.tasks {
            // A task is in a cycle if it is reachable from itself.
            let mut seen = BTreeSet::new();
            let mut stack: Vec<Seq> = succ.get(&t.seq).into_iter().flatten().copied().collect();
            let mut cyclic = false;
            while let Some(x) = stack.pop() {
                if x == t.seq { cyclic = true; break; }
                if seen.insert(x) { stack.extend(succ.get(&x).into_iter().flatten().copied()); }
            }
            prop_assert_eq!(t.flags.contains("in_cycle"), cyclic, "{}", t.seq);
            prop_assert_eq!(t.start.is_some(), !cyclic);
        }
    }

    /// Baseline, then progress: some open tasks are done or started, several half-days have passed.
    #[test]
    fn baseline_then_progress((gs, edges) in net_strategy(120), today in 0i64..6, step in 1i64..8, k in 0usize..50) {
        let input = to_input(&gs, &edges, true);
        let cfg = PlanConfig::default();
        let base = fever::baseline(&plan(&input, None, today, &cfg));
        let later = today + step;
        let mut moved = input.clone();
        for (i, t) in moved.tasks.iter_mut().enumerate() {
            if t.status != Status::Open {
                continue;
            }
            match (i + k) % 6 {
                0 => {
                    t.status = Status::Done;
                    t.done_at = Some(later);
                }
                1 => {
                    t.status = Status::Doing;
                    if let Some(e) = t.estimate.as_mut() {
                        e.remaining_hd = Some((i % 4) as i64);
                    }
                }
                2 => t.status = Status::Cancelled,
                _ => {}
            }
        }
        let p = plan(&moved, Some(&base), later, &cfg);
        check_invariants(&moved, &p, later, false);
        let again = plan(&shuffled(&moved, k), Some(&base), later, &cfg);
        prop_assert_eq!(serde_json::to_string(&p).unwrap(), serde_json::to_string(&again).unwrap());
        for b in &p.buffers {
            prop_assert!(b.zone.is_some() == base.feeding.iter().any(|f| f.key == b.key) || b.key == "pb", "{}", b.key);
            if let Some(cc) = b.chain_complete_pct {
                prop_assert!((0..=100).contains(&cc), "{} chain_complete {}", b.key, cc);
            }
        }
    }
}
