//! Flow-aware fee projection (Engine v2 pure math).
//!
//! Capacity always uses a fixed 10-minute block clock:
//! `capacity_wu(N) = N × BLOCK_WEIGHT_WU`. Live stock + projected inflow above
//! a candidate rate decide the minimum inclusion feerate.

/// Consensus block weight (WU) used as one-block capacity.
pub const BLOCK_WEIGHT_WU: u64 = 4_000_000;

/// Seconds per planned block (product clock; not wall time since last tip).
pub const SECONDS_PER_BLOCK: u64 = 600;

/// Inclusion confidence at N=1 (10-minute default). Higher → higher sat/vB.
pub const CONFIDENCE_NEAR: f64 = 0.999;
/// Inclusion confidence for historical/blended targets from N=2 onward.
pub const CONFIDENCE_FAR: f64 = 0.99;
/// Fraction of `N×4e6` WU to fill at `CONFIDENCE_NEAR` (leave shock room).
pub const FILL_AT_NEAR: f64 = 0.80;
/// Fraction of `N×4e6` WU to fill at `CONFIDENCE_FAR` (today's 95% haircut).
pub const FILL_AT_FAR: f64 = 0.95;
/// Inflow EMA multiplier at `CONFIDENCE_NEAR`.
pub const LAMBDA_AT_NEAR: f64 = 2.0;
/// Inflow EMA multiplier at `CONFIDENCE_FAR`.
pub const LAMBDA_AT_FAR: f64 = 1.0;

/// Admit-rate floors in sat/kvB. 100 geometric steps per decade from min
/// relay (0.1 sat/vB) through 1000 sat/vB. The same grid is the
/// inclusion-search ladder. The last bucket is everything above that.
#[doc(hidden)]
pub const FLOW_BUCKETS_PER_DECADE: usize = 100;
#[doc(hidden)]
pub const FLOW_BUCKET_DECADES: usize = 4;
#[doc(hidden)]
pub const FLOW_BUCKET_EDGE_COUNT: usize = FLOW_BUCKET_DECADES * FLOW_BUCKETS_PER_DECADE + 1;

#[doc(hidden)]
pub const fn flow_bucket_edges() -> [u64; FLOW_BUCKET_EDGE_COUNT] {
    /// `ln(10) * 10^12`, truncated. The exp series below rounds each edge.
    const SCALE: u128 = 1_000_000_000_000;
    const LN10_SCALE: u128 = 2_302_585_092_994;
    let min_relay = rbitcoin_consensus::policy::MIN_RELAY_FEE_RATE_SAT_PER_KVB as u128;
    let mut edges = [0u64; FLOW_BUCKET_EDGE_COUNT];
    let mut i = 0usize;
    while i < FLOW_BUCKET_EDGE_COUNT {
        let y = (i as u128) * LN10_SCALE / (FLOW_BUCKETS_PER_DECADE as u128);
        let mut term = SCALE;
        let mut acc = term;
        let mut n = 1u128;
        while n < 40 {
            term = term * y / (n * SCALE);
            acc += term;
            n += 1;
        }
        edges[i] = ((min_relay * acc + SCALE / 2) / SCALE) as u64;
        i += 1;
    }
    edges
}

pub const FEE_BUCKET_EDGES_SAT_PER_KVB: [u64; FLOW_BUCKET_EDGE_COUNT] = flow_bucket_edges();

/// Index of the bucket that contains `rate_sat_per_kvb` (0 = lowest).
///
/// A rate below the first edge shares that bucket. A rate strictly above
/// the last edge is the open top (`edges.len()`).
pub fn bucket_index(rate_sat_per_kvb: u64) -> usize {
    let edges = &FEE_BUCKET_EDGES_SAT_PER_KVB;
    let at_or_below = edges.partition_point(|&edge| edge <= rate_sat_per_kvb);
    if at_or_below == 0 {
        return 0;
    }
    if rate_sat_per_kvb > edges[edges.len() - 1] {
        edges.len()
    } else {
        at_or_below - 1
    }
}

/// Number of buckets (edges + open top).
pub fn bucket_count() -> usize {
    FEE_BUCKET_EDGES_SAT_PER_KVB.len() + 1
}

/// Capacity for target block count N (≥ 1).
pub fn capacity_wu(n_blocks: u32) -> u64 {
    let n = n_blocks.max(1) as u64;
    n.saturating_mul(BLOCK_WEIGHT_WU)
}

/// Horizon seconds for target N.
pub fn horizon_secs(n_blocks: u32) -> u64 {
    let n = n_blocks.max(1) as u64;
    n.saturating_mul(SECONDS_PER_BLOCK)
}

