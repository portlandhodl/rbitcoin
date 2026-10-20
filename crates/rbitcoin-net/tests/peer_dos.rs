use rbitcoin_net::peer_dos::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[test]
fn rate_limiter_allows_under_budget() {
    let mut r = PeerRateLimiter::new(10, 1000);
    for _ in 0..10 {
        assert!(r.note(50));
    }
    assert!(!r.note(1), "11th message must trip msg limit");
}

#[test]
fn rate_limiter_bytes_cap() {
    let mut r = PeerRateLimiter::new(1000, 100);
    assert!(r.note(100));
    assert!(!r.note(1));

    let mut blended = PeerRateLimiter::new(10_000, 2_000);
    let t0 = Instant::now();
    assert!(blended.note_at(1_000, t0));
    assert!(blended.note_at(1, t0 + RATE_WINDOW));
    assert!(
        !blended.note_at(1_600, t0 + RATE_WINDOW + Duration::from_millis(500)),
        "500ms later half of the previous 1000 bytes plus this frame exceeds 2000"
    );
    // Half of the previous window is 500 bytes, so 1000 still fits in 2000.
    // Adding the elapsed time instead of subtracting it would not.
    let mut half = PeerRateLimiter::new(10_000, 2_000);
    assert!(half.note_at(1_000, t0));
    assert!(half.note_at(1, t0 + RATE_WINDOW));
    assert!(
        half.note_at(1_000, t0 + RATE_WINDOW + Duration::from_millis(500)),
        "500 of the previous 1000 bytes plus 1000 still fits"
    );
}

#[test]
fn rate_limiter_boundary_does_not_grant_a_second_budget() {
    let mut r = PeerRateLimiter::new(2, 10_000);
    let t0 = Instant::now();
    assert!(r.note_at(1, t0));
    assert!(r.note_at(1, t0));
    assert!(!r.note_at(1, t0));
    let boundary = t0 + RATE_WINDOW;
    assert!(
        !r.note_at(1, boundary),
        "a full previous second must not grant another budget at the boundary"
    );
    let cleared = t0 + RATE_WINDOW + RATE_WINDOW + Duration::from_millis(1);
    assert!(r.note_at(1, cleared));
    assert!(r.note_at(1, cleared));
    assert!(!r.note_at(1, cleared));

    let mut rolled = PeerRateLimiter::new(10_000, 1_000);
    let roll_t0 = Instant::now();
    assert!(rolled.note_at(800, roll_t0));
    assert!(rolled.note_at(100, roll_t0 + RATE_WINDOW));
    assert!(
        !rolled.note_at(510, roll_t0 + RATE_WINDOW + Duration::from_millis(500)),
        "rolled: 100 + half of 800 + 510 exceeds 1000"
    );

    // `new` stamps its own start. Jump far enough that this note sets
    // the window start to `anchor`, then hit that start plus one second.
    let mut edge = PeerRateLimiter::new(10_000, 1_000);
    let anchor = Instant::now() + RATE_WINDOW + RATE_WINDOW + Duration::from_secs(5);
    assert!(edge.note_at(800, anchor));
    let boundary = anchor + RATE_WINDOW;
    assert!(
        edge.note_at(200, boundary),
        "the exact boundary still has the rest of a 1000-byte budget"
    );
    assert!(
        !edge.note_at(401, boundary + Duration::from_millis(500)),
        "half a second later the previous 800 bytes still count"
    );
}

#[test]
fn rate_limiter_window_resets() {
    let mut r = PeerRateLimiter::new(2, 10_000);
    let t0 = Instant::now();
    assert!(r.note_at(1, t0));
    assert!(r.note_at(1, t0));
    assert!(!r.note_at(1, t0));
    let cleared = t0 + RATE_WINDOW + RATE_WINDOW + Duration::from_millis(1);
    assert!(r.note_at(1, cleared));
}

#[test]
fn max_inbound_env_default() {
    // Do not mutate env in parallel tests; just check parse of default path.
    const {
        assert!(DEFAULT_MAX_INBOUND >= 1);
    }
    assert_eq!(RATE_LIMIT_BAN_SCORE, 50);
    assert_eq!(OVERSIZE_BAN_SCORE, 100);
}

#[test]
fn one_overflow_scores_and_the_second_disconnects() {
    let mut rate = PeerRateLimiter::new(1, 10_000);
    let mut score = 0u32;
    let disconnect_at = RATE_LIMIT_BAN_SCORE.saturating_mul(2);
    assert!(decoy_stays(&mut rate, &mut score, 1, disconnect_at));
    assert_eq!(score, 0, "a frame that fits does not score");
    assert!(
        decoy_stays(&mut rate, &mut score, 1, disconnect_at),
        "one frame over the window stays connected"
    );
    assert_eq!(score, RATE_LIMIT_BAN_SCORE);
    assert!(
        !decoy_stays(&mut rate, &mut score, 1, disconnect_at),
        "the next overflow reaches the disconnect threshold"
    );
    assert_eq!(score, disconnect_at);
}
