use rbitcoin_primitives::Fk;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::head_resolve_denserels::*;
use rbitcoin_store::height_fence::HeightFence;
use rbitcoin_store::io_backend::{self, ReadIoBackend};
use rbitcoin_store::segmented_head::HeadProbeWave;
use rbitcoin_store::tx_table::TxTable;
use rbitcoin_store::tx_table::{InputRecord, OutputRecord, TxRecord};
use rbitcoin_store::txid_body::TxidBody;
use rbitcoin_store::uring_session::UringSession;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

fn tmp(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("rbitcoin-head-res-{name}-{id}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn seed_table(n: u8) -> (PathBuf, TxTable, Vec<[u8; 32]>) {
    let dir = tmp("seed");
    let t = TxTable::create_tiny(&dir).unwrap();
    let mut items = Vec::new();
    let mut txids = Vec::new();
    for i in 0..n {
        let mut tid = [0u8; 32];
        tid[0] = i;
        tid[1] = 0xa5;
        tid[2] = 0x5a;
        txids.push(tid);
        let tx = TxRecord {
            txid: tid,
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: 1,
        };
        let script: Vec<u8> = (0..((i as usize % 17) + 1)).map(|b| b as u8).collect();
        items.push((
            tx,
            vec![InputRecord::coinbase(u32::MAX, vec![], vec![])],
            vec![OutputRecord::unspent(1000 + i as u64, script)],
        ));
    }
    let _fks = t.put_full_batch_indexed(&items, true).unwrap();
    (dir, t, txids)
}

/// `n` creates (1-based fks). 4 B idx slots + 16 B file header → ~1020
/// slots on page 0, so `n ≥ 1100` spans two OS pages.
fn seed_table_n(n: u32) -> (PathBuf, TxTable, Vec<[u8; 32]>) {
    let dir = tmp("seed-n");
    let t = TxTable::create_tiny(&dir).unwrap();
    let mut items = Vec::new();
    let mut txids = Vec::new();
    for i in 0..n {
        let mut tid = [0u8; 32];
        tid[0] = (i & 0xff) as u8;
        tid[1] = ((i >> 8) & 0xff) as u8;
        tid[2] = 0xa5;
        tid[3] = 0x5a;
        txids.push(tid);
        let tx = TxRecord {
            txid: tid,
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: 1,
        };
        let script: Vec<u8> = (0..((i as usize % 17) + 1)).map(|b| b as u8).collect();
        items.push((
            tx,
            vec![InputRecord::coinbase(u32::MAX, vec![], vec![])],
            vec![OutputRecord::unspent(1000 + i as u64, script)],
        ));
    }
    let _fks = t.put_full_batch_indexed(&items, true).unwrap();
    (dir, t, txids)
}

#[test]
fn resolve_uring_no_swallow_corrupt() {
    let err = StoreError::Corrupt("invariant: io_uring unexpected cqe");
    let mut pread_hits = 0u32;
    match map_uring_resolve(Err(err), || {
        pread_hits += 1;
        Ok(Vec::<rbitcoin_store::tx_table::TxidFkRange>::new())
    }) {
        Err(StoreError::Corrupt("invariant: io_uring unexpected cqe")) => {}
        other => panic!("Corrupt must propagate, got {other:?}"),
    }
    assert_eq!(pread_hits, 0);
}

#[test]
fn resolve_uring_unavailable_falls_back_to_pread() {
    let mut pread_hits = 0u32;
    let out = map_uring_resolve(Err(StoreError::Unavailable), || {
        pread_hits += 1;
        Ok(vec![(
            [0u8; 32],
            None::<(Fk, rbitcoin_store::create_loc::CreateLocPair)>,
        )])
    })
    .unwrap();
    assert_eq!(pread_hits, 1);
    assert_eq!(out.len(), 1);
}

/// Uring machine returns same (fk, body_range) as sequential pread path.
#[test]
fn uring_fk_and_range_matches_pread() {
    let (dir, t, txids) = seed_table(40);
    let pread = resolve_fk_and_range_pread(&t, &txids, None, false).unwrap();
    // Public entry (uring when available, else pread) must match pure pread.
    let via = resolve_fk_and_range_batch(&t, &txids).unwrap();
    assert_eq!(pread.len(), via.len());
    for (a, b) in pread.iter().zip(via.iter()) {
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1, "txid[0]={}", a.0[0]);
    }
    // Every hit has a non-empty body_range matching record_range.
    for (_tid, row) in &pread {
        if let Some((fk, range)) = row {
            assert_eq!(t.body_range(*fk).unwrap(), range.txout);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// After drain, TipOnly hits durable head (write-behind is load-owned).
#[test]
fn uring_pending_write_behind_does_not_nest_tls() {
    let dir = tmp("pending-uring");
    let t = TxTable::create_tiny(&dir).unwrap();
    let mut tid = [0u8; 32];
    tid[0] = 0x51;
    let tx = TxRecord {
        txid: tid,
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let fks = t
        .put_full_batch_indexed(
            &[(
                tx,
                vec![InputRecord::coinbase(u32::MAX, vec![], vec![])],
                vec![OutputRecord::unspent(50, vec![0x51])],
            )],
            /*index=*/ false,
        )
        .unwrap();
    t.head_note_pending(&[(tid, fks[0])]);
    t.head_drain_pending().unwrap();
    let via = resolve_fk_and_range_batch(&t, &[tid]).unwrap();
    assert_eq!(via.len(), 1);
    let (got_tid, row) = &via[0];
    assert_eq!(*got_tid, tid);
    let (fk, range) = row.expect("drained head must stamp fk+range");
    assert_eq!(fk, fks[0]);
    assert_eq!(t.body_range(fk).unwrap(), range.txout);
    assert!(range.txout.1 > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Single-segment store: every winner is sealed_age 0 (open/tip).
///
/// Multi-age mapping is covered by `head_resolve_stats::sealed_age_for_fk_*`.
/// Global AGE_HIT atomics race parallel tests, so we pin mapping on winners
/// via `first_fks` and only require the process counters moved for age 0.
#[test]
fn resolve_records_winner_age_open_segment() {
    let _ = rbitcoin_store::head_resolve_stats::sample_and_reset();
    let (dir, t, txids) = seed_table(16);
    assert_eq!(
        t.head.segment_count(),
        1,
        "unexpected segs={}",
        t.head.segment_count()
    );
    let first = t.head.first_fks_snapshot();
    assert_eq!(first, vec![1]);
    let got = resolve_fk_and_range_batch(&t, &txids).unwrap();
    let hits = got.iter().filter(|(_, r)| r.is_some()).count() as u64;
    assert_eq!(hits, txids.len() as u64);
    for (_tid, row) in &got {
        if let Some((fk, _)) = row {
            assert_eq!(
                rbitcoin_store::head_resolve_stats::sealed_age_for_fk(&first, fk.0),
                Some(0),
                "fk={}",
                fk.0
            );
        }
    }
    let s = rbitcoin_store::head_resolve_stats::sample_and_reset();
    // Our hits are age 0; concurrent resolve tests may add more age-0 counts.
    assert!(
        s.age_hit[0] >= hits,
        "age0={} hits={hits} age_hit={:?}",
        s.age_hit[0],
        &s.age_hit[..8]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Older fence-connected create wins over a newer unconnected duplicate,
/// including when they sit in different sealed segments. TipOnly must not
/// return None after seeing only the newer body.
#[test]
fn tip_fence_keeps_older_connected_across_sealed_segments() {
    use rbitcoin_store::address_head::HeadLayout;
    use rbitcoin_store::height_fence::FenceRun;
    let dir = tmp("fence-across-segs");
    let layout = HeadLayout::with_entry_bytes(8, 4).unwrap();
    let t = TxTable::create_with_head_layout(&dir, layout).unwrap();
    let n = 204u32.saturating_mul(6);
    let mut items = Vec::new();
    let mut txids = Vec::new();
    for i in 0..n {
        let mut tid = [0u8; 32];
        tid[0..4].copy_from_slice(&i.to_le_bytes());
        tid[8] = 0xa5;
        txids.push(tid);
        items.push((
            TxRecord {
                txid: tid,
                version: 1,
                locktime: 0,
                input_start_fk: Fk::NULL,
                input_count: 1,
                output_start_fk: Fk::NULL,
                output_count: 1,
            },
            vec![InputRecord::coinbase(u32::MAX, vec![], vec![])],
            vec![OutputRecord::unspent(1, vec![0x51])],
        ));
    }
    t.put_full_batch_indexed(&items, true).unwrap();
    let dup_tid = txids[0];
    let dup_fk = t
        .put_full_batch_indexed(
            &[(
                TxRecord {
                    txid: dup_tid,
                    version: 1,
                    locktime: 0,
                    input_start_fk: Fk::NULL,
                    input_count: 1,
                    output_start_fk: Fk::NULL,
                    output_count: 1,
                },
                vec![InputRecord::coinbase(u32::MAX, vec![0x22], vec![])],
                vec![OutputRecord::unspent(1, vec![0x52])],
            )],
            true,
        )
        .unwrap()[0];
    t.flush_head().unwrap();
    let first = t.head.first_fks_snapshot();
    let oldest = rbitcoin_store::head_resolve_stats::sealed_age_for_fk(&first, 1).unwrap();
    let newest_fk = Fk(u64::from(n));
    let age_new =
        rbitcoin_store::head_resolve_stats::sealed_age_for_fk(&first, newest_fk.0).unwrap();
    assert!(
        oldest > age_new && age_new >= 1 && t.head.sealed_segment_count() >= 4,
        "oldest age={oldest} newest age={age_new} sealed={}",
        t.head.sealed_segment_count()
    );
    let age_dup = rbitcoin_store::head_resolve_stats::sealed_age_for_fk(&first, dup_fk.0).unwrap();
    assert_eq!(
        age_dup, 0,
        "duplicate must land in the open tail, fk={}",
        dup_fk.0
    );
    assert!(dup_fk.0 > newest_fk.0);
    let fence = HeightFence::from_runs(vec![
        FenceRun {
            first_fk: 1,
            count: 1,
            height: 1,
        },
        FenceRun {
            first_fk: newest_fk.0,
            count: 1,
            height: 2,
        },
    ]);
    assert_eq!(fence.height_of(Fk(1)), Some(1));
    assert_eq!(fence.height_of(newest_fk), Some(2));
    assert_eq!(fence.height_of(dup_fk), None);
    let newest_tid = txids[(n as usize) - 1];
    let tip =
        resolve_fk_and_range_batch_with_tip(&t, &fence, &[dup_tid, newest_tid], true).unwrap();
    assert_eq!(tip[0].1.map(|(f, _)| f), Some(Fk(1)));
    assert_eq!(tip[1].1.map(|(f, _)| f), Some(newest_fk));
    let any = resolve_fk_and_range_batch_with_tip(&t, &fence, &[dup_tid], false).unwrap();
    assert_eq!(any[0].1.map(|(f, _)| f), Some(Fk(1)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn miss_and_deepest_create_wins() {
    let dir = tmp("bip30");
    let t = TxTable::create_tiny(&dir).unwrap();
    let txid = [0xcd; 32];
    let mk = |hint: u8| {
        (
            TxRecord {
                txid,
                version: 1,
                locktime: 0,
                input_start_fk: Fk::NULL,
                input_count: 1,
                output_start_fk: Fk::NULL,
                output_count: 1,
            },
            vec![InputRecord::coinbase(u32::MAX, vec![hint], vec![])],
            vec![OutputRecord::unspent(1, vec![0x51])],
        )
    };
    let _fk1 = t.put_full_batch_indexed(&[mk(1)], true).unwrap()[0];
    let fk2 = t.put_full_batch_indexed(&[mk(2)], true).unwrap()[0];
    let got = resolve_fk_and_range_batch(&t, &[txid, [0xff; 32]]).unwrap();
    assert_eq!(got[0].1.map(|(f, _)| f), Some(fk2));
    assert_eq!(got[1].1, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loc_batch_matches_serial_body_range() {
    let (dir, t, txids) = seed_table_n(1100);
    let first = Fk(1);
    let near = Fk(2);
    let far = Fk(1100);
    let batch = t.create_loc.range_batch(&[first, near, far]).unwrap();
    for (fk, got) in [first, near, far].iter().zip(batch.iter()) {
        let exp = t.body_range(*fk).unwrap();
        assert_eq!(got.map(|p| p.txout), Some(exp), "fk={}", fk.0);
    }
    let got =
        resolve_fk_and_range_pread(&t, &[txids[0], txids[1], txids[1099]], None, false).unwrap();
    assert_eq!(got[0].1, Some((first, batch[0].unwrap())));
    assert_eq!(got[1].1, Some((near, batch[1].unwrap())));
    assert_eq!(got[2].1, Some((far, batch[2].unwrap())));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loc_fill_does_not_use_held_probe_session() {
    let (dir, t, txids) = seed_table(4);
    let mut sess = rbitcoin_store::uring_session::UringSession::try_open(32).unwrap_or_else(|_| {
        rbitcoin_store::uring_session::UringSession::try_open_kind(
            rbitcoin_store::uring_session::SessionKind::Pool,
            32,
        )
        .expect("pool")
    });
    sess.poison();
    let got = t
        .create_loc
        .range_batch(&[Fk(1), Fk(2)])
        .expect("standalone loc must not fail-close on a poisoned probe ring");
    assert_eq!(got[0].unwrap().txout, t.body_range(Fk(1)).unwrap());
    assert_eq!(got[1].unwrap().txout, t.body_range(Fk(2)).unwrap());
    match t
        .create_loc
        .range_batch_ctx(&[Fk(1)], &mut rbitcoin_store::IoCtx::held(&mut sess))
    {
        Err(e) => {
            let m = format!("{e}");
            assert!(m.contains("poisoned") || m.contains("io_uring"), "{m}");
        }
        other => panic!("held loc ctx must stay fail-closed, got {other:?}"),
    }
    let via = resolve_fk_and_range_batch(&t, &[txids[0]]).unwrap();
    assert!(via[0].1.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn attach_loc_missing_after_identity_is_corrupt() {
    let (dir, t, txids) = seed_table(2);
    match attach_loc_to_identity(
        &t,
        &[txids[0]],
        IdentityHits {
            picked: vec![Some(Fk(99))],
        },
    ) {
        Err(StoreError::Corrupt(m)) => {
            assert!(m.contains("loc range missing after identity"), "{m}");
        }
        other => panic!("expected loc-range Corrupt, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
