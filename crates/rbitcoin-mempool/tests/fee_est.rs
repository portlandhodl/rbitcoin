use rbitcoin_mempool::fee_est::*;

#[test]
fn capacity_is_n_times_block() {
    assert_eq!(capacity_wu(1), BLOCK_WEIGHT_WU);
    assert_eq!(capacity_wu(3), 3 * BLOCK_WEIGHT_WU);
    assert_eq!(capacity_wu(0), BLOCK_WEIGHT_WU); // clamp
    assert_eq!(horizon_secs(2), 1200);
}

#[test]
fn zero_inflow_equals_frontier_depth() {
    // stock_above: 2e6 WU above any rate < 1000, 0 above ≥1000
    let stock = |r: u64| if r < 1000 { 2_000_000 } else { 0 };
    let inflow = vec![0u64; bucket_count()];
    let rates = default_candidate_rates();
    let r = min_rate_for_capacity(stock, &inflow, 1, &rates).expect("fits");
    // Need stock_above(R) ≤ 0.95 * 4e6 → 2e6 always ok; minimum R on ladder that
    // still has load ≤ cap. All rates with stock 2e6 fit; min is lowest edge.
    assert_eq!(r, rates[0]);
    // Deeper N still fits at lowest.
    let r2 = min_rate_for_capacity(stock, &inflow, 5, &rates).unwrap();
    assert!(r2 <= r);
}

#[test]
fn high_bucket_inflow_raises_rate() {
    let stock = |_r: u64| 0u64;
    let mut inflow = vec![0u64; bucket_count()];
    // Massive inflow in high-rate buckets (index for 50k+).
    let hi = bucket_index(50_000);
    let floor = FEE_BUCKET_EDGES_SAT_PER_KVB[hi];
    inflow[hi] = 50_000; // WU/s → over 600s = 30e6 WU >> 4e6
    let rates = default_candidate_rates();
    let cold = min_rate_for_capacity(stock, &vec![0u64; bucket_count()], 1, &rates).unwrap();
    let hot = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
    assert!(hot >= cold, "hot={hot} cold={cold}");
    assert!(
        hot >= floor,
        "should clear high-inflow competitors, got {hot} floor {floor}"
    );
}

#[test]
fn monotone_in_target_blocks() {
    let stock = |r: u64| if r < 5_000 { 3_500_000 } else { 0 };
    let mut inflow = vec![0u64; bucket_count()];
    inflow[bucket_index(1_000)] = 1_000; // modest
    let rates = default_candidate_rates();
    let r1 = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
    let r3 = min_rate_for_capacity(stock, &inflow, 3, &rates).unwrap();
    let r10 = min_rate_for_capacity(stock, &inflow, 10, &rates).unwrap();
    assert!(r3 <= r1, "r3={r3} r1={r1}");
    assert!(r10 <= r3, "r10={r10} r3={r3}");
}

#[test]
fn bucket_index_edges() {
    let edges = &FEE_BUCKET_EDGES_SAT_PER_KVB;
    assert_eq!(bucket_index(edges[0] - 1), 0);
    assert_eq!(bucket_index(edges[0]), 0);
    assert_eq!(bucket_index(edges[1] - 1), 0);
    assert_eq!(bucket_index(edges[1]), 1);
    assert_eq!(bucket_index(edges[edges.len() - 1]), edges.len() - 1);
    assert_eq!(bucket_index(edges[edges.len() - 1] + 1), edges.len());
}

#[test]
fn confidence_schedule_and_fill() {
    assert!((inclusion_confidence(1) - 0.999).abs() < 1e-12);
    assert!((inclusion_confidence(2) - 0.99).abs() < 1e-12);
    assert!((inclusion_confidence(1008) - 0.99).abs() < 1e-12);
    assert!((fill_frac(0.999) - 0.80).abs() < 1e-12);
    assert!((fill_frac(0.99) - 0.95).abs() < 1e-12);
    assert!((lambda_mult(0.999) - 2.0).abs() < 1e-12);
    assert!((lambda_mult(0.99) - 1.0).abs() < 1e-12);
    assert_eq!(effective_capacity_wu(1), 3_200_000);
    assert_eq!(
        effective_capacity_wu(2),
        (2.0_f64 * 4_000_000.0 * 0.95).round() as u64
    );
}

