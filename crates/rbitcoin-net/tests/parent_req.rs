use rbitcoin_net::tx_relay::parent_req::*;
use rbitcoin_net::tx_relay::{GETDATA_TX_INTERVAL_SECS, MAX_PARENTS_PER_PARK};
use std::collections::{BTreeMap, HashMap, HashSet};

#[test]
fn txid_parent_already_have_does_not_follow_another_peers_wtxid() {
    let mut t = ParentTracker::new();
    let hash = [0xcd; 32];
    t.schedule(hash, 2, true, 1_000);
    assert_eq!(
        t.note_inv(1, hash, false, 1_000, true),
        ParentNote::RequestNow
    );
    assert_eq!(
        t.note_inv(3, hash, false, 1_000, true),
        ParentNote::Deferred
    );
    let now = 1_000 + GETDATA_TX_INTERVAL_SECS;
    let mut saw_txid = false;
    let dropped = t.take_due(2, now, |h, wtxid| {
        assert_eq!(*h, hash);
        assert!(!wtxid, "txid parent is checked as a txid");
        saw_txid = true;
        true
    });
    assert!(saw_txid);
    assert!(
        dropped.is_empty(),
        "a txid parent already in the mempool is not requested"
    );
    let follow = t.take_due(3, now, |_, wtxid| {
        assert!(wtxid, "wtxid follow-up is not checked as a txid");
        false
    });
    assert_eq!(follow.len(), 1);
    assert!(follow[0].wtxid);
}

#[test]
fn inflight_wtxid_of_the_same_hash_is_not_requested_again_as_txid() {
    let mut t = ParentTracker::new();
    let inflight = [0x11; 32];
    let missing = [0x22; 32];
    assert_eq!(
        t.note_inv(1, inflight, true, 1_000, true),
        ParentNote::RequestNow
    );
    t.schedule(inflight, 2, false, 1_000);
    t.schedule(missing, 2, false, 1_000);
    let due = t.take_due(2, 1_000, |_, _| false);
    assert_eq!(
        due.len(),
        1,
        "the in-flight hash must not join this getdata"
    );
    assert_eq!(due[0].hash, missing);
    assert!(!due[0].wtxid, "the other parent stays a txid request");
}