/// Flow capacity confidence: 0.999 at N=1 and 0.99 at every farther depth.
#[doc(hidden)]
pub fn inclusion_confidence(n_blocks: u32) -> f64 {
    if n_blocks <= 1 {
        CONFIDENCE_NEAR
    } else {
        CONFIDENCE_FAR
    }
}

#[doc(hidden)]
pub fn lerp_conf(c: f64, y_near: f64, y_far: f64) -> f64 {
    let span = CONFIDENCE_NEAR - CONFIDENCE_FAR;
    if span <= 0.0 {
        return y_far;
    }
    let t = ((c - CONFIDENCE_FAR) / span).clamp(0.0, 1.0);
    y_far + t * (y_near - y_far)
}

/// Block-fill fraction at confidence `c` (higher `c` → less of the block).
#[doc(hidden)]
pub fn fill_frac(c: f64) -> f64 {
    lerp_conf(c, FILL_AT_NEAR, FILL_AT_FAR)
}

/// Inflow EMA multiplier at confidence `c` (higher `c` → more assumed λ).
#[doc(hidden)]
pub fn lambda_mult(c: f64) -> f64 {
    lerp_conf(c, LAMBDA_AT_NEAR, LAMBDA_AT_FAR)
}

/// Effective capacity after the confidence fill fraction.
pub fn effective_capacity_wu(n_blocks: u32) -> u64 {
    let fill = fill_frac(inclusion_confidence(n_blocks));
    (capacity_wu(n_blocks) as f64 * fill).round() as u64
}

/// Weight over `horizon_secs` from each bucket through the open top.
/// `suffix[i]` is buckets `i..`, and `suffix[n]` is 0.
#[doc(hidden)]
pub fn inflow_suffix_wu(inflow_wu_per_s_by_bucket: &[u64], horizon_secs: u64) -> Vec<u64> {
    let n = bucket_count().min(inflow_wu_per_s_by_bucket.len());
    let mut suffix = vec![0u64; n + 1];
    for i in (0..n).rev() {
        suffix[i] =
            suffix[i + 1].saturating_add(inflow_wu_per_s_by_bucket[i].saturating_mul(horizon_secs));
    }
    suffix
}

/// First bucket whose floor is strictly above `rate`, or [`bucket_count`]
/// when every floor is at or below it.
#[doc(hidden)]
pub fn first_bucket_above(rate_sat_per_kvb: u64) -> usize {
    let edges = &FEE_BUCKET_EDGES_SAT_PER_KVB;
    if let Some(i) = edges.iter().position(|&floor| floor > rate_sat_per_kvb) {
        return i;
    }
    let open_floor = edges.last().copied().unwrap_or(0).saturating_add(1);
    if open_floor > rate_sat_per_kvb {
        edges.len()
    } else {
        edges.len().saturating_add(1)
    }
}

/// Minimum feerate (sat/kvB) such that
/// `stock_above(R) + projected_in(R, H) ≤ effective_capacity(N)`.
///
/// `stock_above` is a callback (live mining-chunk weight strictly above R).
/// `candidate_rates` is searched ascending (bucket edges + optional extras).
/// Returns `None` if even the highest candidate cannot fit (caller applies floors).
pub fn min_rate_for_capacity<F>(
    stock_above: F,
    inflow_wu_per_s_by_bucket: &[u64],
    n_blocks: u32,
    candidate_rates: &[u64],
) -> Option<u64>
where
    F: Fn(u64) -> u64,
{
    let cap = effective_capacity_wu(n_blocks);
    let h = inflow_horizon_secs(n_blocks);
    let lam = lambda_mult(inclusion_confidence(n_blocks));
    let suffix = inflow_suffix_wu(inflow_wu_per_s_by_bucket, h);
    let mut best: Option<u64> = None;
    for &r in candidate_rates {
        let idx = first_bucket_above(r).min(suffix.len().saturating_sub(1));
        let stressed = (suffix[idx] as f64 * lam).round() as u64;
        let load = stock_above(r).saturating_add(stressed);
        if load <= cap {
            best = Some(match best {
                Some(b) => b.min(r),
                None => r,
            });
        }
    }
    best
}

/// Default candidate ladder: bucket edges.
pub fn default_candidate_rates() -> Vec<u64> {
    FEE_BUCKET_EDGES_SAT_PER_KVB.to_vec()
}

/// Inclusion-search ladder. Same floors as the admit-rate buckets.
pub fn fine_candidate_rates() -> Vec<u64> {
    default_candidate_rates()
}

