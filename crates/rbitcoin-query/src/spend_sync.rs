//! Background device barrier for spend replay stems.
//!
//! Confirm records a snapshot height and does not `sync_data`. This thread
//! does, about every ten minutes, and publishes only the height it read
//! before the barrier.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rbitcoin_store::{elapsed_after_checkpoint, SPEND_DURABLE_INTERVAL_MS};

use crate::Query;

pub struct SpendSync {
    pub wake: Arc<(Mutex<bool>, Condvar)>,
    pub join: Option<JoinHandle<()>>,
}

impl SpendSync {
    pub fn spawn(query: Arc<Query>) -> Self {
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let flag = Arc::clone(&wake);
        let join = std::thread::Builder::new()
            .name("rbtc-spend-sync".into())
            .spawn(move || run(query, flag))
            .expect("spawn spend sync");
        Self {
            wake,
            join: Some(join),
        }
    }

    /// One checkpoint at the current snapshot, then join.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    #[doc(hidden)]
    pub fn stop_and_join(&mut self) {
        {
            let (lock, cv) = &*self.wake;
            let mut stop = lock.lock().unwrap();
            *stop = true;
            cv.notify_one();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for SpendSync {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[doc(hidden)]
pub fn run(query: Arc<Query>, wake: Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &*wake;
    let mut since_ok = Instant::now();
    loop {
        let stop = {
            let guard = lock.lock().unwrap();
            if *guard {
                true
            } else {
                let elapsed = since_ok.elapsed().as_millis() as u64;
                let wait = SPEND_DURABLE_INTERVAL_MS.saturating_sub(elapsed);
                if wait == 0 {
                    *guard
                } else {
                    let (guard, _) = cv.wait_timeout(guard, Duration::from_millis(wait)).unwrap();
                    *guard
                }
            }
        };
        let elapsed = since_ok.elapsed().as_millis() as u64;
        let published = match checkpoint(&query) {
            Ok(()) => true,
            Err(e) => {
                rbitcoin_log::warn!("store: spend checkpoint: {e}");
                false
            }
        };
        // A failed publish stays due. Park one interval so the retry cannot spin.
        if elapsed_after_checkpoint(elapsed, published) == 0 {
            since_ok = Instant::now();
        } else if !stop {
            let guard = lock.lock().unwrap();
            if !*guard {
                let _ = cv
                    .wait_timeout(guard, Duration::from_millis(SPEND_DURABLE_INTERVAL_MS))
                    .unwrap();
            }
        }
        if stop {
            break;
        }
    }
}

#[doc(hidden)]
pub fn checkpoint(query: &Query) -> Result<(), rbitcoin_store::StoreError> {
    query.store().checkpoint_observed_spend()
}
