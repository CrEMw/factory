//! Feeding subnets and buffer sizes (rules 7-8).

use std::collections::{BTreeMap, BTreeSet};

use super::model::{BufferMethod, Seq};
use super::schedule::Slot;

/// What a subnet feeds: a critical chain link or the virtual finish (FB before PB).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Entry {
    Chain(Seq),
    Finish,
}

impl Entry {
    pub fn key(self) -> String {
        match self {
            Entry::Chain(s) => format!("fb:{s}"),
            Entry::Finish => "fb:finish".to_string(),
        }
    }
}

/// Subnet of every task outside the critical chain: the entry point with the earliest planned start (ties go to the smaller seq),
/// finish only if the task does not reach the critical chain. Reachability by dependencies and resource links.
/// `order` follows the forward start, it is a topological order of the `succ` graph.
pub fn membership(
    order: &[Seq],
    chain: &BTreeSet<Seq>,
    succ: &BTreeMap<Seq, BTreeSet<Seq>>,
    fwd: &BTreeMap<Seq, Slot>,
) -> BTreeMap<Seq, Entry> {
    let rank = |e: Entry| match e {
        Entry::Chain(s) => (0, fwd[&s].start, s),
        Entry::Finish => (1, 0, 0),
    };
    let mut out: BTreeMap<Seq, Entry> = BTreeMap::new();
    for &x in order.iter().rev() {
        if chain.contains(&x) {
            continue;
        }
        let best = succ[&x]
            .iter()
            .map(|y| {
                if chain.contains(y) {
                    Entry::Chain(*y)
                } else {
                    out[y]
                }
            })
            .min_by_key(|&e| rank(e))
            .unwrap_or(Entry::Finish);
        out.insert(x, best);
    }
    out
}

/// Longest path of a subnet by the sum of `a` (edges only inside the subnet); ties go to the smaller seq.
pub fn longest_path(
    group: &[Seq],
    pred: &BTreeMap<Seq, BTreeSet<Seq>>,
    a: &BTreeMap<Seq, i64>,
) -> Vec<Seq> {
    let set: BTreeSet<Seq> = group.iter().copied().collect();
    let mut len: BTreeMap<Seq, (i64, Option<Seq>)> = BTreeMap::new();
    // group goes in topological order (by forward start).
    for &x in group {
        let best = pred[&x]
            .iter()
            .filter(|p| set.contains(p))
            .map(|&p| (len[&p].0, p))
            .max_by(|l, r| l.0.cmp(&r.0).then(r.1.cmp(&l.1)));
        len.insert(x, (a[&x] + best.map_or(0, |b| b.0), best.map(|b| b.1)));
    }
    let Some(mut cur) = group
        .iter()
        .copied()
        .max_by(|l, r| len[l].0.cmp(&len[r].0).then(r.cmp(l)))
    else {
        return Vec::new();
    };
    let mut path = vec![cur];
    while let Some(p) = len[&cur].1 {
        path.push(p);
        cur = p;
    }
    path.reverse();
    path
}

/// Buffer size from the path margins `d_i = s_i - a_i`: C&P `⌈Σd/2⌉`, SSQ `⌈sqrt(Σd²)⌉`, at least 1.
pub fn size(method: BufferMethod, d: &[i64]) -> i64 {
    let v = match method {
        BufferMethod::Cnp => (d.iter().sum::<i64>() + 1).div_euclid(2),
        BufferMethod::Ssq => {
            let sq: i64 = d.iter().map(|x| x * x).sum();
            let r = sq.isqrt();
            if r * r < sq { r + 1 } else { r }
        }
    };
    v.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_round_up_with_minimum() {
        assert_eq!(size(BufferMethod::Cnp, &[]), 1);
        assert_eq!(size(BufferMethod::Cnp, &[0, 0]), 1);
        assert_eq!(size(BufferMethod::Cnp, &[3]), 2);
        assert_eq!(size(BufferMethod::Cnp, &[3, 5]), 4);
        assert_eq!(size(BufferMethod::Ssq, &[3, 4]), 5);
        assert_eq!(size(BufferMethod::Ssq, &[3, 5]), 6);
        assert_eq!(size(BufferMethod::Ssq, &[1]), 1);
    }
}