/// Trust admit-EMA at most this many seconds (≈ 4× 150s half-life).
pub const INFLOW_HORIZON_CAP_SECS: u64 = 600;
/// Blend length in blocks (~1 hour). `w(1)=1`, `w(6)≈0.43`, `w(144)≈0`.
pub const BLEND_N0: f64 = 6.0;
/// Under-full pool may still answer min-relay when `blend_weight ≥` this (N=1–5).
pub const NEAR_BLEND_FLOOR: f64 = 0.5;

/// Horizon used for inflow projection (capped; not N×10 minutes for N=144).
pub fn inflow_horizon_secs(n_blocks: u32) -> u64 {
    horizon_secs(n_blocks).min(INFLOW_HORIZON_CAP_SECS)
}

/// Weight on the live flow estimate vs historical blocks.
pub fn blend_weight(n_blocks: u32) -> f64 {
    let n = f64::from(n_blocks.max(1));
    (-(n - 1.0) / BLEND_N0).exp()
}

/// Combine projected inflow with the N-block frontier.
///
/// If the pool is thinner than N blocks, live stock does not set a far rate
/// (`w < NEAR_BLEND_FLOOR`). Near depths with any live stock still answer
/// min-relay (everything fits now).
pub fn flow_for_depth(
    projected: Option<u64>,
    frontier: Option<u64>,
    has_live_stock: bool,
    n_blocks: u32,
    min_relay: u64,
) -> Option<u64> {
    let mut flow = match (projected, frontier) {
        (Some(p), Some(f)) => Some(p.max(f)),
        (Some(p), None) => Some(p),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    };
    if frontier.is_none() {
        if blend_weight(n_blocks) >= NEAR_BLEND_FLOOR {
            if flow.is_none() && has_live_stock {
                flow = Some(min_relay);
            }
        } else {
            flow = None;
        }
    }
    flow
}

/// `pct` in 0..=100. Empty → None.
pub fn percentile_sat(mut v: Vec<u64>, pct: u8) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let pct = pct.min(100) as usize;
    let i = (v.len() - 1).saturating_mul(pct) / 100;
    Some(v[i])
}

/// Vsize-weighted p10 of individual transactions in one block.
pub fn block_individual_p10_sat_kvb(txs: &[(u64, u64)], min_relay: u64) -> Option<u64> {
    use rbitcoin_consensus::policy::fee_rate_sat_per_kvb;

    let mut rates: Vec<(u64, u64)> = txs
        .iter()
        .filter_map(|&(fee, weight)| {
            if weight == 0 {
                return None;
            }
            let rate = fee_rate_sat_per_kvb(fee, weight);
            (rate >= min_relay).then_some((rate, weight.saturating_add(3) / 4))
        })
        .collect();
    if rates.is_empty() {
        return None;
    }
    rates.sort_unstable_by_key(|(rate, _)| *rate);
    let total_vsize = rates.iter().map(|(_, vsize)| *vsize).sum::<u64>();
    let cutoff = total_vsize.saturating_mul(10).div_ceil(100).max(1);
    let mut seen = 0u64;
    for (rate, vsize) in rates {
        seen = seen.saturating_add(vsize);
        if seen >= cutoff {
            return Some(rate);
        }
    }
    None
}

/// `w·flow + (1-w)·hist` with `w = blend_weight(N)`. Missing side drops out.
#[doc(hidden)]
pub fn blend_sat_kvb(flow: Option<u64>, hist: Option<u64>, n_blocks: u32) -> Option<u64> {
    match (flow, hist) {
        (Some(f), Some(h)) => {
            let w = blend_weight(n_blocks);
            Some((w * f as f64 + (1.0 - w) * h as f64).round() as u64)
        }
        (flow, hist) => flow.or(hist),
    }
}

/// `(1-α)·cold + α·warm`. `α` outside `0..=1` clamps.
///
/// A missing warm side leaves the cold quote. Flow with no history answers
/// only at α = 1, so a thin pool cannot invent a rate.
pub fn warmup_blend(alpha: f64, cold: Option<u64>, warm: Option<u64>) -> Option<u64> {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        0.0
    };
    match (cold, warm) {
        (Some(c), Some(w)) => Some(((1.0 - alpha) * c as f64 + alpha * w as f64).round() as u64),
        (Some(c), None) => (alpha < 1.0).then_some(c),
        (None, Some(w)) => (alpha >= 1.0).then_some(w),
        (None, None) => None,
    }
}

