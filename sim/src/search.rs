//! Every power cut: at each erase or program of a device's life, with each
//! way that operation can be left torn, then the life carried on to the end.
//! At depth 2, a second cut at every operation after the device comes back.

use crate::flash::{Cut, Op, SimFlash, Tear};
use crate::life::{live, provisioned, Outcome, Workload};
use crate::scheme::{Images, Scheme};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// The result of one set of cuts.
#[derive(Clone, Debug)]
pub struct Trial {
    pub cuts: Vec<Cut>,
    pub outcome: Outcome,
}

pub struct Report {
    pub scheme: String,
    pub workload: Workload,
    pub depth: usize,
    /// The life with no power cut.
    pub uncut: Outcome,
    /// The operations of the uncut life.
    pub ops: Vec<Op>,
    /// Every cut state tried, in order.
    pub trials: Vec<Trial>,
}

impl Report {
    pub fn count(&self, label: &str) -> usize {
        self.trials
            .iter()
            .filter(|t| t.outcome.label() == label)
            .count()
    }
    pub fn failures(&self) -> usize {
        self.trials
            .iter()
            .filter(|t| t.outcome != Outcome::Ok)
            .count()
    }
    /// The first failing state, in cut order.
    pub fn first_failure(&self) -> Option<&Trial> {
        self.trials.iter().find(|t| t.outcome != Outcome::Ok)
    }
    /// The worst outcome at each operation of the uncut life, over every
    /// tear (and, at depth 2, every second cut).
    pub fn worst_by_op(&self) -> Vec<Outcome> {
        let mut w = vec![Outcome::Ok; self.ops.len()];
        for t in &self.trials {
            let i = t.cuts[0].at;
            if t.outcome.rank() > w[i].rank() {
                w[i] = t.outcome.clone();
            }
        }
        w
    }
}

/// The first cuts: every operation of the uncut life, every tear.
fn first_cuts(ops: &[Op]) -> Vec<Cut> {
    ops.iter()
        .enumerate()
        .flat_map(|(at, op)| {
            Tear::for_kind(op.kind)
                .iter()
                .map(move |&tear| Cut { at, tear })
        })
        .collect()
}

pub fn search(
    scheme: &dyn Scheme,
    images: &Images,
    workload: Workload,
    depth: usize,
    threads: usize,
) -> Report {
    let fresh = provisioned(scheme, images);
    let uncut = live(scheme, images, &fresh, workload, &[]);
    let ops: Vec<Op> = uncut.periods.concat();
    let firsts = first_cuts(&ops);

    let jobs: Vec<Vec<Cut>> = if depth >= 2 {
        // The second cut goes at every operation of what happens after the
        // first one, which depends on where the first one was.
        let after: Vec<Vec<Op>> = par_map(&firsts, threads, |c| {
            let l = live(scheme, images, &fresh, workload, &[*c]);
            l.periods.get(1).cloned().unwrap_or_default()
        });
        firsts
            .iter()
            .zip(after)
            .flat_map(|(c1, rest)| {
                let mut v = vec![vec![*c1]];
                v.extend(first_cuts(&rest).into_iter().map(|c2| vec![*c1, c2]));
                v
            })
            .collect()
    } else {
        firsts.iter().map(|c| vec![*c]).collect()
    };

    let outcomes = par_map(&jobs, threads, |cuts| {
        live(scheme, images, &fresh, workload, cuts).outcome
    });
    Report {
        scheme: scheme.name(),
        workload,
        depth,
        uncut: uncut.outcome,
        ops,
        trials: jobs
            .into_iter()
            .zip(outcomes)
            .map(|(cuts, outcome)| Trial { cuts, outcome })
            .collect(),
    }
}

/// Maps `f` over `items` on `threads` threads, keeping the order.
pub fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap())
        .collect()
}

/// A fresh device for `scheme`, for tools that want to replay one life.
pub fn fresh(scheme: &dyn Scheme, images: &Images) -> SimFlash {
    provisioned(scheme, images)
}
