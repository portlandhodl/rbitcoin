//! Process meters behind the 5s DEBUG `tip: perf` line and `/metrics`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic event count. `/metrics` reads the running total; the `tip: perf`
/// line takes the change since its previous sample.
#[derive(Debug, Default)]
pub struct PerfCounter {
    pub total: AtomicU64,
    pub window_mark: AtomicU64,
}

impl PerfCounter {
    pub const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            window_mark: AtomicU64::new(0),
        }
    }

    pub fn add(&self, n: u64) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Count since the previous call. The running total is untouched.
    pub fn take_window(&self) -> u64 {
        let now = self.total();
        now.saturating_sub(self.window_mark.fetch_max(now, Ordering::Relaxed))
    }
}

/// Largest value noted since the previous [`Self::take`]. A window maximum
/// cannot be derived from running totals, so this one resets.
#[derive(Debug, Default)]
pub struct PerfMax(AtomicU64);

impl PerfMax {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn note(&self, v: u64) {
        self.0.fetch_max(v, Ordering::Relaxed);
    }

    pub fn take(&self) -> u64 {
        self.0.swap(0, Ordering::Relaxed)
    }
}

/// Request count, summed wall (µs), and window max wall for one serve
/// surface (Esplora REST, Electrum JSON-RPC).
#[derive(Debug, Default)]
pub struct RequestMeter {
    pub requests: PerfCounter,
    pub wall_us: PerfCounter,
    pub max_us: PerfMax,
}

impl RequestMeter {
    pub const fn new() -> Self {
        Self {
            requests: PerfCounter::new(),
            wall_us: PerfCounter::new(),
            max_us: PerfMax::new(),
        }
    }

    pub fn note(&self, wall_us: u64) {
        self.requests.add(1);
        self.wall_us.add(wall_us);
        self.max_us.note(wall_us);
    }

    /// `(count, sum_us, max_us)` since the previous call (`tip: perf`).
    pub fn take_window(&self) -> (u64, u64, u64) {
        (
            self.requests.take_window(),
            self.wall_us.take_window(),
            self.max_us.take(),
        )
    }

    /// Running `(requests, sum_us)` (`/metrics`).
    pub fn totals(&self) -> (u64, u64) {
        (self.requests.total(), self.wall_us.total())
    }
}
