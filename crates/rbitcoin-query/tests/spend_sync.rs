use rbitcoin_query::Query;
use rbitcoin_store::{elapsed_after_checkpoint, SPEND_DURABLE_INTERVAL_MS};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rbitcoin_primitives::{Fk, Height};

use rbitcoin_query::spend_sync::SpendSync;

#[test]
fn shutdown_publishes_the_snapshot() {
    let (dir, q) = rbitcoin_query::testutil::tiny_query_labeled("spend-sync-stop");
    q.store().confirmed.set(Height(4), Fk(1)).unwrap();
    q.store().note_spend_snapshot(4);
    let q = Arc::new(q);
    let sync = SpendSync::spawn(Arc::clone(&q));
    sync.shutdown();
    let raw = std::fs::read(q.store().path().join(rbitcoin_store::SPEND_DURABLE_NAME)).unwrap();
    assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), 4);
    assert_eq!(u32::from_le_bytes(raw[12..16].try_into().unwrap()), 4);
    let _ = dir;
}
