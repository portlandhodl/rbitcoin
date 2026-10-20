//! Live progress of long-running stages (index build, rebuild, backfill),
//! readable at any log level.
//!
//! The stages already compute `done/total` for their INFO lines; this keeps the
//! same numbers where the health listener (`GET /progress`, `/metrics`) can
//! read them, so an operator running at `warn` still sees where a multi-hour
//! build is. [`begin`] registers a stage and returns its guard; workers report
//! through the guard, and dropping it (done, error, cancel, or panic)
//! unregisters the stage and records its final numbers, which [`view`] reports
//! as the last finished stage. Stages that overlap keep separate counts. The
//! registry is per process: nodes sharing one process (tests) see each other's
//! stages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

#[doc(hidden)]
pub struct Entry {
    pub stage: &'static str,
    pub total: AtomicU64,
    pub base: u64,
    pub started: Instant,
    pub started_at: SystemTime,
    pub done: AtomicU64,
}

impl Entry {
    #[doc(hidden)]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            stage: self.stage,
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            base: self.base,
            elapsed: self.started.elapsed(),
            started_at: self.started_at,
        }
    }
}

/// Running stages, oldest first, and the last one to end. One lock, so a
/// reader never sees a stage in neither place.
#[doc(hidden)]
pub struct Registry {
    pub active: Vec<Arc<Entry>>,
    pub last_finished: Option<Snapshot>,
}

#[doc(hidden)]
pub static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    active: Vec::new(),
    last_finished: None,
});

#[doc(hidden)]
pub fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    static CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CAPTURED: std::cell::RefCell<Vec<Snapshot>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record the final numbers of stages that end on this thread, for
/// [`take_finished`]. Either call drops anything recorded before it. Off in
/// production; tests use it because the registry is process-global and other
/// tests run stages concurrently.
pub fn capture_finished(on: bool) {
    CAPTURE.with(|c| c.set(on));
    let _ = take_finished();
}

/// Drain stages recorded after [`capture_finished`]`(true)`, oldest first.
pub fn take_finished() -> Vec<Snapshot> {
    CAPTURED.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// A point-in-time view of one stage.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub stage: &'static str,
    pub done: u64,
    pub total: u64,
    /// `done` when the stage began: work finished before a restart.
    pub base: u64,
    pub elapsed: Duration,
    /// Wall-clock time the stage began, read once at [`begin_at`].
    pub started_at: SystemTime,
}

impl Snapshot {
    /// `done / total` as a percentage rounded down to hundredths (so 100.0
    /// only when done), or `None` when the total is unknown (0). Integer
    /// math: a float floor would turn 57/100 into 56.99.
    pub fn percent(&self) -> Option<f64> {
        (self.total > 0).then(|| {
            let hundredths =
                u128::from(self.done.min(self.total)) * 10_000 / u128::from(self.total);
            hundredths as f64 / 100.0
        })
    }

    /// Time left at the rate of this run (`done - base` in `elapsed`), once
    /// this run has done some work and while short of `total`.
    pub fn eta(&self) -> Option<Duration> {
        let done = self.done.min(self.total);
        let ran = done.checked_sub(self.base).filter(|&r| r > 0)?;
        let left = self.total.checked_sub(done).filter(|&l| l > 0)?;
        Duration::try_from_secs_f64(self.elapsed.as_secs_f64() * left as f64 / ran as f64).ok()
    }
}

/// A registered stage. Share `&Stage` with worker threads to report; the
/// stage unregisters when this drops.
#[must_use = "the stage ends when this guard is dropped"]
pub struct Stage(Arc<Entry>);

impl Stage {
    /// Record cumulative progress. Never moves backwards, so concurrent
    /// workers may report out of order.
    pub fn set_done(&self, done: u64) {
        self.0.done.fetch_max(done, Ordering::Relaxed);
    }

    /// Record `n` more units of progress.
    pub fn add_done(&self, n: u64) {
        self.0.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Replace the total, for a stage whose target moves while it runs (an
    /// index build chasing a tip that keeps advancing).
    pub fn set_total(&self, total: u64) {
        self.0.total.store(total, Ordering::Relaxed);
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let fin = self.0.snapshot();
        if CAPTURE.with(|c| c.get()) {
            CAPTURED.with(|c| c.borrow_mut().push(fin.clone()));
        }
        let mut reg = registry();
        reg.active.retain(|e| !Arc::ptr_eq(e, &self.0));
        reg.last_finished = Some(fin);
    }
}

/// Register `stage` with `total` units of work (0 = unknown).
pub fn begin(stage: &'static str, total: u64) -> Stage {
    begin_at(stage, total, 0)
}

/// [`begin`] for a resumed stage: `done` starts at `base`, the units finished
/// before a restart, and [`Snapshot::eta`] counts only this run's rate.
pub fn begin_at(stage: &'static str, total: u64, base: u64) -> Stage {
    let entry = Arc::new(Entry {
        stage,
        total: AtomicU64::new(total),
        base,
        started: Instant::now(),
        started_at: SystemTime::now(),
        done: AtomicU64::new(base),
    });
    registry().active.push(Arc::clone(&entry));
    Stage(entry)
}

/// Every running stage, oldest first.
pub fn snapshots() -> Vec<Snapshot> {
    registry().active.iter().map(|e| e.snapshot()).collect()
}

/// The most recently begun stage still running (the innermost when stages
/// nest, the one an operator is waiting on) and the stage that ended most
/// recently, with its numbers at the end (`done < total` when it stopped
/// early). One lock, so a stage that ends meanwhile is never reported as
/// both.
pub fn view() -> (Option<Snapshot>, Option<Snapshot>) {
    let reg = registry();
    (
        reg.active.last().map(|e| e.snapshot()),
        reg.last_finished.clone(),
    )
}
