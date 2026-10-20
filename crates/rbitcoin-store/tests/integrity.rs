use rbitcoin_primitives::{merkle_branch, merkle_root_from_branch, merkle_root_mutated};
use rbitcoin_primitives::{
    merkle_root_from_txids, schema_file_openable, Fk, Height, SCHEMA_VERSION, STORE_MAGIC,
};
use rbitcoin_store::error::StoreError;
use rbitcoin_store::header_table::block_header_hash;
use rbitcoin_store::header_table::HeaderRecord;
use rbitcoin_store::integrity::*;
use rbitcoin_store::store::Store;
use rbitcoin_store::tx_table::{InputRecord, OutputRecord, TxRecord};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("rbitcoin-integrity-{n}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn hdr(prev: Fk, parent_hash: [u8; 32], salt: u8) -> HeaderRecord {
    let mut merkle = [0u8; 32];
    merkle[0] = salt;
    hdr_merkle(prev, parent_hash, salt, merkle)
}

fn hdr_merkle(prev: Fk, parent_hash: [u8; 32], salt: u8, merkle: [u8; 32]) -> HeaderRecord {
    let version = 1i32;
    let timestamp = u32::from(salt) + 1;
    let bits = 0x207fffff;
    let nonce = u32::from(salt);
    let hash = if prev.is_null() {
        block_header_hash(version, &[0u8; 32], &merkle, timestamp, bits, nonce)
    } else {
        block_header_hash(version, &parent_hash, &merkle, timestamp, bits, nonce)
    };
    HeaderRecord {
        prev_fk: prev,
        version,
        timestamp,
        bits,
        nonce,
        merkle_root: merkle,
        hash,
        size: 0,
        weight: 0,
    }
}

