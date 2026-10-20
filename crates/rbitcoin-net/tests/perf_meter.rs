use rbitcoin_net::perf_meter::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn a_window_sample_leaves_the_running_total() {
    let c = PerfCounter::new();
    c.add(1);
    c.add(2);
    assert_eq!((c.take_window(), c.total()), (3, 3));
    assert_eq!((c.take_window(), c.total()), (0, 3));
    c.add(1);
    assert_eq!((c.take_window(), c.total()), (1, 4));
}

#[test]
fn a_request_window_keeps_the_totals() {
    let m = RequestMeter::new();
    m.note(10);
    m.note(30);
    assert_eq!(m.take_window(), (2, 40, 30));
    m.note(5);
    assert_eq!(m.take_window(), (1, 5, 5));
    assert_eq!(m.totals(), (3, 45));
}

#[test]
fn a_window_max_resets_per_sample() {
    let m = PerfMax::new();
    m.note(5);
    m.note(3);
    assert_eq!(m.take(), 5);
    assert_eq!(m.take(), 0);
}
