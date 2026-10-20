use rbitcoin_net::tip_accept::*;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::thread;

#[test]
fn lane_runs_on_named_thread() {
    let name = run_on_tip_accept(|| thread::current().name().map(str::to_string));
    assert_eq!(name.as_deref(), Some(TIP_ACCEPT_THREAD_NAME));
}

#[test]
fn wait_idle_blocks_until_job_finishes() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        run_on_tip_accept(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
    });
    started_rx.recv().unwrap();
    let waiter = thread::spawn(wait_idle);
    thread::sleep(Duration::from_millis(20));
    assert!(
        !waiter.is_finished(),
        "wait_idle must not return while a tip-accept job is running"
    );
    release_tx.send(()).unwrap();
    waiter.join().expect("wait_idle");
    worker.join().expect("job");
    wait_idle();
}

#[test]
fn wait_current_job_returns_while_second_job_queued() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (started1_tx, started1_rx) = mpsc::sync_channel(1);
    let (release1_tx, release1_rx) = mpsc::sync_channel(1);
    let (started2_tx, started2_rx) = mpsc::sync_channel(1);
    let (release2_tx, release2_rx) = mpsc::sync_channel(1);
    let first = thread::spawn(move || {
        run_on_tip_accept(move || {
            started1_tx.send(()).unwrap();
            release1_rx.recv().unwrap();
        });
    });
    started1_rx.recv().unwrap();
    let second = thread::spawn(move || {
        run_on_tip_accept(move || {
            started2_tx.send(()).unwrap();
            release2_rx.recv().unwrap();
        });
    });
    thread::sleep(Duration::from_millis(20));
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let waiter = thread::spawn(move || {
        wait_current_job();
        done_tx.send(()).ok();
    });
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        done_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "wait_current_job must not return while the snapshotted job is running"
    );
    release1_tx.send(()).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("wait_current_job must return when job 1 ends");
    waiter.join().expect("wait_current_job thread");
    assert!(
        started2_rx.try_recv().is_err() || !second.is_finished(),
        "second job must still be queued or running after wait_current_job returns"
    );
    release2_tx.send(()).unwrap();
    first.join().expect("job1");
    second.join().expect("job2");
}

#[test]
fn wait_tip_idle_from_env_only_one_and_true() {
    assert!(!wait_tip_idle_from_env(None));
    assert!(!wait_tip_idle_from_env(Some("")));
    assert!(!wait_tip_idle_from_env(Some("0")));
    assert!(!wait_tip_idle_from_env(Some("false")));
    assert!(wait_tip_idle_from_env(Some("1")));
    assert!(wait_tip_idle_from_env(Some("true")));
    assert!(wait_tip_idle_from_env(Some("TRUE")));
}

#[tokio::test]
async fn lane_sync_from_current_thread_runtime() {
    assert_eq!(run_on_tip_accept(|| 7), 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lane_async_is_not_tokio_worker() {
    let task = tokio::spawn(async {
        let caller = thread::current().name().map(str::to_string);
        let name = run_on_tip_accept_async(|| thread::current().name().map(str::to_string)).await;
        (caller, name)
    });
    let (caller, name) = task.await.expect("join worker task");
    assert!(
        caller
            .as_deref()
            .is_some_and(|n| n.starts_with("tokio-rt-worker")),
        "spawned task must run on a tokio worker, got {caller:?}"
    );
    assert_eq!(name.as_deref(), Some(TIP_ACCEPT_THREAD_NAME));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn drop_join_does_not_park_worker() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new((std::sync::Mutex::new(false), Condvar::new()));
    // A failed assert must still wake the job. The tip-accept thread is
    // process-wide, and leaving it parked hangs every later test.
    let _wake = ReleaseOnDrop(Arc::clone(&release));
    let s2 = Arc::clone(&started);
    let r2 = Arc::clone(&release);
    let waiter = tokio::spawn(async move {
        run_on_tip_accept_async(move || {
            s2.store(true, Ordering::Release);
            let (lock, cv) = &*r2;
            let mut g = lock.lock().unwrap();
            while !*g {
                g = cv.wait(g).unwrap();
            }
            1u8
        })
        .await
    });
    // The lane is shared by every test in this binary. A journey that
    // mines 100+ blocks is one job and can hold it for seconds under load.
    let t0 = Instant::now();
    while !started.load(Ordering::Acquire) {
        assert!(t0.elapsed() < Duration::from_secs(60), "job never started");
        tokio::task::yield_now().await;
    }
    waiter.abort();
    let progressed = tokio::time::timeout(Duration::from_millis(200), async {
        tokio::task::yield_now().await;
        1u8
    })
    .await;
    assert_eq!(
        progressed,
        Ok(1),
        "JoinOnDrop must not park the tokio worker"
    );
    drop(_wake);
}

struct ReleaseOnDrop(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap() = true;
        cv.notify_one();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_job_finishes_after_waiter_abort() {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let saw = Arc::new(AtomicU8::new(0));
    let saw_job = Arc::clone(&saw);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let waiter = tokio::spawn(async move {
        run_on_tip_accept_async(move || {
            saw_job.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
            saw_job.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });
    started_rx.recv().expect("job started");
    waiter.abort();
    release_tx.send(()).unwrap();
    let t0 = Instant::now();
    while saw.load(Ordering::SeqCst) < 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "aborted waiter left the tip-accept job unfinished"
        );
        tokio::task::yield_now().await;
    }
    wait_idle();
}