fn put_coinbase(s: &Store, salt: u8) -> Fk {
    let rec = TxRecord {
        txid: [salt; 32],
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let ins = vec![InputRecord::coinbase(u32::MAX, vec![salt], vec![])];
    let outs = vec![OutputRecord::unspent(50, vec![0x51])];
    s.put_tx_full_batch_indexed(&[(rec, ins, outs)], true)
        .unwrap()[0]
}

#[test]
fn merkle_root_single_and_pair() {
    let a = [1u8; 32];
    assert_eq!(merkle_root_from_txids(&[a]), a);
    let b = [2u8; 32];
    let root = merkle_root_from_txids(&[a, b]);
    assert_eq!(root, merkle_root_from_branch(a, &[b], 0));
}

/// CVE-2012-2459: a repeated tail at any level keeps the root and is
/// flagged. The shorter list and an unpaired repeat are not.
#[test]
fn merkle_mutation_flags_a_repeated_tail_at_any_level() {
    let l: Vec<[u8; 32]> = (1..=6u8).map(|n| [n; 32]).collect();
    let (root3, mutated) = merkle_root_mutated(&l[..3]);
    assert!(!mutated);
    assert_eq!(
        merkle_root_mutated(&[l[0], l[1], l[2], l[2]]),
        (root3, true)
    );
    assert!(!merkle_root_mutated(&[l[0], l[1], l[1]]).1);

    let (root6, mutated) = merkle_root_mutated(&l);
    assert!(!mutated);
    let mut tail = l.clone();
    tail.extend_from_slice(&l[4..]);
    assert_eq!(merkle_root_mutated(&tail), (root6, true));
    assert_eq!(merkle_root_mutated(&[]), ([0u8; 32], false));
}

#[test]
fn merkle_path_folds_to_root_at_every_index() {
    let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    assert!(merkle_branch(&[a], 0).is_empty());
    assert_eq!(merkle_branch(&[a, b], 0), vec![b]);
    assert_eq!(
        merkle_branch(&[a, b, c], 0),
        vec![b, merkle_root_from_txids(&[c, c])]
    );
    assert_eq!(
        merkle_branch(&[a, b, c], 2),
        vec![c, merkle_root_from_txids(&[a, b])]
    );
    for n in 1..=9u8 {
        let leaves: Vec<[u8; 32]> = (0..n).map(|i| [i + 10; 32]).collect();
        let root = merkle_root_from_txids(&leaves);
        for (index, leaf) in leaves.iter().enumerate() {
            let branch = merkle_branch(&leaves, index);
            assert_eq!(
                merkle_root_from_branch(*leaf, &branch, index),
                root,
                "n={n} index={index}"
            );
        }
    }
}

#[test]
fn clean_header_chain_revalidate_no_shrink() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    for h in 0u32..4 {
        let rec = hdr(prev, parent_hash, h as u8);
        parent_hash = rec.hash;
        let fk = s.put_header(&rec).unwrap();
        prev = fk;
        s.confirmed.set(Height(h), fk).unwrap();
    }
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();

    rbitcoin_log::capture_logs(true);
    let r = s.revalidate_tip_window_n(6).unwrap();
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    assert!(r.is_clean(), "clean chain should not shrink: {r:?}");
    assert_eq!(s.confirmed.tip_height(), Some(Height(3)));
    let text = logs
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("store: tip revalidate 0..=3 (4 heights)"),
        "{text}"
    );
    assert!(
        text.contains("store: tip revalidate done height=3"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn poison_prev_edge_shrinks_tip() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let g = hdr(Fk::NULL, [0u8; 32], 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    let a = hdr(g_fk, g.hash, 1);
    let a_fk = s.put_header(&a).unwrap();
    s.confirmed.set(Height(1), a_fk).unwrap();
    let b = hdr(a_fk, a.hash, 2);
    let b_fk = s.put_header(&b).unwrap();
    s.confirmed.set(Height(2), b_fk).unwrap();
    // Steal tip height 2 to point at G (false conf edge).
    s.confirmed.set(Height(2), g_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    rbitcoin_store::spend_durable::SpendDurable::new(5, 5)
        .store(s.path())
        .unwrap();

    let r = s.revalidate_tip_window_n(6).unwrap();
    assert!(r.tip_shrunk, "must shrink: {r:?}");
    assert_eq!(r.first_bad_reason, Some("prev_fk != confirmed parent"));
    assert_eq!(s.confirmed.tip_height(), Some(Height(1)));
    assert_eq!(r.tip_after, Some(1));
    let m = rbitcoin_store::spend_durable::SpendDurable::load(s.path())
        .unwrap()
        .unwrap();
    assert_eq!(m.annotated_through(), 1);
    assert_eq!(
        m.durable_through(),
        1,
        "a marker above the shrunk tip is clamped"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tip_seal_clamps_unsealed_extension() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let g = hdr(Fk::NULL, [0u8; 32], 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    let seal = TipSeal::load(s.path())
        .unwrap()
        .expect("seal after tip barrier");
    assert_eq!(seal.tip_height, 0);
    assert_eq!(seal.tip_hash, g.hash);

    // Extend tip in RAM + flush confirmed only *without* updating seal:
    // simulate by writing a higher tip then restoring old seal file.
    let a = hdr(g_fk, g.hash, 1);
    let a_fk = s.put_header(&a).unwrap();
    s.confirmed.set(Height(1), a_fk).unwrap();
    s.confirmed.flush().unwrap();
    // Overwrite seal with the old tip-0 seal (as if barrier never finished).
    seal.store(s.path()).unwrap();
    assert_eq!(s.confirmed.tip_height(), Some(Height(1)));

    let r = s.revalidate_tip_window_n(6).unwrap();
    assert!(r.tip_shrunk, "seal clamp must shrink: {r:?}");
    assert_eq!(s.confirmed.tip_height(), Some(Height(0)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merkle_mismatch_clears_body_and_shrinks() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let g = hdr(Fk::NULL, [0u8; 32], 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    let tx_fk = put_coinbase(&s, 9);
    let txid = s.txs.body_txid(tx_fk).unwrap();
    // Header merkle is salt-based, not txid → S3 fail.
    s.header_txs.put_range(g_fk, tx_fk, 1).unwrap();
    // Ensure genesis hash still consistent with its merkle field (no rewrite).
    let _ = txid;
    s.flush_class_c_tip().unwrap();
    s.header_txs.flush().unwrap();
    s.txs.flush().unwrap();

    rbitcoin_log::capture_logs(true);
    let r = s.revalidate_tip_window_n(6).unwrap();
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    assert_eq!(r.first_bad_reason, Some("merkle root mismatch"));
    assert!(r.bodies_cleared >= 1);
    assert!(r.tip_shrunk);
    assert!(s.confirmed.tip_height().is_none());
    let text = logs
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("store: tip revalidate done height=none"),
        "{text}"
    );
    assert!(text.contains("bad=0 reason=merkle root mismatch"), "{text}");
    assert!(!s.header_txs.has_body(g_fk).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tip_window_missing_strong_shrinks() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    let mut tip_tx = Fk::NULL;
    for h in 0u32..3 {
        let tx_fk = put_coinbase(&s, h as u8 + 1);
        let txid = s.txs.body_txid(tx_fk).unwrap();
        let rec = hdr_merkle(prev, parent_hash, h as u8, txid);
        parent_hash = rec.hash;
        let hfk = s.put_header(&rec).unwrap();
        s.header_txs.put_range(hfk, tx_fk, 1).unwrap();
        s.strong_tx.set_strong(tx_fk, hfk).unwrap();
        s.confirmed.set(Height(h), hfk).unwrap();
        prev = hfk;
        tip_tx = tx_fk;
    }
    s.rebuild_height_fence().unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    s.txs.flush().unwrap();
    s.header_txs.flush().unwrap();
    assert_eq!(s.confirmed.tip_height(), Some(Height(2)));
    s.strong_tx.set_unstrong(tip_tx).unwrap();

    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(
        r.first_bad_reason,
        Some("strong bits missing in tip window")
    );
    assert!(r.tip_shrunk, "must shrink: {r:?}");
    assert_eq!(s.confirmed.tip_height(), Some(Height(1)));
    assert_eq!(
        s.tx_height_get(tip_tx).unwrap(),
        None,
        "fence must drop the disconnected height"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 20 trailing null `confirmed[]` slots must become the last real tip in
/// one revalidate (the 6-height window alone only walks the tail).
#[test]
fn trailing_null_confirmed_slots_trimmed_in_one_open() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    for h in 0u32..4 {
        let rec = hdr(prev, parent_hash, h as u8);
        parent_hash = rec.hash;
        let fk = s.put_header(&rec).unwrap();
        prev = fk;
        s.confirmed.set(Height(h), fk).unwrap();
    }
    s.headers.flush().unwrap();
    s.confirmed.flush().unwrap();
    // No tip seal — seal clamp must not mask the suffix-null case.
    drop(s);

    let path = dir.join("confirmed.body");
    let f = rbitcoin_store::file::TableFile::open(&path, rbitcoin_primitives::TableKind::Confirmed)
        .unwrap();
    let n = (f.logical_len() - rbitcoin_store::file::FILE_HEADER_LEN as u64) / 8;
    assert_eq!(n, 4, "four real confirmed heights");
    let extra = 20u64;
    let zeros = vec![0u8; (extra * 8) as usize];
    f.write_at(rbitcoin_store::file::FILE_HEADER_LEN as u64 + n * 8, &zeros)
        .unwrap();
    f.flush().unwrap();
    drop(f);

    let s = Store::open_tiny(&dir).unwrap();
    assert_eq!(
        s.confirmed.tip_height(),
        Some(Height(23)),
        "HWM includes 20 trailing nulls"
    );
    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(
        s.confirmed.tip_height(),
        Some(Height(3)),
        "one open must drop the whole null suffix; report={r:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revalidate_n_zero_and_empty_store() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let r = s.revalidate_tip_window_n(6).unwrap();
    assert!(!r.tip_shrunk);
    assert_eq!(r.tip_after, None);
    assert!(s.confirmed.tip_height().is_none());

    let rec = hdr(Fk::NULL, [0u8; 32], 0);
    let fk = s.put_header(&rec).unwrap();
    s.confirmed.set(Height(0), fk).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    let r = s.revalidate_tip_window_n(0).unwrap();
    assert!(!r.tip_shrunk);
    assert_eq!(r.tip_after, Some(0));
    assert_eq!(s.confirmed.tip_height(), Some(Height(0)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn genesis_nonzero_prev_fk_and_header_txs_oob_shrink() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let dummy = hdr(Fk::NULL, [0u8; 32], 9);
    let dummy_fk = s.put_header(&dummy).unwrap();
    let g = hdr(dummy_fk, dummy.hash, 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(r.first_bad_reason, Some("genesis prev_fk non-null"));
    assert!(r.tip_shrunk);
    let _ = std::fs::remove_dir_all(&dir);

    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let g = hdr(Fk::NULL, [0u8; 32], 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    s.header_txs.put_range(g_fk, Fk(1_000_000), 1).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    s.header_txs.flush().unwrap();
    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(r.first_bad_reason, Some("header_txs range OOB"));
    assert!(r.bodies_cleared >= 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tip_seal_hash_mismatch_drops_tip_block() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let g = hdr(Fk::NULL, [0u8; 32], 0);
    let g_fk = s.put_header(&g).unwrap();
    s.confirmed.set(Height(0), g_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    let mut seal = TipSeal::load(s.path())
        .unwrap()
        .expect("seal after tip barrier");
    seal.tip_hash = [0xff; 32];
    seal.store(s.path()).unwrap();
    let r = s.revalidate_tip_window_n(6).unwrap();
    assert!(r.tip_shrunk, "seal hash mismatch must shrink: {r:?}");
    assert!(s.confirmed.tip_height().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Core `-checkblocks=0` / `-1` means the whole chain, not “skip checks”.
#[test]
fn checkblocks_zero_walks_below_default_window() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    let mut genesis_fk = Fk::NULL;
    for h in 0u32..10 {
        let rec = hdr(prev, parent_hash, h as u8);
        parent_hash = rec.hash;
        let fk = s.put_header(&rec).unwrap();
        if h == 0 {
            genesis_fk = fk;
        }
        prev = fk;
        s.confirmed.set(Height(h), fk).unwrap();
    }
    s.confirmed.set(Height(1), genesis_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    // A marker at the tip keeps the default window short. Without one,
    // `n == 6` is the whole chain.
    rbitcoin_store::spend_durable::SpendDurable::new(9, 9)
        .store(s.path())
        .unwrap();

    let r6 = s.revalidate_tip_window_n(6).unwrap();
    assert!(
        r6.is_clean(),
        "a tip marker keeps the default window off height-1 poison: {r6:?}"
    );
    assert_eq!(s.confirmed.tip_height(), Some(Height(9)));

    let r0 = s.revalidate_tip_window_n(0).unwrap();
    assert!(r0.tip_shrunk, "n=0 must walk from genesis: {r0:?}");
    assert_eq!(r0.first_bad_reason, Some("prev_fk != confirmed parent"));
    assert_eq!(r0.first_bad_height, Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_through_widens_past_the_six_block_window() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    let mut genesis_fk = Fk::NULL;
    for h in 0u32..10 {
        let rec = hdr(prev, parent_hash, h as u8);
        parent_hash = rec.hash;
        let fk = s.put_header(&rec).unwrap();
        if h == 0 {
            genesis_fk = fk;
        }
        prev = fk;
        s.confirmed.set(Height(h), fk).unwrap();
    }
    s.confirmed.set(Height(1), genesis_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    rbitcoin_store::spend_durable::SpendDurable::new(0, 0)
        .store(s.path())
        .unwrap();

    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(r.first_bad_height, Some(1), "{r:?}");
    assert!(r.tip_shrunk, "marker at 0 must include height 1: {r:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// No `spend_durable` means nothing has been device-flushed. A fault older
/// than the last 6 heights is still in the window.
#[test]
fn missing_marker_revalidates_from_genesis() {
    let dir = tmp();
    let s = Store::create_tiny(&dir).unwrap();
    let mut parent_hash = [0u8; 32];
    let mut prev = Fk::NULL;
    let mut genesis_fk = Fk::NULL;
    for h in 0u32..10 {
        let rec = hdr(prev, parent_hash, h as u8);
        parent_hash = rec.hash;
        let fk = s.put_header(&rec).unwrap();
        if h == 0 {
            genesis_fk = fk;
        }
        prev = fk;
        s.confirmed.set(Height(h), fk).unwrap();
    }
    s.confirmed.set(Height(1), genesis_fk).unwrap();
    s.flush_class_c_tip().unwrap();
    s.headers.flush().unwrap();
    assert!(rbitcoin_store::spend_durable::SpendDurable::load(s.path())
        .unwrap()
        .is_none());

    let r = s.revalidate_tip_window_n(6).unwrap();
    assert_eq!(r.first_bad_height, Some(1), "{r:?}");
    assert!(
        r.tip_shrunk,
        "a missing marker must walk from genesis, not the last 6: {r:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
