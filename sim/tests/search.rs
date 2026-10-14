//! The power-cut search, end to end: ota-safe survives every cut, the naive
//! schemes fail where the README says, and every planted bug is caught.

use ota_safe::Mutant;
use ota_sim::flash::{Cut, Tear};
use ota_sim::life::{live, live_keeping_cut, provisioned, Outcome, Workload};
use ota_sim::scheme::{Images, NaiveAB, NaiveInPlace, OtaSafe, Scheme, MUTANTS};
use ota_sim::search::search;

const BOTH: [Workload; 2] = [Workload::Good, Workload::BadUpdate];

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

#[test]
fn ota_safe_settles_correctly_after_every_single_cut() {
    let images = Images::standard();
    for w in BOTH {
        let r = search(&OtaSafe::new(), &images, w, 1, threads());
        assert_eq!(r.uncut, Outcome::Ok, "{w:?}");
        let expected: usize = r.ops.iter().map(|op| Tear::for_kind(op.kind).len()).sum();
        assert_eq!(r.trials.len(), expected, "every operation, every tear");
        assert_eq!(r.failures(), 0, "{w:?}: {:?}", r.first_failure());
    }
}

/// `make deep`: a second cut at every operation after the device comes back.
#[test]
#[ignore = "about a minute in release mode; run with make deep"]
fn ota_safe_settles_correctly_after_every_pair_of_cuts() {
    let images = Images::standard();
    for w in BOTH {
        let r = search(&OtaSafe::new(), &images, w, 2, threads());
        assert!(r.trials.len() > 150_000);
        assert_eq!(r.failures(), 0, "{w:?}: {:?}", r.first_failure());
    }
}

#[test]
fn the_naive_schemes_fail_where_the_readme_says() {
    let images = Images::standard();
    let r = search(&NaiveInPlace, &images, Workload::Good, 1, threads());
    assert_eq!((r.count("BRICKED"), r.trials.len()), (437, 443));
    let r = search(&NaiveAB, &images, Workload::Good, 1, threads());
    assert_eq!((r.count("BRICKED"), r.trials.len()), (14, 459));
    // Neither can get off a bad update.
    for s in [&NaiveInPlace as &dyn Scheme, &NaiveAB] {
        let f = provisioned(s, &images);
        let l = live(s, &images, &f, Workload::BadUpdate, &[]);
        assert_eq!(l.outcome.label(), "STUCK", "{}", s.name());
    }
}

#[test]
fn naive_ab_bricks_when_power_goes_between_erasing_and_writing_the_flag() {
    let images = Images::standard();
    let f = provisioned(&NaiveAB, &images);
    let uncut = live(&NaiveAB, &images, &f, Workload::Good, &[]);
    let ops = uncut.periods.concat();
    let flag = ops.iter().position(|op| op.addr == 0).unwrap();
    let l = live(
        &NaiveAB,
        &images,
        &f,
        Workload::Good,
        &[Cut {
            at: flag + 1,
            tear: Tear::Before,
        }],
    );
    assert_eq!(l.outcome.label(), "BRICKED");
}

#[test]
fn every_planted_bug_is_caught() {
    let images = Images::standard();
    for m in MUTANTS {
        let s = OtaSafe::with_mutant(m);
        let failures: usize = BOTH
            .iter()
            .map(|&w| {
                let r = search(&s, &images, w, 1, threads());
                r.failures() + usize::from(r.uncut != Outcome::Ok)
            })
            .sum();
        assert!(failures > 0, "{} went uncaught", s.name());
    }
}

#[test]
fn three_of_the_bugs_only_show_under_a_power_cut() {
    let images = Images::standard();
    for m in [
        Mutant::SingleMetaCopy,
        Mutant::NoCrc,
        Mutant::TrialBeforeImage,
    ] {
        let s = OtaSafe::with_mutant(m);
        let f = provisioned(&s, &images);
        for w in BOTH {
            assert_eq!(
                live(&s, &images, &f, w, &[]).outcome,
                Outcome::Ok,
                "{} {w:?}",
                s.name()
            );
        }
    }
}

#[test]
fn a_cut_replays_exactly() {
    let images = Images::standard();
    let s = OtaSafe::new();
    let f = provisioned(&s, &images);
    for cut in ["44:erase-0", "61:scatter", "99:prefix-2"] {
        let cut: Cut = cut.parse().unwrap();
        let a = live_keeping_cut(&s, &images, &f, Workload::Good, &[cut]);
        let b = live_keeping_cut(&s, &images, &f, Workload::Good, &[cut]);
        assert_eq!(a.at_first_cut.unwrap().mem, b.at_first_cut.unwrap().mem);
        assert_eq!(a.events.len(), b.events.len());
        assert_eq!(a.outcome, b.outcome);
    }
}

#[test]
fn uncut_lives_end_where_expected() {
    let images = Images::standard();
    let s = OtaSafe::new();
    let f = provisioned(&s, &images);
    let good = live(&s, &images, &f, Workload::Good, &[]);
    assert!(good.events.last().unwrap().what.contains("v3 confirmed"));
    let bad = live(&s, &images, &f, Workload::BadUpdate, &[]);
    let crashes = bad
        .events
        .iter()
        .filter(|e| e.what.contains("crashes"))
        .count();
    assert_eq!(crashes, 3, "three trial boots, then rollback");
    assert!(bad
        .events
        .last()
        .unwrap()
        .what
        .contains("refused (rolled back before)"));
}