#[test]
fn near_confidence_raises_rate_vs_far_fill() {
    let stock = |r: u64| if r < 5_000 { 3_500_000 } else { 0 };
    let inflow = vec![0u64; bucket_count()];
    let rates = default_candidate_rates();
    let r1 = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
    let r6 = min_rate_for_capacity(stock, &inflow, 6, &rates).unwrap();
    assert!(r1 >= 5_000, "N=1 80% fill cannot take 3.5e6 WU, got {r1}");
    assert_eq!(r6, rates[0], "N=6 95% of 24e6 WU fits the cheap stock");
    assert!(r1 > r6);
}

#[test]
fn near_lambda_stress_raises_rate() {
    let stock = |_r: u64| 0u64;
    let mut inflow = vec![0u64; bucket_count()];
    inflow[bucket_index(1_000)] = 4_000;
    let rates = default_candidate_rates();
    let r1 = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
    let r6 = min_rate_for_capacity(stock, &inflow, 6, &rates).unwrap();
    assert!(r1 >= r6, "2× λ at N=1 must not undercut N=6, {r1} vs {r6}");
}

#[test]
fn individual_txstat_p10_is_vsize_weighted_and_relay_filtered() {
    let rows = [(1_000, 400), (5_000, 400), (9_000, 400), (1, 400)];
    assert_eq!(block_individual_p10_sat_kvb(&rows, 100), Some(10_000));
    assert_eq!(block_individual_p10_sat_kvb(&[(1, 400)], 100), None);
    assert_eq!(block_individual_p10_sat_kvb(&[], 100), None);

    assert_eq!(
        block_individual_p10_sat_kvb(&[(1, 5), (3, 10)], 100),
        Some(500),
        "a 5-WU transaction occupies two vbytes after ceiling"
    );
    assert_eq!(
        block_individual_p10_sat_kvb(&[(1, 5), (18, 72)], 100),
        Some(500),
        "two vbytes out of twenty place the first transaction exactly at p10"
    );
}

#[test]
fn blend_is_flow_at_one_and_hist_at_far() {
    assert_eq!(blend_sat_kvb(Some(5_000), Some(2_000), 1), Some(5_000));
    let r144 = blend_sat_kvb(Some(5_000), Some(2_000), 144).unwrap();
    assert!((r144 as i64 - 2_000).abs() < 5, "{r144}");
    let r2 = blend_sat_kvb(Some(5_000), Some(2_000), 2).unwrap();
    assert!(r2 > 4_000 && r2 < 5_000, "w(2) is mostly flow: {r2}");
    assert_eq!(blend_sat_kvb(Some(9_000), None, 6), Some(9_000));
    assert_eq!(blend_sat_kvb(None, Some(3_000), 6), Some(3_000));
    assert_eq!(blend_sat_kvb(None, None, 6), None);
}

#[test]
fn warmup_blend_is_history_at_zero_and_flow_at_full() {
    assert_eq!(warmup_blend(0.0, Some(4_000), Some(1_000)), Some(4_000));
    assert_eq!(warmup_blend(1.0, Some(4_000), Some(1_000)), Some(1_000));
    assert_eq!(warmup_blend(0.5, Some(1_000), Some(3_000)), Some(2_000));
    assert_eq!(warmup_blend(0.0, Some(4_000), None), Some(4_000));
    assert_eq!(warmup_blend(0.5, Some(4_000), None), Some(4_000));
    assert_eq!(warmup_blend(1.0, Some(4_000), None), None);
    assert_eq!(warmup_blend(0.0, None, Some(8_000)), None);
    assert_eq!(warmup_blend(0.25, None, Some(8_000)), None);
    assert_eq!(warmup_blend(1.0, None, Some(8_000)), Some(8_000));
    assert_eq!(warmup_blend(0.0, None, None), None);
}

