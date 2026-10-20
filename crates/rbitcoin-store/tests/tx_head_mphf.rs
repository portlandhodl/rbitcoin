use rbitcoin_store::bdz::BdzMphf;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::tx_head_mphf::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rbitcoin_store::uring_session::{IoCtx, SessionKind, UringSession};

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rbitcoin-tx-mphf-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn index_batch_held_session_submits_g_pages() {
    let dir = tmp("held");
    std::fs::create_dir_all(&dir).unwrap();
    let keys: Vec<u64> = (0..200u64).map(|i| i * 17 + 3).collect();
    let ram = BdzMphf::build(&keys).unwrap();
    let p = dir.join("t.mphf");
    ram.write_packed_to(&p).unwrap();
    let fd = BdzMphf::read_packed_from(&p).unwrap();
    let serial = fd.index_batch(&keys[..8], &mut IoCtx::none()).unwrap();
    let mut session = UringSession::try_open_kind(SessionKind::Pool, 32).expect("pool");
    let mut ctx = IoCtx::held(&mut session);
    let batch = fd.index_batch(&keys[..8], &mut ctx).unwrap();
    session.drain_all().unwrap();
    assert_eq!(batch, serial);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Held-session leftover `KIND_BULK_PREAD` must not be harvested as a BDZ
/// g-page CQE (`bdz g page bad slot`). Drain the foreign SQE first.
#[test]
fn index_batch_held_session_drains_foreign_kind_leftover() {
    use std::io::Write;
    let dir = tmp("held-leftover");
    std::fs::create_dir_all(&dir).unwrap();
    let keys: Vec<u64> = (0..200u64).map(|i| i * 17 + 3).collect();
    let ram = BdzMphf::build(&keys).unwrap();
    let p = dir.join("t.mphf");
    ram.write_packed_to(&p).unwrap();
    let fd_mphf = BdzMphf::read_packed_from(&p).unwrap();
    let serial = fd_mphf.index_batch(&keys[..8], &mut IoCtx::none()).unwrap();

    let leftover_path = dir.join("leftover.bin");
    let mut leftover_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&leftover_path)
        .unwrap();
    leftover_file.write_all(&[0xAAu8; 8]).unwrap();
    leftover_file.sync_all().unwrap();
    let leftover_fd = rbitcoin_store::io_handle::IoHandle::from_file(&leftover_file);

    let mut session = UringSession::try_open_kind(SessionKind::Pool, 32).expect("pool");
    session.begin_batch().unwrap();
    let mut leftover_buf = [0u8; 8];
    let ud = rbitcoin_store::uring_session::pack_ud(
        rbitcoin_store::uring_session::KIND_BULK_PREAD,
        session.epoch(),
        0,
    );
    // SAFETY: `leftover_buf` lives until this session drains.
    unsafe { session.push_pread(leftover_fd, 0, &mut leftover_buf, ud) }.unwrap();
    session.submit().unwrap();
    assert!(
        session.in_flight() > 0,
        "foreign SQE must still be pending when BDZ starts"
    );

    let batch = {
        let mut ctx = IoCtx::held(&mut session);
        fd_mphf
            .index_batch(&keys[..8], &mut ctx)
            .unwrap_or_else(|e| {
                panic!("held index_batch with leftover KIND_BULK_PREAD must drain, not {e}")
            })
    };
    session.drain_all().unwrap();
    assert_eq!(batch, serial);
    assert_eq!(session.in_flight(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tx_head_mphf_open_is_header_only() {
    let dir = tmp("open");
    std::fs::create_dir_all(&dir).unwrap();
    let base = dir.join("000000");
    let pairs: Vec<(u64, u32)> = (1..64u32)
        .map(|i| (u64::from(i).wrapping_mul(0x9e37_79b9_7f4a_7c15), i))
        .collect();
    let h = TxHeadMphf::write(&base, &pairs).unwrap();
    assert_eq!(h.mphf.g_bytes_resident(), 0);
    assert!(!rel_path(&base).is_file());
    assert_eq!(&std::fs::read(mphf_path(&base)).unwrap()[0..4], b"BDZ2");
    let slots = h.slots_for(&[pairs[0].0]).unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0], pairs[0].1 - 1);
    let rels = h
        .read_rels_batch(&slots, &mut rbitcoin_store::IoCtx::none())
        .unwrap();
    assert_eq!(rels[0][0], pairs[0].1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn group_assigned_pairs_unique_and_bip30_newest() {
    assert!(group_assigned_pairs(&mut []).unwrap().keys.is_empty());
    assert!(matches!(
        group_assigned_pairs(&mut [(1, 0)]),
        Err(StoreError::Corrupt(_))
    ));
    let mut pairs = vec![(10u64, 2u32), (20, 1), (10, 5), (10, 3), (30, 4)];
    let g = group_assigned_pairs(&mut pairs).unwrap();
    assert_eq!(g.modulus, 5);
    let mut got: Vec<(u64, u32)> = g
        .keys
        .iter()
        .copied()
        .zip(g.values.iter().copied())
        .collect();
    got.sort_unstable();
    assert_eq!(got, vec![(10, 4), (20, 0), (30, 3)]);
    assert_eq!(g.mlt.get(&4), Some(&vec![3, 2]));
    assert_eq!(g.mlt.len(), 1);
}

#[test]
fn tx_head_mphf_unlinks_leftover_rel_and_bip30_mlt() {
    let dir = tmp("leftover-rel");
    std::fs::create_dir_all(&dir).unwrap();
    let base = dir.join("000000");
    let key = 0xB1B0_u64;
    let pairs = vec![(key, 1u32), (0x2222, 2), (key, 3)];
    std::fs::write(rel_path(&base), [1u8, 2, 3, 4]).unwrap();
    let h = TxHeadMphf::write(&base, &pairs).unwrap();
    assert!(!rel_path(&base).is_file());
    let slots = h.slots_for(&[key]).unwrap();
    assert_eq!(slots, vec![2]);
    let rels = h
        .read_rels_batch(&slots, &mut rbitcoin_store::IoCtx::none())
        .unwrap();
    assert_eq!(rels[0], vec![3, 1]);
    std::fs::write(rel_path(&base), [9u8; 4]).unwrap();
    let h2 = TxHeadMphf::open(&base).unwrap();
    assert!(!rel_path(&base).is_file());
    assert_eq!(
        h2.read_rels_batch(&slots, &mut rbitcoin_store::IoCtx::none())
            .unwrap()[0],
        vec![3, 1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
