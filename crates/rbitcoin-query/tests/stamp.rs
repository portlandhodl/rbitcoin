use rbitcoin_primitives::Fk;
use rbitcoin_query::stamp::*;
use rbitcoin_query::{CreatePin, InFlight, QueryError, U64Map};
use rbitcoin_query::{IdMap, TxidHasher};
use rbitcoin_store::Store;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::time::Instant;

fn tmp_store() -> (rbitcoin_query::testutil::TempDir, rbitcoin_query::Query) {
    rbitcoin_query::testutil::tiny_query_labeled("stamp")
}

fn pin(id: u64) -> CreatePin {
    use rbitcoin_store::{OutputRecord, TxRecord};
    let mut txid = [0u8; 32];
    txid[..8].copy_from_slice(&id.to_le_bytes());
    rbitcoin_query::CreatePinInner::records(
        TxRecord {
            txid,
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: 1,
        },
        vec![OutputRecord::unspent(1, vec![0x51])],
    )
}

#[test]
fn skeleton_hit_skips_leftover_head() {
    let (dir, q) = tmp_store();
    let mut txid = [0u8; 32];
    txid[0] = 0x11;
    let mut ids = IdMap::default();
    ids.insert(txid, (Fk(7), (10, 20)));
    let mut spent = U64Map::default();
    spent.insert(7, (30, 40));
    let skel = BatchParentIds {
        ids: Arc::new(ids),
        spent: Arc::new(spent),
        n_out: Default::default(),
        need_vouts: U64Map::default(),
    };
    let empty = InFlight::new();
    let st =
        stamp_external_parents(q.store(), &[txid], &empty, Some(&skel), q.confirm_stats()).unwrap();
    assert_eq!(st.head_need_n, 0);
    assert_eq!(st.resolved.get(&txid), Some(&Fk(7)));
    assert_eq!(st.idents.get(&7).and_then(|e| e.body), Some((10, 20)));
    assert_eq!(st.idents.get(&7).and_then(|e| e.spent), Some((30, 40)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inflight_hit_without_skeleton() {
    let (dir, q) = tmp_store();
    let p = pin(42);
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(42), &p)], Some(1));
    let txid = p.tx().txid;
    let st =
        stamp_external_parents(q.store(), &[txid], &inflight, None, q.confirm_stats()).unwrap();
    assert_eq!(st.head_need_n, 0);
    assert_eq!(st.resolved.get(&txid), Some(&Fk(42)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn null_txid_is_skipped() {
    let (dir, q) = tmp_store();
    let empty = InFlight::new();
    let st =
        stamp_external_parents(q.store(), &[[0u8; 32]], &empty, None, q.confirm_stats()).unwrap();
    assert!(st.resolved.is_empty());
    assert_eq!(st.head_need_n, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// IBD skeleton miss + InFlight pin + loc on disk: load does not loc-by-fk.
/// Spent stays unset for write TLS / `CreatePin::set_loc` (mainnet 133433).
#[test]
fn inflight_hit_skeleton_miss_leaves_spent_unset_despite_disk_loc() {
    use std::sync::atomic::Ordering;
    let (dir, q) = tmp_store();
    let p = pin(1);
    let txid = p.tx().txid;
    let fks = q
        .store
        .txs
        .put_full_batch_indexed(
            &[(
                p.tx().clone(),
                vec![rbitcoin_store::InputRecord::coinbase(
                    u32::MAX,
                    vec![0x01],
                    vec![],
                )],
                (0..p.n_out() as u32)
                    .filter_map(|v| p.out_record(v))
                    .collect(),
            )],
            true,
        )
        .unwrap();
    assert_eq!(fks[0], Fk(1));
    assert!(q.store.txs.spent_range(Fk(1)).expect("spent range").1 > 0);
    let _ = q.confirm_stats().fill_missing_n.swap(0, Ordering::Relaxed);
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(1), &p)], Some(1));
    let skel = BatchParentIds::default();
    let st = stamp_external_parents(
        q.store(),
        &[txid],
        &inflight,
        Some(&skel),
        q.confirm_stats(),
    )
    .unwrap();
    assert_eq!(st.head_need_n, 0);
    assert_eq!(st.resolved.get(&txid), Some(&Fk(1)));
    let ident = st.idents.get(&1).expect("inflight ident");
    assert!(ident.pin.is_some(), "inflight pin is kept");
    assert_eq!(ident.spent, None, "IBD stamp must not loc-by-fk");
    assert_eq!(ident.body, None);
    assert_eq!(ident.n_out, None);
    assert_eq!(
        q.confirm_stats().fill_missing_n.load(Ordering::Relaxed),
        0,
        "IBD skeleton path must not fill_missing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inflight_hit_uses_pin_loc_without_disk() {
    let (dir, q) = tmp_store();
    let p = pin(1);
    let txid = p.tx().txid;
    let fks = q
        .store
        .txs
        .put_full_batch_indexed(
            &[(
                p.tx().clone(),
                vec![rbitcoin_store::InputRecord::coinbase(
                    u32::MAX,
                    vec![0x01],
                    vec![],
                )],
                (0..p.n_out() as u32)
                    .filter_map(|v| p.out_record(v))
                    .collect(),
            )],
            true,
        )
        .unwrap();
    assert_eq!(fks[0], Fk(1));
    let pair = q
        .store
        .txs
        .create_loc_range_batch(&[Fk(1)])
        .unwrap()
        .into_iter()
        .next()
        .flatten()
        .expect("loc after append");
    p.set_loc(pair);
    q.store.txs.create_loc_truncate_to_count(0).unwrap();
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(1), &p)], Some(1));
    let skel = BatchParentIds::default();
    let st = stamp_external_parents(
        q.store(),
        &[txid],
        &inflight,
        Some(&skel),
        q.confirm_stats(),
    )
    .expect("pin loc must bind without disk loc");
    let ident = st.idents.get(&1).expect("inflight ident");
    assert_eq!(ident.spent, Some(pair.spent));
    assert_eq!(ident.body, Some(pair.txout));
    assert_eq!(ident.n_out, Some(pair.n_out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inflight_hit_skeleton_miss_body_without_loc_count_leaves_spent_unset() {
    let (dir, q) = tmp_store();
    let p = pin(1);
    let txid = p.tx().txid;
    let fks = q
        .store
        .txs
        .put_full_batch_indexed(
            &[(
                p.tx().clone(),
                vec![rbitcoin_store::InputRecord::coinbase(
                    u32::MAX,
                    vec![0x01],
                    vec![],
                )],
                (0..p.n_out() as u32)
                    .filter_map(|v| p.out_record(v))
                    .collect(),
            )],
            true,
        )
        .unwrap();
    assert_eq!(fks[0], Fk(1));
    assert!(q.store.txs.count() >= 1);
    q.store.txs.create_loc_truncate_to_count(0).unwrap();
    assert!(q.store.tx_create_loc_count() < 1);
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(1), &p)], Some(1));
    let skel = BatchParentIds::default();
    let st = stamp_external_parents(
        q.store(),
        &[txid],
        &inflight,
        Some(&skel),
        q.confirm_stats(),
    )
    .expect("body HWM without loc count is same-wave hole, not Corrupt");
    let ident = st.idents.get(&1).expect("inflight ident");
    assert!(ident.pin.is_some());
    assert_eq!(ident.spent, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inflight_hit_skeleton_miss_without_loc_leaves_spent_unset() {
    let (dir, q) = tmp_store();
    let p = pin(42);
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(42), &p)], Some(1));
    let txid = p.tx().txid;
    let skel = BatchParentIds::default();
    let st = stamp_external_parents(
        q.store(),
        &[txid],
        &inflight,
        Some(&skel),
        q.confirm_stats(),
    )
    .unwrap();
    let ident = st.idents.get(&42).expect("inflight ident");
    assert!(ident.pin.is_some());
    assert_eq!(ident.spent, None);
    assert_eq!(ident.body, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inflight_hit_adopts_skeleton_loc() {
    let (dir, q) = tmp_store();
    let p = pin(42);
    let mut inflight = InFlight::new();
    inflight.note_pins([(Fk(42), &p)], Some(1));
    let txid = p.tx().txid;
    let mut ids = IdMap::default();
    ids.insert(txid, (Fk(42), (10, 20)));
    let mut spent = U64Map::default();
    spent.insert(42, (30, 40));
    let mut n_out = U64Map::default();
    n_out.insert(42, 1);
    let skel = BatchParentIds {
        ids: Arc::new(ids),
        spent: Arc::new(spent),
        n_out: Arc::new(n_out),
        need_vouts: U64Map::default(),
    };
    let st = stamp_external_parents(
        q.store(),
        &[txid],
        &inflight,
        Some(&skel),
        q.confirm_stats(),
    )
    .unwrap();
    assert_eq!(st.resolved.get(&txid), Some(&Fk(42)));
    let ident = st.idents.get(&42).expect("inflight ident");
    assert!(ident.pin.is_some(), "inflight pin is kept");
    assert_eq!(ident.body, Some((10, 20)));
    assert_eq!(ident.spent, Some((30, 40)));
    assert_eq!(ident.n_out, Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skeleton_miss_is_unresolved_without_head_probe() {
    let (dir, q) = tmp_store();
    let mut txid = [0u8; 32];
    txid[0] = 0x22;
    let skel = BatchParentIds::default();
    let empty = InFlight::new();
    let err = stamp_external_parents(q.store(), &[txid], &empty, Some(&skel), q.confirm_stats())
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("parent create_fk unresolved"), "got: {msg}");
    let _ = std::fs::remove_dir_all(&dir);
}