#[test]
fn published_rate_is_cold_at_zero_and_warm_at_full() {
    // Frontier raises history. The confirm floor stays out at α = 0.
    assert_eq!(
        published_depth_rate(1, 0.0, Some(500), Some(2_000), Some(1_000), Some(9_000)),
        Some(2_000)
    );
    // α = 1 is the warm flow quote, raised by the confirm floor.
    assert_eq!(
        published_depth_rate(1, 1.0, Some(500), Some(2_000), Some(1_000), Some(9_000)),
        Some(9_000)
    );
    assert_eq!(
        published_depth_rate(1, 1.0, Some(500), Some(2_000), Some(1_000), None),
        Some(500)
    );
    // A missing cold quote does not invent the floor, or a thin flow rate.
    assert_eq!(
        published_depth_rate(1, 0.0, None, None, None, Some(9_000)),
        None
    );
    assert_eq!(
        published_depth_rate(1, 0.25, Some(500), None, None, None),
        None
    );
    assert_eq!(
        published_depth_rate(1, 1.0, Some(500), None, None, None),
        Some(500)
    );
    assert_eq!(
        published_depth_rate(1, 1.0, None, None, None, Some(9_000)),
        Some(9_000)
    );
    // N=2 ignores the confirm floor. α = 0 is history; α = 1 is the blend.
    assert_eq!(
        published_depth_rate(2, 0.0, Some(1_000), Some(1_000), Some(5_000), Some(9_000)),
        Some(5_000)
    );
    let warm =
        published_depth_rate(2, 1.0, Some(1_000), Some(1_000), Some(5_000), Some(9_000)).unwrap();
    assert!(warm > 1_000 && warm < 3_000, "{warm}");
    assert_eq!(
        published_depth_rate(1, 0.5, Some(1_000), None, Some(3_000), None),
        Some(2_000)
    );
}

#[test]
fn warm_flow_drives_near_targets_without_a_history_floor() {
    // 1 block: flow alone, even far under history
    assert_eq!(
        depth_rate_sat_kvb(1, true, Some(1_000), None, Some(9_000)),
        Some(1_000)
    );
    assert_eq!(
        depth_rate_sat_kvb(1, true, None, None, Some(9_000)),
        Some(9_000)
    );
    // 2 blocks: a blend, pulled mostly toward flow
    let r2 = depth_rate_sat_kvb(2, true, Some(1_000), Some(1_000), Some(9_000)).unwrap();
    assert!(r2 > 1_000 && r2 < 3_000, "{r2}");
    let r2 = depth_rate_sat_kvb(2, true, Some(9_000), Some(9_000), Some(1_000)).unwrap();
    assert!(
        r2 > 7_000 && r2 < 9_000,
        "history does not floor flow either: {r2}"
    );
}

#[test]
fn cold_flow_serves_history_that_the_pool_can_only_raise() {
    assert_eq!(
        depth_rate_sat_kvb(2, false, Some(100), Some(100), Some(4_000)),
        Some(4_000)
    );
    assert_eq!(
        depth_rate_sat_kvb(1, false, Some(100), None, Some(4_000)),
        Some(4_000)
    );
    assert_eq!(
        depth_rate_sat_kvb(2, false, None, Some(6_000), Some(4_000)),
        Some(6_000)
    );
    assert_eq!(
        depth_rate_sat_kvb(2, false, Some(6_000), Some(6_000), None),
        None
    );
}

#[test]
fn hold_defined_then_monotone_fills_tail_holes() {
    let mut r = [Some(5_000), Some(900), None, None];
    hold_defined_then_monotone(&mut r);
    assert_eq!(r, [Some(5_000), Some(900), Some(900), Some(900)]);
    let mut empty = [None, None];
    hold_defined_then_monotone(&mut empty);
    assert_eq!(empty, [None, None]);
    let mut mid = [None, Some(200), None];
    hold_defined_then_monotone(&mut mid);
    assert_eq!(mid, [None, Some(200), Some(200)]);
}

