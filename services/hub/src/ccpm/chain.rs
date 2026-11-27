//! Critical chain extraction (rule 6).

use std::collections::{BTreeMap, BTreeSet};

use super::model::{ChainLink, Seq};
use super::schedule::Slot;

/// From the task with the largest finish (ties go to the smaller seq) backwards: the predecessor whose finish equals the start,
/// first by dependency (smaller seq), then by resource. Returns the links from the start of the chain.
pub fn extract(
    slots: &BTreeMap<Seq, Slot>,
    preds: &BTreeMap<Seq, BTreeSet<Seq>>,
    res_prev: &BTreeMap<Seq, Seq>,
) -> Vec<ChainLink> {
    let Some(mut cur) = slots
        .iter()
        .max_by(|l, r| l.1.end.cmp(&r.1.end).then(r.0.cmp(l.0)))
        .map(|(s, _)| *s)
    else {
        return Vec::new();
    };
    let mut back: Vec<ChainLink> = Vec::new();
    loop {
        let start = slots[&cur].start;
        let dep = preds[&cur]
            .iter()
            .copied()
            .find(|p| slots.get(p).is_some_and(|s| s.end == start));
        let res = res_prev
            .get(&cur)
            .copied()
            .filter(|p| slots[p].end == start);
        match (dep, res) {
            (Some(p), _) => {
                back.push(ChainLink {
                    seq: cur,
                    resource_link: false,
                });
                cur = p;
            }
            (None, Some(p)) => {
                back.push(ChainLink {
                    seq: cur,
                    resource_link: true,
                });
                cur = p;
            }
            (None, None) => {
                back.push(ChainLink {
                    seq: cur,
                    resource_link: false,
                });
                break;
            }
        }
    }
    // The resource link flag refers to the connection with the previous link: after reversing, shift it.
    back.reverse();
    let mut out: Vec<ChainLink> = back
        .iter()
        .map(|l| ChainLink {
            seq: l.seq,
            resource_link: false,
        })
        .collect();
    for i in 1..back.len() {
        out[i].resource_link = back[i].resource_link;
    }
    out
}

/// Links for a given chain membership (baseline): resource if the neighbours are not linked by a dependency.
pub fn links_for(
    seqs: &[Seq],
    preds: &BTreeMap<Seq, BTreeSet<Seq>>,
    assignee: impl Fn(Seq) -> Option<String>,
) -> Vec<ChainLink> {
    seqs.iter()
        .enumerate()
        .map(|(i, &s)| ChainLink {
            seq: s,
            resource_link: i > 0
                && !preds[&s].contains(&seqs[i - 1])
                && assignee(s).is_some()
                && assignee(s) == assignee(seqs[i - 1]),
        })
        .collect()
}
