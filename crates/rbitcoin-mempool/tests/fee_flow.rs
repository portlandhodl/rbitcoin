use rbitcoin_mempool::fee_est::{bucket_count, bucket_index, BLOCK_WEIGHT_WU, SECONDS_PER_BLOCK};
use rbitcoin_mempool::fee_flow::*;
use std::time::Instant;

use std::time::Duration;

/// Two admits, two seconds apart, at 10_000 WU. The rounded bucket rate is
/// the EMA a warm `estimatesmartfee` reads. An empty pool answers
/// "Insufficient data" before this blend runs, and one RPC admit does not
/// move a half-life EMA to this exact integer.
#[test]
fn two_admits_pin_the_bucket_ema() {
    let t0 = Instant::now();
    let mut m = FeeFlowMeter::new(t0);
    m.note_admit(10_000, 1_000, t0 + Duration::from_secs(2));
    m.note_admit(10_000, 1_000, t0 + Duration::from_secs(4));
    let rates = m.admit_rates_wu_s(t0 + Duration::from_secs(4));
    let bi = bucket_index(1_000);
    assert_eq!(rates[bi], 91, "{rates:?}");
}

#[test]
fn admit_stream_raises_ema() {
    let t0 = Instant::now();
    let mut m = FeeFlowMeter::new(t0);
    for i in 0..40 {
        let t = t0 + Duration::from_secs(i + 1);
        m.note_admit(10_000, 1_000, t);
    }
    let rates = m.admit_rates_wu_s(t0 + Duration::from_secs(41));
    let bi = bucket_index(1_000);
    assert!(rates[bi] > 0, "ema should be warm, got {rates:?}");
}

#[test]
fn fullness_tracks_decayed_weight_not_the_clock() {
    let t0 = Instant::now();
    let mut idle = FeeFlowMeter::new(t0);
    assert_eq!(idle.fullness(t0 + Duration::from_secs(10_000)), 0.0);

    let mut one = FeeFlowMeter::new(t0);
    one.note_admit(1_000, 1_000, t0 + Duration::from_secs(1_000));
    let after_gap = one.fullness(t0 + Duration::from_secs(1_000));
    assert!(
        after_gap < 0.01,
        "one tx after a long pause must not fill the meter, got {after_gap}"
    );

    let mut small = FeeFlowMeter::new(t0);
    for i in 0..32 {
        small.note_admit(1_000, 1_000, t0 + Duration::from_secs(i + 1));
    }
    let young = small.fullness(t0 + Duration::from_secs(33));
    assert!(
        young < 0.05,
        "32 ordinary admits stay near zero, got {young}"
    );

    let full = flow_full_wu().round() as u64;
    let mut lump = FeeFlowMeter::new(t0);
    lump.note_admit(full, 1_000, t0 + Duration::from_secs(1));
    let hot = lump.fullness(t0 + Duration::from_secs(1));
    assert!(hot > 0.99, "a full block of weight fills α, got {hot}");
    let cooled = lump.fullness(t0 + Duration::from_secs(1 + 150));
    assert!(
        (cooled - 0.5).abs() < 0.02,
        "half-life decay of the scalar, got {cooled}"
    );
    let expect = (BLOCK_WEIGHT_WU as f64 / SECONDS_PER_BLOCK as f64)
        * (ADMIT_HALF_LIFE_SECS / std::f64::consts::LN_2);
    assert!((flow_full_wu() - expect).abs() < 1.0, "{}", flow_full_wu());
}

#[test]
fn idle_decays() {
    let t0 = Instant::now();
    let mut m = FeeFlowMeter::new(t0);
    m.note_admit(100_000, 500, t0 + Duration::from_secs(1));
    let hot = m.admit_rates_wu_s(t0 + Duration::from_secs(2));
    let cold = m.admit_rates_wu_s(t0 + Duration::from_secs(600));
    let bi = bucket_index(500);
    assert!(cold[bi] < hot[bi], "hot={} cold={}", hot[bi], cold[bi]);
}