/// Piecewise-linear curve over a fixed monotone table. Expected sat/kvB
/// values are the arithmetic line, not a second copy of the estimator.
#[test]
fn fee_curve_interpolates_between_computed_depths() {
    let depths = [1, 2, 3, 4, 5, 6, 10, 20, 144, 504, 1008];
    assert_eq!(depths.len(), 11);
    let rates = [
        None,
        None,
        Some(9_000),
        Some(8_000),
        Some(7_000),
        Some(6_000),
        Some(5_000),
        Some(4_001),
        Some(3_000),
        Some(2_000),
        Some(1_000),
    ];

    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 3), Some(9_000));
    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 10), Some(5_000));
    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 1008), Some(1_000));

    let slope_depths = [10, 20];
    let slope_rates = [Some(10_000u64), Some(5_000)];
    assert_eq!(
        fee_at_target_sat_kvb(&slope_depths, &slope_rates, 10),
        Some(10_000)
    );
    assert_eq!(
        fee_at_target_sat_kvb(&slope_depths, &slope_rates, 20),
        Some(5_000)
    );
    let steps: Vec<u64> = (10..=20)
        .map(|t| fee_at_target_sat_kvb(&slope_depths, &slope_rates, t).unwrap())
        .collect();
    assert_eq!(
        steps,
        [10_000, 9_500, 9_000, 8_500, 8_000, 7_500, 7_000, 6_500, 6_000, 5_500, 5_000]
    );
    assert_ne!(steps[2], steps[0], "target 12 is not the left knot");
    let log_w = (12f64.ln() - 10f64.ln()) / (20f64.ln() - 10f64.ln());
    let log_12 = (10_000.0 + (5_000.0 - 10_000.0) * log_w).round() as u64;
    assert_ne!(steps[2], log_12, "target 12 is not a log-depth blend");

    // Halfway between 20 (4001) and 144 (3000) is block 82: 3500.5 → 3501.
    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 82), Some(3_501));

    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 1), None);
    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 2), None);
    assert_eq!(fee_at_target_sat_kvb(&slope_depths, &slope_rates, 9), None);

    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 1009), Some(1_000));
    assert_eq!(fee_at_target_sat_kvb(&depths, &rates, 5000), Some(1_000));

    let hold_only = [10u32, 20, 144];
    let hold_rates = [Some(5_000u64), None, None];
    assert_eq!(
        fee_at_target_sat_kvb(&hold_only, &hold_rates, 15),
        Some(5_000)
    );
    assert_eq!(
        fee_at_target_sat_kvb(&hold_only, &hold_rates, 100),
        Some(5_000)
    );
    assert_eq!(fee_at_target_sat_kvb(&hold_only, &hold_rates, 20), None);

    let mut prev: Option<u64> = None;
    for target in 1..=1008 {
        let rate = fee_at_target_sat_kvb(&depths, &rates, target);
        if let Some(rate) = rate {
            if let Some(prev) = prev {
                assert!(rate <= prev, "target {target} rose above {prev} to {rate}");
            }
            prev = Some(rate);
        }
    }
    assert!(prev.is_some());
}

#[test]
fn monotone_desc_clips_rises() {
    let mut r = [5_000u64, 6_000, 2_000, 3_000];
    enforce_monotone_desc(&mut r);
    assert_eq!(r, [5_000, 5_000, 2_000, 2_000]);
    let mut flat = [4_000u64, 4_000, 1_000];
    enforce_monotone_desc(&mut flat);
    assert_eq!(flat, [4_000, 4_000, 1_000]);
}

/// Bucket count is one past the edges. `estimatesmartfee` on an empty
/// pool returns "Insufficient data" and does not read this count. A warm
/// pool's published rate indexes these buckets.
#[test]
fn bucket_count_is_one_past_the_edges() {
    assert_eq!(bucket_count(), FEE_BUCKET_EDGES_SAT_PER_KVB.len() + 1);
}

/// Percentile of recorded feerates. An empty `estimatesmartfee` answer
/// does not rank any samples. A warm estimate uses this index.
#[test]
fn percentile_sat_picks_the_ranked_index() {
    assert_eq!(percentile_sat(vec![], 50), None);
    assert_eq!(percentile_sat(vec![10, 40, 20, 30], 0), Some(10));
    assert_eq!(percentile_sat(vec![10, 40, 20, 30], 50), Some(20));
    assert_eq!(percentile_sat(vec![10, 40, 20, 30], 100), Some(40));
}

