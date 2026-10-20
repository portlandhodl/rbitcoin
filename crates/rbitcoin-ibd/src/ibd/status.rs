//! IBD main-loop timers and sample (status / perf_log).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// Main-loop hot-path timers (atomics; reset every status sample).
///
/// Used to see whether the pegged core is Class C confirm, getdata assign,
/// event drain, or the status scan itself.
///
/// **Live confirm:** `confirm_ns` accrues when a **script** batch finishes.
/// It is **script-stage work only** (not lookup stamp, not load pin/assemble, not write).
/// Lookup/load/write walls live on `Query::confirm_stats` (sampled by `perf_log`).
/// During lookup/load claim, [`Self::confirm_live`] shows in-progress stage wall.
pub struct LoopStats {
    /// Pure script-stage wall for completed script batches (excludes lookup/load/write).
    pub confirm_ns: AtomicU64,
    /// Successful tip accepts this window.
    pub confirm_blocks: AtomicU64,
    /// Times confirm stopped on a non-skippable reject.
    pub confirm_reject_stops: AtomicU64,
    /// Wall time in `assign_work_ordered`.
    pub assign_ns: AtomicU64,
    /// Unique hashes put into getdata this window.
    pub assign_issued: AtomicU64,
    /// Wall time draining peer/archive channels.
    pub drain_ns: AtomicU64,
    /// Peer + archive events applied this window.
    pub drain_events: AtomicU64,
    /// Wall time building the status `work_chain_progress` snapshot.
    pub status_scan_ns: AtomicU64,
    /// Class A bodies published this run (any height; cumulative).
    /// Includes gap-fills below the archive high-water mark — not just HWM rises.
    pub archived_bodies: AtomicU64,
    /// In-flight confirm batch (set by confirm OS thread; status only).
    pub confirm_live: Mutex<Option<ConfirmLive>>,
}

#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct ConfirmLive {
    pub first_height: u32,
    pub batch_n: u32,
    /// Σ tx.input count over the packed batch (confirm-side pack).
    pub batch_inputs: u32,
    pub started: Instant,
}

impl Default for LoopStats {
    fn default() -> Self {
        Self {
            confirm_ns: AtomicU64::new(0),
            confirm_blocks: AtomicU64::new(0),
            confirm_reject_stops: AtomicU64::new(0),
            assign_ns: AtomicU64::new(0),
            assign_issued: AtomicU64::new(0),
            drain_ns: AtomicU64::new(0),
            drain_events: AtomicU64::new(0),
            status_scan_ns: AtomicU64::new(0),
            archived_bodies: AtomicU64::new(0),
            confirm_live: Mutex::new(None),
        }
    }
}

impl LoopStats {
    pub fn confirm_begin(&self, first_height: u32, batch_n: u32, batch_inputs: u32) {
        *self.confirm_live.lock().unwrap() = Some(ConfirmLive {
            first_height,
            batch_n,
            batch_inputs,
            started: Instant::now(),
        });
    }

    pub fn confirm_end(&self) {
        *self.confirm_live.lock().unwrap() = None;
    }

    /// `(first_height, batch_n, batch_inputs, elapsed_ms)` if a confirm batch is running.
    pub fn confirm_live_snap(&self) -> Option<(u32, u32, u32, u64)> {
        self.confirm_live.lock().unwrap().as_ref().map(|l| {
            (
                l.first_height,
                l.batch_n,
                l.batch_inputs,
                l.started.elapsed().as_millis() as u64,
            )
        })
    }

    pub fn sample_and_reset(&self) -> LoopSample {
        LoopSample {
            confirm_ns: self.confirm_ns.swap(0, Ordering::Relaxed),
            confirm_blocks: self.confirm_blocks.swap(0, Ordering::Relaxed),
            confirm_reject_stops: self.confirm_reject_stops.swap(0, Ordering::Relaxed),
            assign_ns: self.assign_ns.swap(0, Ordering::Relaxed),
            assign_issued: self.assign_issued.swap(0, Ordering::Relaxed),
            drain_ns: self.drain_ns.swap(0, Ordering::Relaxed),
            drain_events: self.drain_events.swap(0, Ordering::Relaxed),
            status_scan_ns: self.status_scan_ns.swap(0, Ordering::Relaxed),
            confirm_live: self.confirm_live_snap(),
        }
    }
}

pub struct LoopSample {
    pub confirm_ns: u64,
    pub confirm_blocks: u64,
    pub confirm_reject_stops: u64,
    pub assign_ns: u64,
    pub assign_issued: u64,
    pub drain_ns: u64,
    pub drain_events: u64,
    pub status_scan_ns: u64,
    /// Live batch if confirm engine is mid-`confirm_run`:
    /// `(first_height, batch_n, batch_inputs, elapsed_ms)`.
    pub confirm_live: Option<(u32, u32, u32, u64)>,
}

impl LoopSample {
    #[doc(hidden)]
    pub fn ms(ns: u64) -> u64 {
        ns / 1_000_000
    }
    pub fn confirm_ms(&self) -> u64 {
        Self::ms(self.confirm_ns)
    }
    pub fn assign_ms(&self) -> u64 {
        Self::ms(self.assign_ns)
    }
    pub fn drain_ms(&self) -> u64 {
        Self::ms(self.drain_ns)
    }
    pub fn status_scan_ms(&self) -> u64 {
        Self::ms(self.status_scan_ns)
    }
    pub fn confirm_us_per_block(&self) -> u64 {
        self.confirm_ns
            .checked_div(self.confirm_blocks)
            .unwrap_or(0)
            / 1000
    }
    /// Which phase dominated wall time this window (for one-glance diagnosis).
    pub fn dominant(&self) -> &'static str {
        // Completed-batch timer is 0 while a batch is still running — treat live as confirm.
        if self.confirm_live.is_some() && self.confirm_ns == 0 {
            return "confirm";
        }
        let c = self.confirm_ns;
        let a = self.assign_ns;
        let d = self.drain_ns;
        let s = self.status_scan_ns;
        let m = c.max(a).max(d).max(s);
        if m == 0 {
            "idle"
        } else if m == c {
            "confirm"
        } else if m == a {
            "assign"
        } else if m == d {
            "drain"
        } else {
            "status_scan"
        }
    }
}
