use rbitcoin_store::block_queue::*;
use rbitcoin_store::error::StoreError;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn temp() -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("rbitcoin-bq-ram-{id}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn second_enqueue_at_height_keeps_the_first_body() {
    let mut q = BlockQueue::empty();
    let first = q.enqueue(1, [1u8; 32], 1, b"aaaa").unwrap();
    let again = q.enqueue(1, [2u8; 32], 9, b"bbbbbbbb").unwrap();
    assert_eq!(again, first);
    assert_eq!(q.count(), 1);
    assert_eq!(q.bytes(), 4);
    assert_eq!(q.hash_at_height(1), Some([1u8; 32]));
    assert!(q.has_hash(&[1u8; 32]));
    assert!(!q.has_hash(&[2u8; 32]));
    assert_eq!(q.dequeue_height(1).unwrap(), 1);
    assert!(!q.has_hash(&[1u8; 32]));
    assert_eq!(q.count(), 0);
}

#[test]
fn get_by_height_peek_without_dequeue() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let payload = b"wire-bytes-height-42".to_vec();
    q.enqueue(42, [0x11u8; 32], 3, &payload).unwrap();
    let got = q.get_by_height(42).unwrap().expect("by height");
    assert_eq!(got.payload, payload);
    assert_eq!(got.height, 42);
    assert!(q.contains_height(42));
    assert!(q.get_by_height(99).unwrap().is_none());
    assert_eq!(q.count(), 1, "peek must not dequeue");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enqueue_dequeue_no_disk_rehydrate() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let payload = b"fake-block-bytes-for-queue".to_vec();
    let id = q.enqueue(100, [0xABu8; 32], 7, &payload).unwrap();
    assert_eq!(q.count(), 1);
    let got = q.get(id).unwrap().expect("present");
    assert_eq!(got.height, 100);
    assert_eq!(got.hash, [0xABu8; 32]);
    assert_eq!(got.payload, payload);
    assert!(q.dequeue(id).unwrap());
    assert_eq!(q.count(), 0);
    // Restart: RAM queue is empty (by design — no durable rehydrate).
    drop(q);
    let q2 = BlockQueue::open_or_create(&dir).unwrap();
    assert_eq!(q2.count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enqueue_accepts_large_payload_and_drops_legacy_dir() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let big = vec![0u8; 65 * 1024 * 1024];
    q.enqueue(1, [9u8; 32], 1, &big)
        .expect("enqueue has no byte ceiling");
    assert_eq!(q.count(), 1);
    let legacy = dir.join("block_queue");
    std::fs::create_dir_all(&legacy).unwrap();
    let _ = BlockQueue::open_or_create(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn max_height_span() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    assert_eq!(q.max_height(), None);
    q.enqueue(10, [1u8; 32], 1, b"a").unwrap();
    q.enqueue(15, [2u8; 32], 2, b"b").unwrap();
    q.enqueue(12, [3u8; 32], 3, b"c").unwrap();
    assert_eq!(q.max_height(), Some(15));
    assert_eq!(q.min_height(), Some(10));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fifo_oldest_and_heights() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let id1 = q.enqueue(1, [1u8; 32], 1, b"a").unwrap();
    let id2 = q.enqueue(2, [2u8; 32], 2, b"bb").unwrap();
    assert_eq!(q.peek_oldest_id(), Some(id1));
    let mut hs = q.heights();
    hs.sort_unstable();
    assert_eq!(hs, vec![1, 2]);
    assert_eq!(q.id_for_height(2), Some(id2));
    assert_eq!(q.dequeue_height(1).unwrap(), 1);
    assert_eq!(q.peek_oldest_id(), Some(id2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_meta_matches_payload_len() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(5, [9u8; 32], 1, b"hello").unwrap();
    let m = q.list_meta();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].payload_len, 5);
    assert_eq!(m[0].height, 5);
    assert_eq!(m[0].n_inputs, 0, "junk payload is not a block");
    assert!(!m[0].resolve_complete, "enqueue starts unresolved");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enqueue_stamps_wire_input_count() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let mut payload = vec![0u8; 80];
    rbitcoin_primitives::write_compact_size(&mut payload, 1);
    payload.extend_from_slice(&1u32.to_le_bytes());
    rbitcoin_primitives::write_compact_size(&mut payload, 1);
    payload.extend_from_slice(&[0u8; 32]);
    payload.extend_from_slice(&u32::MAX.to_le_bytes());
    rbitcoin_primitives::write_compact_size(&mut payload, 0);
    payload.extend_from_slice(&u32::MAX.to_le_bytes());
    rbitcoin_primitives::write_compact_size(&mut payload, 1);
    payload.extend_from_slice(&0u64.to_le_bytes());
    rbitcoin_primitives::write_compact_size(&mut payload, 0);
    payload.extend_from_slice(&0u32.to_le_bytes());
    q.enqueue(1, [1u8; 32], 1, &payload).unwrap();
    assert_eq!(q.input_count_at(1), Some(1));
    assert_eq!(q.list_meta()[0].n_inputs, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_meta_reports_resolve_complete() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(3, [3u8; 32], 1, b"a").unwrap();
    q.enqueue(4, [4u8; 32], 2, b"b").unwrap();
    q.mark_resolve_complete(4).unwrap();
    let m = q.list_meta();
    assert_eq!(m.len(), 2);
    let h3 = m.iter().find(|e| e.height == 3).unwrap();
    let h4 = m.iter().find(|e| e.height == 4).unwrap();
    assert!(!h3.resolve_complete);
    assert!(h4.resolve_complete);
    assert!(q.is_resolve_complete(4));
    assert!(!q.is_resolve_complete(3));
    let id4 = q.id_for_height(4).unwrap();
    assert!(q.dequeue(id4).unwrap());
    let m = q.list_meta();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].height, 3);
    assert!(!m[0].resolve_complete);
    assert!(!q.is_resolve_complete(4));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unresolved_heights_skips_complete_and_respects_cap() {
    use std::collections::HashSet;
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    for h in 0..24u32 {
        q.enqueue(h, [h as u8; 32], 1, b"x").unwrap();
    }
    for h in 0..16u32 {
        q.mark_resolve_complete(h).unwrap();
    }
    let skip: HashSet<u32> = [16, 17].into_iter().collect();
    let got = q.unresolved_heights(10, &skip, 8);
    assert_eq!(got, vec![18, 19, 20, 21, 22, 23]);
    assert_eq!(q.unresolved_heights(10, &skip, 3), vec![18, 19, 20]);
    assert!(q.unresolved_heights(10, &skip, 0).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unresolved_heights_stops_at_first_gap() {
    use std::collections::HashSet;
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(12, [12u8; 32], 1, b"a").unwrap();
    q.enqueue(13, [13u8; 32], 1, b"b").unwrap();
    let none = HashSet::new();
    assert!(
        q.unresolved_heights(10, &none, 8).is_empty(),
        "missing path_lo must not skip ahead: {:?}",
        q.unresolved_heights(10, &none, 8)
    );
    q.enqueue(10, [10u8; 32], 1, b"c").unwrap();
    q.enqueue(11, [11u8; 32], 1, b"d").unwrap();
    assert_eq!(q.unresolved_heights(10, &none, 8), vec![10, 11, 12, 13]);
    // Hole at 12 after removing it — prefix only.
    q.dequeue_height(12).unwrap();
    assert_eq!(q.unresolved_heights(10, &none, 8), vec![10, 11]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enqueue_never_refuses_on_bytes() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let big = vec![0u8; 32 * 1024 * 1024];
    q.enqueue(1, [1u8; 32], 1, &big).unwrap();
    q.enqueue(2, [2u8; 32], 2, &big).unwrap();
    q.enqueue(3, [3u8; 32], 3, &big)
        .expect("BQ must accept already-requested bodies; densify assign-stop is the cap");
    assert_eq!(q.count(), 3);
    assert!(q.bytes() > 64 * 1024 * 1024);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ids_and_get_by_height() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    assert!(q.ids().is_empty());
    assert!(!q.contains_height(1));
    let id = q.enqueue(7, [7u8; 32], 1, b"xyz").unwrap();
    let mut ids = q.ids();
    ids.sort_unstable();
    assert_eq!(ids, vec![id]);
    assert!(q.contains_height(7));
    assert!(!q.contains_height(8));
    assert!(q.get_by_height(99).unwrap().is_none());
    let got = q.get_by_height(7).unwrap().unwrap();
    assert_eq!(got.payload, b"xyz");
    assert_eq!(q.get(id).unwrap().unwrap().payload, b"xyz");
    assert!(q.get(999).unwrap().is_none());
    assert_eq!(q.min_height(), Some(7));
    assert_eq!(q.max_height(), Some(7));
    assert!(q.dequeue(id).unwrap());
    assert!(!q.contains_height(7));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_complete_clears_with_dequeue() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    let id = q.enqueue(5, [5u8; 32], 1, b"blk").unwrap();
    q.mark_resolve_complete(5).unwrap();
    assert!(q.is_resolve_complete(5));
    assert!(q.dequeue(id).unwrap());
    assert!(!q.is_resolve_complete(5));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_complete_clears_with_dequeue_height() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(9, [9u8; 32], 1, b"a").unwrap();
    q.enqueue(10, [10u8; 32], 2, b"b").unwrap();
    q.mark_resolve_complete(10).unwrap();
    assert_eq!(q.dequeue_height(10).unwrap(), 1);
    assert!(!q.is_resolve_complete(10));
    assert!(q.contains_height(9));
    assert!(!q.is_resolve_complete(9));
    assert!(q.mark_resolve_complete(10).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lookup_take_removes_bq_row() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(10, [10u8; 32], 7, b"aaaa").unwrap();
    q.enqueue(11, [11u8; 32], 8, b"bb").unwrap();
    assert_eq!(q.bytes(), 6);
    let got = q.take_raw(10).expect("take 10");
    assert_eq!(got.hash, [10u8; 32]);
    assert_eq!(got.header_fk, 7);
    assert_eq!(q.header_fk_at(11), Some(8));
    assert!(q.header_fk_at(10).is_none());
    assert_eq!(got.payload, b"aaaa");
    assert!(!q.contains_height(10));
    assert!(q.contains_height(11));
    assert_eq!(q.bytes(), 2);
    assert!(q.take_raw(10).is_none());
    assert!(q.take_raw(99).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn promote_wave_drops_raw_and_charges_decoded() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(10, [1u8; 32], 1, b"aaaa").unwrap();
    q.enqueue(11, [2u8; 32], 2, b"bb").unwrap();
    q.enqueue(12, [3u8; 32], 3, b"c").unwrap();
    assert_eq!(q.bytes(), 7);
    let raw = q.raw_payloads(&[10, 11, 99]);
    assert_eq!(raw.len(), 2);
    assert_eq!(q.promote_wave(&[(10, 16), (11, 8)]).unwrap(), 2);
    assert_eq!(
        q.bytes(),
        16 + 8 + 1,
        "promoted charge replaces raw; 12 stays raw"
    );
    assert!(q.get_by_height(10).unwrap().unwrap().payload.is_empty());
    assert!(q.get_by_height(11).unwrap().unwrap().payload.is_empty());
    assert_eq!(q.get_by_height(12).unwrap().unwrap().payload, b"c");
    assert_eq!(q.raw_payloads(&[10, 11, 12]), vec![(12, b"c".to_vec())]);
    assert_eq!(q.promoted_count(), 2);
    let meta10 = q.list_meta().into_iter().find(|m| m.height == 10).unwrap();
    assert_eq!(meta10.payload_len, 16);
    assert_eq!(q.dequeue_height(10).unwrap(), 1);
    assert_eq!(q.bytes(), 8 + 1);
    assert_eq!(q.promoted_count(), 1);
    assert!(q.get_by_height(10).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn raw_payload_clones_one_has_raw_does_not() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    for h in 0..8u32 {
        q.enqueue(h, [h as u8; 32], 1, &[h as u8; 8]).unwrap();
    }
    assert!(q.has_raw(3));
    let first = q.raw_payload(3).unwrap();
    let second = q.raw_payload(3).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.as_slice(), &[3u8; 8]);
    let taken = q.take_raw(3).unwrap();
    assert_eq!(taken.payload.as_slice(), first.as_slice());
    assert!(!q.has_raw(3));
    assert!(q.raw_payload(3).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn height_index_survives_promote_and_dequeue() {
    let dir = temp();
    let mut q = BlockQueue::open_or_create(&dir).unwrap();
    q.enqueue(10, [10u8; 32], 1, b"a").unwrap();
    q.enqueue(11, [11u8; 32], 2, b"b").unwrap();
    q.enqueue(12, [12u8; 32], 3, b"c").unwrap();
    assert_eq!(q.id_for_height(11), Some(2));
    assert!(q.contains_height(11));
    q.mark_resolve_complete(11).unwrap();
    assert!(q.is_resolve_complete(11));
    assert!(!q.is_resolve_complete(10));
    assert_eq!(q.promote_wave(&[(11, 32)]).unwrap(), 1);
    assert_eq!(q.hash_at_height(11), Some([11u8; 32]));
    let snap = q.pack_snapshot(&[10, 11, 12, 99]);
    assert_eq!(snap.len(), 3);
    assert_eq!(snap[0], (10, [10u8; 32], false));
    assert_eq!(snap[1], (11, [11u8; 32], true));
    assert_eq!(snap[2], (12, [12u8; 32], false));
    assert_eq!(q.dequeue_height(11).unwrap(), 1);
    assert!(!q.contains_height(11));
    assert!(!q.is_resolve_complete(11));
    assert!(q.contains_height(10));
    assert!(q.contains_height(12));
    assert_eq!(q.id_for_height(10), Some(1));
    assert_eq!(q.id_for_height(12), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
}
