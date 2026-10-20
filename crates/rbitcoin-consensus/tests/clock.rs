use rbitcoin_consensus::clock::*;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn mock_then_wall() {
    let c = NodeClock::new();
    c.set_mock(1_700_000_000);
    assert_eq!(c.now_secs(), 1_700_000_000);
    c.set_mock(0);
    assert!(c.now_secs() >= 1_700_000_000 || c.now_secs() > 1_600_000_000);
}

#[test]
fn with_now_scopes() {
    assert!(current_now() > 0);
    with_now(42, || assert_eq!(current_now(), 42));
    assert_ne!(current_now(), 42);
}

#[test]
fn with_frozen_pins_clock_sample_then_restores_prior() {
    let c = NodeClock::new();
    c.set_mock(2_000_000_000);
    with_now(42, || {
        c.with_frozen(|| assert_eq!(current_now(), 2_000_000_000));
        assert_eq!(current_now(), 42);
    });
}

#[test]
fn connection_attempt_stays_put_when_mock_time_jumps() {
    let c = NodeClock::new();
    assert_eq!(c.connection_attempt_at(), 0);
    c.note_connection_attempt();
    let started = c.connection_attempt_at();
    assert!(started > 0);
    c.set_mock((started + 11) as i64);
    assert_eq!(c.connection_attempt_at(), started);
    assert!(c.now_secs() >= started + 11);
    c.note_connection_attempt();
    assert_eq!(c.connection_attempt_at(), started);
}
