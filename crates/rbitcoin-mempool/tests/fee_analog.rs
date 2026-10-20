use rbitcoin_mempool::fee_analog::*;
use std::collections::VecDeque;

/// Calm hurdles near 1000 sat/kvB with a deterministic wobble.
fn calm(i: usize) -> u64 {
    1_000 + (i as u64 * 7_919) % 200
}

fn history(targets: &[u32], hurdles: impl IntoIterator<Item = u64>) -> AnalogHistory {
    let mut h = AnalogHistory::new(targets);
    h.rebuild(hurdles);
    h
}

fn paired_history(now: u64, pairs: impl IntoIterator<Item = (f64, u64)>) -> AnalogHistory {
    let mut h = AnalogHistory::new(&[1]);
    h.hurdles = VecDeque::from([now, now, now]);
    h.depths[0].pairs = pairs.into_iter().collect();
    h
}

#[test]
fn far_quantile_separates_a_short_window_from_a_long_one() {
    let n = 2_500;
    // Pairs of the higher hurdle, then three of the common one. Short
    // windows are often entirely higher; a 10-block window always sees
    // the common hurdle.
    let hurdles = (0..n).map(|i| if i % 5 < 2 { 1_200 } else { 1_000 });
    let h = history(&[2, 10], hurdles);
    let short = h.rate_sat_kvb(2, 0.95).unwrap();
    let long = h.rate_sat_kvb(10, 0.95).unwrap();
    assert!(short > long, "2-block {short} vs 10-block {long}");

    // About 6% of blocks are cheaper. That is too rare to move the
    // 10-block 95% off the common hurdle.
    let hurdles = (0..n).map(|i| if i % 17 == 0 { 800 } else { 1_000 });
    let h = history(&[10], hurdles);
    assert_eq!(h.rate_sat_kvb(10, 0.95), Some(1_000));

    let flat = history(&[2, 6, 10], std::iter::repeat_n(1_000u64, n));
    let rates = [2u32, 6, 10].map(|d| flat.rate_sat_kvb(d, 0.95).unwrap());
    assert_eq!(rates, [1_000, 1_000, 1_000]);
}

#[test]
fn quantile_interpolates_in_log_rate_between_adjacent_hurdles() {
    let now = 1_000u64;
    let center = (now as f64).ln();
    let n = ANALOG_READY_PAIRS;
    let split = std::iter::repeat_n((center, 1_000u64), 1_900)
        .chain(std::iter::repeat_n((center, 2_000u64), n - 1_900));
    let h = paired_history(now, split);
    let rate = h.rate_sat_kvb(1, 0.95025).unwrap();
    let expect = (1_000f64 * 2_000f64).sqrt().round() as u64;
    assert_eq!(rate, expect, "halfway in log rate");

    let flat = paired_history(now, std::iter::repeat_n((center, 1_000u64), n));
    assert_eq!(flat.rate_sat_kvb(1, 0.95025), Some(1_000));
}

#[test]
fn a_past_spike_does_not_steer_a_calm_market() {
    // 4000 calm blocks with a 400-block spike at 50000 in the middle
    let hurdles = (0..4_000).map(|i| {
        if (1_800..2_200).contains(&i) {
            50_000
        } else {
            calm(i)
        }
    });
    let h = history(&[2, 20], hurdles);
    for n in [2, 20] {
        let rate = h.rate_sat_kvb(n, 0.99).unwrap();
        assert!(rate < 1_200, "N={n}: calm now, got {rate}");
    }
}

#[test]
fn a_spike_in_progress_finds_the_spike_windows() {
    let hurdles = (0..4_000).map(|i| {
        if (1_800..2_200).contains(&i) || i >= 3_900 {
            50_000 + (i as u64 % 7) * 100
        } else {
            calm(i)
        }
    });
    let h = history(&[2], hurdles);
    let rate = h.rate_sat_kvb(2, 0.99).unwrap();
    assert!(rate >= 50_000, "spike now, got {rate}");
}

#[test]
fn a_spike_that_ended_stops_counting_once_the_lookback_is_calm() {
    let hurdles = (0..4_000).map(|i| {
        if (1_800..2_200).contains(&i) || (3_700..3_990).contains(&i) {
            50_000
        } else {
            calm(i)
        }
    });
    let h = history(&[2], hurdles);
    let rate = h.rate_sat_kvb(2, 0.99).unwrap();
    assert!(rate < 1_200, "spike ended 10 blocks ago, got {rate}");
}

