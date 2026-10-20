use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use rbitcoin_log::progress::*;

fn running(stage: &'static str) -> Option<Snapshot> {
    snapshots().into_iter().find(|s| s.stage == stage)
}

#[test]
fn stage_reports_and_unregisters_on_drop() {
    capture_finished(true);
    {
        let s = begin("progress unit a", 200);
        s.set_done(50);
        s.set_done(20); // out of order: never moves back
        let snap = running("progress unit a").expect("registered");
        assert_eq!((snap.done, snap.total), (50, 200));
        assert_eq!(snap.percent(), Some(25.0));
        s.add_done(50);
        assert_eq!(running("progress unit a").unwrap().done, 100);
    }
    assert!(running("progress unit a").is_none(), "drop unregisters");
    let fin: Vec<_> = take_finished()
        .into_iter()
        .map(|s| (s.stage, s.done, s.total))
        .collect();
    capture_finished(false);
    assert_eq!(fin, [("progress unit a", 100, 200)]);
}

#[test]
fn total_follows_a_moving_target() {
    capture_finished(true);
    {
        let s = begin_at("progress unit moving", 100, 40);
        s.set_done(90);
        s.set_total(120);
        let snap = running("progress unit moving").unwrap();
        assert_eq!((snap.done, snap.total, snap.base), (90, 120, 40));
        assert_eq!(snap.percent(), Some(75.0));
        s.set_done(130);
        s.set_total(130);
    }
    let fin: Vec<_> = take_finished()
        .into_iter()
        .map(|s| (s.stage, s.done, s.total))
        .collect();
    capture_finished(false);
    assert_eq!(fin, [("progress unit moving", 130, 130)]);
}

#[test]
fn overlapping_stages_keep_their_own_counts() {
    let outer = begin("progress unit outer", 10);
    let inner = begin("progress unit inner", 20);
    outer.add_done(3);
    inner.add_done(7);
    assert_eq!(running("progress unit outer").unwrap().done, 3);
    assert_eq!(running("progress unit inner").unwrap().done, 7);
    let all = snapshots();
    let pos = |n| all.iter().position(|s| s.stage == n).unwrap();
    assert!(pos("progress unit outer") < pos("progress unit inner"));
    drop(outer);
    assert!(running("progress unit outer").is_none());
    assert_eq!(running("progress unit inner").unwrap().done, 7);
    drop(inner);
    assert!(running("progress unit inner").is_none());
}

#[test]
fn eta_only_between_zero_and_full() {
    let s = Snapshot {
        stage: "x",
        done: 5,
        total: 0,
        base: 0,
        elapsed: Duration::from_secs(10),
        started_at: SystemTime::UNIX_EPOCH,
    };
    assert_eq!(s.percent(), None);
    assert_eq!(s.eta(), None);
    let half = Snapshot { total: 10, ..s };
    assert_eq!(half.eta(), Some(Duration::from_secs(10)));
    let none_yet = Snapshot {
        done: 0,
        ..half.clone()
    };
    assert_eq!(none_yet.percent(), Some(0.0));
    assert_eq!(none_yet.eta(), None);
    let full = Snapshot { done: 10, ..half };
    assert_eq!(full.eta(), None);
}

#[test]
fn percent_rounds_down_exactly() {
    let at = |done, total| Snapshot {
        stage: "x",
        done,
        total,
        base: 0,
        elapsed: Duration::ZERO,
        started_at: SystemTime::UNIX_EPOCH,
    };
    // Each of these is a hundredth low under a float floor.
    assert_eq!(at(57, 100).percent(), Some(57.0));
    assert_eq!(at(69, 100).percent(), Some(69.0));
    assert_eq!(at(43, 1000).percent(), Some(4.3));
    assert_eq!(at(1, 3).percent(), Some(33.33));
    assert_eq!(at(99_999, 100_000).percent(), Some(99.99));
    assert_eq!(at(100, 100).percent(), Some(100.0));
    assert_eq!(at(5, 0).percent(), None);
    // Exhaustive for small totals: never above the true value, never a
    // whole hundredth below it.
    for total in 1..=300u64 {
        for done in 0..=total {
            let p = at(done, total).percent().unwrap();
            let exact = done as f64 * 100.0 / total as f64;
            assert!(p <= exact + 1e-9 && exact - p < 0.01, "{done}/{total}: {p}");
        }
    }
}

#[test]
fn resumed_stage_eta_counts_only_this_run() {
    let s = begin_at("progress unit resumed", 100, 50);
    let snap = running("progress unit resumed").unwrap();
    assert_eq!((snap.done, snap.base), (50, 50));
    assert_eq!(snap.percent(), Some(50.0));
    assert_eq!(snap.eta(), None, "nothing done this run yet");
    drop(s);
    let after = Snapshot {
        stage: "x",
        done: 60,
        total: 100,
        base: 50,
        elapsed: Duration::from_secs(10),
        started_at: SystemTime::UNIX_EPOCH,
    };
    // 10 units in 10s this run, 40 left.
    assert_eq!(after.eta(), Some(Duration::from_secs(40)));
}