/// One published depth: cold at α = 0, warm at α = 1.
///
/// The N=1 confirm-memory floor is part of the warm quote. α = 0 leaves it
/// out, including when history and flow are both missing.
pub fn published_depth_rate(
    n_blocks: u32,
    alpha: f64,
    flow: Option<u64>,
    frontier: Option<u64>,
    hist: Option<u64>,
    confirm_floor: Option<u64>,
) -> Option<u64> {
    let cold = depth_rate_sat_kvb(n_blocks, false, flow, frontier, hist);
    let mut warm = depth_rate_sat_kvb(n_blocks, true, flow, frontier, hist);
    if n_blocks <= 1 && alpha.is_finite() && alpha > 0.0 {
        warm = match (warm, confirm_floor) {
            (Some(rate), Some(floor)) => Some(rate.max(floor)),
            (None, Some(floor)) => Some(floor),
            (warm, None) => warm,
        };
    }
    warmup_blend(alpha, cold, warm)
}

/// One target's rate (sat/kvB) before the monotone pass.
///
/// Once flow is warm, the 1-block target is flow (history only when flow has
/// nothing to say) and farther targets blend flow with history by `w(N)`, so
/// neither is a floor for the other. While flow is cold, history answers
/// alone and the live pool may only raise it: a restarted pool can be thin
/// or missing what peers relayed while this node was down.
pub fn depth_rate_sat_kvb(
    n_blocks: u32,
    flow_warm: bool,
    flow: Option<u64>,
    frontier: Option<u64>,
    hist: Option<u64>,
) -> Option<u64> {
    if !flow_warm {
        return hist.map(|h| frontier.map_or(h, |f| h.max(f)));
    }
    if n_blocks <= 1 {
        flow.or(hist)
    } else {
        blend_sat_kvb(flow, hist, n_blocks)
    }
}

/// Enforce R(1) ≥ R(2) ≥ … in place (depths already sorted ascending).
pub fn enforce_monotone_desc(rates: &mut [u64]) {
    for i in 1..rates.len() {
        rates[i] = rates[i].min(rates[i - 1]);
    }
}

/// After the first defined depth, hold that rate through later holes, then
/// monotone. Trailing insufficient (no hist, pool thinner than N) must not
/// bounce above a cheaper mid estimate (Esplora maps -1 to 1.0 sat/vB).
pub fn hold_defined_then_monotone(rates: &mut [Option<u64>]) {
    let mut last = None;
    for r in rates.iter_mut() {
        match *r {
            Some(v) => last = Some(v),
            None => {
                if let Some(v) = last {
                    *r = Some(v);
                }
            }
        }
    }
    let mut filled: Vec<u64> = rates.iter().copied().flatten().collect();
    enforce_monotone_desc(&mut filled);
    let mut i = 0;
    for r in rates.iter_mut() {
        if r.is_some() {
            *r = Some(filled[i]);
            i += 1;
        }
    }
}

/// Confirm-target rate (sat/kvB) from already computed depths.
///
/// `depths` is ascending and the same length as `rates`. `None` is no rate.
/// An exact depth returns that depth's rate. Between two depths that both
/// have rates, the answer is linear in the block count: nearest sat/kvB,
/// halves away from zero, clamped to the two endpoints. A missing side of
/// that interval holds the nearest defined rate at or below the target. A
/// target above the last defined depth holds that rate. A target below every
/// defined depth has no rate.
pub fn fee_at_target_sat_kvb(depths: &[u32], rates: &[Option<u64>], target: u32) -> Option<u64> {
    let n = depths.len();
    if n == 0 || rates.len() != n {
        return None;
    }
    if target > depths[n - 1] {
        return rates.iter().copied().rev().find_map(|r| r);
    }
    let idx = depths.partition_point(|&d| d < target);
    if idx >= n {
        return rates.iter().copied().rev().find_map(|r| r);
    }
    if depths[idx] == target {
        return rates[idx];
    }
    if idx == 0 {
        return None;
    }
    let lo = idx - 1;
    match (rates[lo], rates[idx]) {
        (Some(left), Some(right)) => {
            Some(lerp_sat_kvb(depths[lo], left, depths[idx], right, target))
        }
        (Some(left), None) => Some(left),
        (None, _) => rates[..=lo].iter().copied().rev().find_map(|r| r),
    }
}

/// `left` at `d0`, `right` at `d1`, straight line at `target` in between.
#[doc(hidden)]
pub fn lerp_sat_kvb(d0: u32, left: u64, d1: u32, right: u64, target: u32) -> u64 {
    let span = u128::from(d1.saturating_sub(d0));
    if span == 0 {
        return left;
    }
    let dist = u128::from(target.saturating_sub(d0)).min(span);
    let numer = u128::from(left) * (span - dist) + u128::from(right) * dist;
    let mut quote = numer / span;
    let rem = numer % span;
    if rem * 2 >= span {
        quote += 1;
    }
    let lo = u128::from(left.min(right));
    let hi = u128::from(left.max(right));
    quote.clamp(lo, hi) as u64
}