#[test]
fn a_target_answers_only_once_it_holds_enough_pairs() {
    let lookback = analog_lookback(2);
    let needed = ANALOG_READY_PAIRS + lookback + 2 - 1;
    let mut h = history(&[2], (0..needed - 1).map(calm));
    assert_eq!(h.pairs(2), ANALOG_READY_PAIRS - 1);
    assert_eq!(h.rate_sat_kvb(2, 0.99), None);
    h.push_back(calm(needed));
    assert_eq!(h.pairs(2), ANALOG_READY_PAIRS);
    assert!(h.rate_sat_kvb(2, 0.99).is_some());
    assert_eq!(h.rate_sat_kvb(3, 0.99), None, "untracked target");
    assert_eq!(h.rate_sat_kvb(2, 1.5), None, "confidence out of range");
}

#[test]
fn an_unmatched_level_uses_the_nearest_neighbors() {
    // now sits at 20000, far outside every past lookback band
    let hurdles = (0..3_000).map(|i| if i >= 2_990 { 20_000 } else { calm(i) });
    let h = history(&[2], hurdles);
    let rate = h.rate_sat_kvb(2, 0.5).unwrap();
    assert!(rate >= 1_000, "{rate}");
}

#[test]
fn analog_selector_keeps_each_boundary_rate() {
    let now = 10_000u64;
    let center = (now as f64).ln();
    let band = std::iter::repeat_n((center, 1_000), ANALOG_MIN_NEIGHBORS + 50)
        .chain(std::iter::repeat_n((center + 0.1, 50_000), 50))
        .chain(std::iter::repeat_n(
            (center + 1.0, 90_000),
            ANALOG_READY_PAIRS - ANALOG_MIN_NEIGHBORS - 100,
        ));
    let quantile = std::iter::repeat_n((center + 0.3, 50_000), ANALOG_MIN_NEIGHBORS - 1).chain(
        std::iter::repeat_n(
            (center - 0.4, 1_000),
            ANALOG_READY_PAIRS - ANALOG_MIN_NEIGHBORS + 1,
        ),
    );
    let mut nearest = std::iter::repeat_n((center + 0.3, 1_000), ANALOG_MIN_NEIGHBORS - 1)
        .chain([(center + 0.31, 50_000), (center + 0.32, 1_000)])
        .chain(std::iter::repeat_n(
            (center + 0.4, 1_000),
            ANALOG_READY_PAIRS - ANALOG_MIN_NEIGHBORS - 1,
        ))
        .collect::<Vec<_>>();
    let mut seed = 341usize;
    for i in (1..nearest.len()).rev() {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        nearest.swap(i, seed % (i + 1));
    }
    let upper = std::iter::repeat_n((0.0, 1_000), ANALOG_READY_PAIRS - 1)
        .chain([(ANALOG_BAND.ln(), 50_000)]);

    let cases = [
        (
            "exact upper boundary",
            paired_history(1, upper),
            1.0,
            50_000,
        ),
        (
            "absolute log distance",
            paired_history(now, band),
            0.9,
            50_000,
        ),
        (
            "absolute log distance quantile",
            paired_history(now, quantile),
            0.99,
            50_000,
        ),
        (
            "two hundredth window",
            paired_history(now, nearest),
            1.0,
            50_000,
        ),
    ];
    for (name, history, confidence, want) in cases {
        assert_eq!(history.rate_sat_kvb(1, confidence), Some(want), "{name}");
    }
}

#[test]
fn end_updates_match_a_rebuild() {
    // deterministic LCG drives pushes and pops at both ends
    let mut seed = 42u64;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        seed >> 33
    };
    let targets = [1, 2, 6, 20];
    let mut h = AnalogHistory::new(&targets);
    let mut seq: VecDeque<u64> = VecDeque::new();
    for _ in 0..3_000 {
        let r = next();
        let hurdle = 100 + next() % 5_000;
        match r % 10 {
            0..=4 => {
                h.push_back(hurdle);
                seq.push_back(hurdle);
            }
            5..=6 => {
                h.push_front(hurdle);
                seq.push_front(hurdle);
            }
            7 => {
                h.pop_back();
                seq.pop_back();
            }
            _ => {
                h.pop_front();
                seq.pop_front();
            }
        }
    }
    let rebuilt = history(&targets, seq.iter().copied());
    assert_eq!(h.hurdles, rebuilt.hurdles);
    for (a, b) in h.depths.iter().zip(&rebuilt.depths) {
        assert_eq!(a.pairs, b.pairs, "N={}", a.n);
    }
}
