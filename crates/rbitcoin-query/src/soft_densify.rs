//! Body-queue soft densify assign policy (IBD getdata window).

/// Soft assign free floor (~100 MiB). Under this payload size, densify uses the
/// usual ahead horizon (net-side densify cap). Over it, densify is limited to
/// the confirm-time window ([`BQ_SOFT_CONFIRM_SECS`]).
///
/// Tunable constant — no hysteresis band (single threshold).
pub const BQ_SOFT_FREE_BYTES: u64 = 100 * 1024 * 1024;

/// When body-queue payload is over [`BQ_SOFT_FREE_BYTES`], only assign getdata
/// for heights confirm will consume in this many seconds at the current tip
/// rate. Tunable constant — no hysteresis band.
pub const BQ_SOFT_CONFIRM_SECS: f64 = 60.0;

/// Default densify assign-stop (1 GiB). At/over this, densify only holes within
/// the ~1 min tip-rate confirm window **and** not past `fetched_hi` (do not grow
/// past fetched; do not densify far holes outside the window). Override with
/// `RBITCOIN_BLOCK_QUEUE_BYTES` / `_GB` (`0` = unlimited).
pub const BQ_ASSIGN_STOP_BYTES: u64 = 1024 * 1024 * 1024;

/// Blocks confirm can take in one soft confirm window at `rate` (ceil).
///
/// Rate unknown / non-positive → `0` (no densify ahead when restricted).
pub fn soft_confirm_window_n(rate_blocks_per_s: Option<f64>) -> u32 {
    let rate = rate_blocks_per_s
        .filter(|r| r.is_finite() && *r > 1e-9)
        .unwrap_or(0.0);
    (rate * BQ_SOFT_CONFIRM_SECS).ceil() as u32
}

/// True when BQ payload is over the free-byte floor (densify uses confirm window).
#[inline]
pub fn soft_assign_restricted(depth_bytes: u64) -> bool {
    depth_bytes > BQ_SOFT_FREE_BYTES
}

/// True when BQ payload is at/over the densify assign-stop (`0` / `u64::MAX` = off).
#[inline]
pub fn soft_assign_stopped(depth_bytes: u64, stop_bytes: u64) -> bool {
    stop_bytes != 0 && stop_bytes != u64::MAX && depth_bytes >= stop_bytes
}

/// Assign-stop from env, or [`BQ_ASSIGN_STOP_BYTES`]. `_BYTES` wins over `_GB`.
/// `0` means unlimited (`u64::MAX`). Invalid parse falls through.
pub fn bq_assign_stop_bytes() -> u64 {
    if let Ok(s) = std::env::var("RBITCOIN_BLOCK_QUEUE_BYTES") {
        if let Ok(n) = s.parse::<u64>() {
            return if n == 0 { u64::MAX } else { n };
        }
    }
    if let Ok(s) = std::env::var("RBITCOIN_BLOCK_QUEUE_GB") {
        if let Ok(n) = s.parse::<u64>() {
            return if n == 0 {
                u64::MAX
            } else {
                n.saturating_mul(1024 * 1024 * 1024)
            };
        }
    }
    BQ_ASSIGN_STOP_BYTES
}

/// Inclusive densify band high height for getdata assign.
///
/// - **At/over** `assign_stop_bytes`: holes only within the ~1 min tip-rate
///   confirm window **and** not past `fetched_hi` (do not grow past fetched;
///   do not densify far holes outside the window). No usable fetched range →
///   empty band.
/// - **Under** [`BQ_SOFT_FREE_BYTES`]: full `densify_hi` (usual densify ahead).
/// - **Over** free bytes (and under assign-stop): only heights confirm will
///   pick up within [`BQ_SOFT_CONFIRM_SECS`] at current rate —
///   `path_lo .. path_lo+window-1` (clamped to `densify_hi`). Rate cold →
///   only `path_lo` (tip-adjacent).
///
/// **Never** gates peer TCP reads or [`Query::block_queue_offer`].
pub fn soft_densify_band_hi(
    path_lo: u32,
    densify_hi: u32,
    depth_bytes: u64,
    rate_blocks_per_s: Option<f64>,
    assign_stop_bytes: u64,
    fetched_hi: Option<u32>,
) -> u32 {
    if densify_hi < path_lo {
        return densify_hi;
    }
    if soft_assign_stopped(depth_bytes, assign_stop_bytes) {
        let n = soft_confirm_window_n(rate_blocks_per_s);
        let window_hi = if n == 0 {
            path_lo.min(densify_hi)
        } else {
            path_lo.saturating_add(n.saturating_sub(1)).min(densify_hi)
        };
        return match fetched_hi {
            Some(h) if h >= path_lo => window_hi.min(h),
            _ => path_lo.saturating_sub(1),
        };
    }
    if !soft_assign_restricted(depth_bytes) {
        return densify_hi;
    }
    let n = soft_confirm_window_n(rate_blocks_per_s);
    if n == 0 {
        return path_lo.min(densify_hi);
    }
    path_lo.saturating_add(n.saturating_sub(1)).min(densify_hi)
}

/// True when over free bytes and the queue already holds at least one confirm
/// window of blocks (assign densify has little/no room left in the window).
///
/// Used for Critical assign (tip race only) when inflight is low.
pub fn soft_confirm_window_covered(
    depth_n: u32,
    depth_bytes: u64,
    rate_blocks_per_s: Option<f64>,
) -> bool {
    if !soft_assign_restricted(depth_bytes) {
        return false;
    }
    let w = soft_confirm_window_n(rate_blocks_per_s);
    if w == 0 {
        // Over free, rate unknown: treat as covered (no densify ahead).
        return true;
    }
    depth_n >= w
}

/// Queued bodies that make a tip gap a hole when the confirm rate is cold
/// or the rate window is huge.
///
/// Early blocks are small. A quarter of the default 1 GiB assign-stop is
/// 256 MiB, a very large count of those blocks. A missing tip+1 with this
/// many bodies already queued is latency-bound, so it is a hole before the
/// byte cap fills. When the rate window's own quarter is smaller, that
/// quarter still starts the race.
pub const TIP_HOLE_MIN_AHEAD_BLOCKS: u32 = 1000;

/// True when the body queue is far enough ahead to treat a tip gap as a hole.
///
/// Any one of these is enough. Fullness is the queue's aggregate block count
/// and payload bytes, gaps included. An empty queue is the frontier. No
/// height walk.
///
/// - [`TIP_HOLE_MIN_AHEAD_BLOCKS`] bodies already queued
/// - at least 1/4 of the confirm-time block window ([`soft_confirm_window_n`])
/// - at least 1/4 of `assign_stop_bytes` (default [`BQ_ASSIGN_STOP_BYTES`],
///   1 GiB). `0` and `u64::MAX` are unlimited, so that side stays off
///
/// The ~100 MiB free floor is the densify horizon, not this budget.
pub fn soft_ahead_quarter_full(
    ahead_n: u32,
    ahead_bytes: u64,
    rate_blocks_per_s: Option<f64>,
    assign_stop_bytes: u64,
) -> bool {
    if ahead_n == 0 {
        return false;
    }
    if ahead_n >= TIP_HOLE_MIN_AHEAD_BLOCKS {
        return true;
    }
    let window_n = soft_confirm_window_n(rate_blocks_per_s);
    if window_n > 0 && u64::from(ahead_n).saturating_mul(4) >= u64::from(window_n) {
        return true;
    }
    assign_stop_bytes != 0
        && assign_stop_bytes != u64::MAX
        && ahead_bytes.saturating_mul(4) >= assign_stop_bytes
}