/// Geometric buckets: min relay through 1000 sat/vB, ~100 edges per decade.
#[test]
fn log_buckets_span_min_relay_through_1000_sat_vb() {
    let edges = &FEE_BUCKET_EDGES_SAT_PER_KVB;
    assert_eq!(edges[0], 100, "starts at min relay");
    assert_eq!(*edges.last().unwrap(), 1_000_000, "reaches 1000 sat/vB");
    for w in edges.windows(2) {
        assert!(w[0] < w[1], "edges must rise: {} then {}", w[0], w[1]);
    }
    for (lo, hi) in [
        (100u64, 1_000),
        (1_000, 10_000),
        (10_000, 100_000),
        (100_000, 1_000_000),
    ] {
        let n = edges.iter().filter(|e| (lo..hi).contains(e)).count();
        assert!(
            (90..=110).contains(&n),
            "decade {lo}..{hi} has {n} edges, want about 100"
        );
    }
    for anchor in [100u64, 1_000, 10_000, 100_000, 1_000_000] {
        assert!(edges.contains(&anchor), "decade anchor {anchor} missing");
    }
}

/// A point mass quotes its own log step. 0.26 sat/vB must not fall to 0.2,
/// and 4.9 must not fall to 2.0.
#[test]
fn point_mass_quotes_its_log_step() {
    let stock = |_r: u64| 0u64;
    let rates = fine_candidate_rates();
    // 3_000 WU/s × 600 s × λ2 = 3.6e6 WU, over the 3.2e6 N=1 cap.
    for (rate, lo, hi) in [(260u64, 200u64, 300u64), (4_900, 2_000, 4_900)] {
        let mut inflow = vec![0u64; bucket_count()];
        inflow[bucket_index(rate)] = 3_000;
        let quoted = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
        assert!(
            quoted > lo && quoted <= hi,
            "rate {rate} quoted {quoted}, want ({lo}, {hi}]"
        );
    }
}

/// A point mass quotes the greatest edge at or below its rate.
#[test]
fn point_mass_quotes_its_bucket_floor() {
    let stock = |_r: u64| 0u64;
    let rates = fine_candidate_rates();
    // 3_000 WU/s × 600 s × λ2 = 3.6e6 WU, over the 3.2e6 N=1 cap.
    for rate in [1_500u64, 4_900] {
        let mut inflow = vec![0u64; bucket_count()];
        let floor = FEE_BUCKET_EDGES_SAT_PER_KVB[bucket_index(rate)];
        inflow[bucket_index(rate)] = 3_000;
        let quoted = min_rate_for_capacity(stock, &inflow, 1, &rates).unwrap();
        assert_eq!(quoted, floor, "rate {rate}");
        assert!(quoted <= rate);
    }
}

#[test]
fn inflow_horizon_is_capped() {
    assert_eq!(inflow_horizon_secs(1), 600);
    assert_eq!(inflow_horizon_secs(144), INFLOW_HORIZON_CAP_SECS);
    assert_eq!(fine_candidate_rates(), default_candidate_rates());
    assert_eq!(fine_candidate_rates()[0], 100);
    assert_eq!(*fine_candidate_rates().last().unwrap(), 1_000_000);
}

#[test]
fn underfull_live_stock_defines_near_not_far() {
    assert!(blend_weight(5) >= NEAR_BLEND_FLOOR);
    assert!(blend_weight(6) < NEAR_BLEND_FLOOR);
    let min_r = 100u64;
    assert_eq!(flow_for_depth(None, None, true, 1, min_r), Some(min_r));
    assert_eq!(flow_for_depth(None, None, true, 5, min_r), Some(min_r));
    assert_eq!(flow_for_depth(None, None, true, 6, min_r), None);
    assert_eq!(flow_for_depth(None, None, true, 144, min_r), None);
    assert_eq!(flow_for_depth(None, None, false, 1, min_r), None);
    assert_eq!(
        flow_for_depth(Some(5_000), None, true, 1, min_r),
        Some(5_000)
    );
    assert_eq!(flow_for_depth(Some(5_000), None, true, 144, min_r), None);
    assert_eq!(
        flow_for_depth(Some(4_000), Some(3_000), true, 1, min_r),
        Some(4_000)
    );
    assert_eq!(
        flow_for_depth(None, Some(3_000), true, 144, min_r),
        Some(3_000)
    );
}
