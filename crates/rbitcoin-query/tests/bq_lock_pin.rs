//! Pins: getdata→BQ does not take lookup's decoded-stash lock, and queue
//! ids stay unique in enqueue order.
use bitcoin::absolute::LockTime;
use bitcoin::block::{Header as BlockHeader, Version as BlockVersion};
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version as TxVersion;
use bitcoin::{
    Amount, Block, BlockHash, CompactTarget, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxMerkleNode, TxOut, Witness,
};
use rbitcoin_consensus::{
    block_wire_len_from_pres, decode_block_precomputes, pres_for_tip, IdMap, OutPointHasher,
    OutPointSet, TxPrecompute, TxidHasher, TxidMap, TxidSet,
};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::archive::{
    input_records_from_wire, ArchiveWritePlan, CreatePin, CreatePinArc, CreatePinInner,
    WirePlanNeed,
};
use rbitcoin_query::batch_parents::FkSet;
use rbitcoin_query::batch_parents::{
    layout_covers_need, sparse_spender_rels, BatchParents, FkMap, ParentPinView,
    SharedPrevoutScript, U32Map, U64Map, U64Set,
};
use rbitcoin_query::block_filter::{basic_filter_from_scripts, basic_filter_of};
use rbitcoin_query::catchup::IndexMode;
use rbitcoin_query::chain_view::{header_lookup_count, ChainView, ChainViewKind};
use rbitcoin_query::combined_stage::{load_creates_once, CombinedCreate};
use rbitcoin_query::confirm_error::ConfirmError;
use rbitcoin_query::confirm_load::SpendEdges;
use rbitcoin_query::confirm_stats::{
    add as note_confirm, add_dur as note_confirm_dur, ConfirmStats, ConfirmWindow, LastPinPhases,
    LastPlanBatch, LastUnionMiss, LastWritePhases, TipShSnap,
};
use rbitcoin_query::connect::{spawn_sh_writebehind, ConfirmPrepared};
use rbitcoin_query::connect_facade::*;
use rbitcoin_query::convert::header_to_record;
use rbitcoin_query::header_read::{
    expected_next_bits, median_time_past, median_time_past_store, validate_header,
    validate_header_hashed,
};
use rbitcoin_query::in_flight::InFlight;
use rbitcoin_query::index_writebehind::{
    build_indexes_released, index_tip_entry, prepare_live_indexes, spawn_index_writebehind,
    IndexTipEntry, INDEX_STARTUP_GAP_HEIGHTS,
};
use rbitcoin_query::reconstruct::{BlockTxStatRows, StampedTxstatBlock};
use rbitcoin_query::regtest_pad::pad_empty_from;
use rbitcoin_query::resolved_wire::{BlockQueueWaveIntake, ResolvedWire};
use rbitcoin_query::scripthash::{
    BlockTouch, FilteredHistory, HistoryFilter, HistoryOrder, ScriptHashBalance,
    ScriptHashChainStats, ScriptHashHistoryItem, ScriptHashTxSummary, ScriptHashUtxo, ShJoinSlot,
};
use rbitcoin_query::soft_densify::{
    bq_assign_stop_bytes, soft_ahead_quarter_full, soft_assign_restricted,
    soft_confirm_window_covered, soft_confirm_window_n, soft_densify_band_hi, BQ_SOFT_FREE_BYTES,
    TIP_HOLE_MIN_AHEAD_BLOCKS,
};
use rbitcoin_query::sp_read::{tweaks_at_height, tweaks_for_height, tweaks_from_thin_and_body};
use rbitcoin_query::sp_tweaks::{ThinTweakRangeLimits, ThinTweakRow};
use rbitcoin_query::spend_sync::SpendSync;
use rbitcoin_query::stamp::{
    fill_missing_parent_ranges, stamp_external_parents, BatchParentIds, ExternalParentStamp,
    ParentIdent,
};
use rbitcoin_query::wave_prevout::SpendEdge;
use rbitcoin_store::{
    script_hash, HeaderRecord, InputRecord, OutputRecord, PointRecord, ScriptHashRecord,
    SpTweaksTable, Store, StoreError, StoreLayout, TxRecord,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering as AtomicOrdering,
};
use std::sync::{Arc, Condvar, Mutex};

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use std::thread;
use std::time::Duration;

use rbitcoin_query::Query;

fn open() -> Arc<Query> {
    static N: AtomicU64 = AtomicU64::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rbtc-bq-lock-{id}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(Query::open_or_create_tiny(&dir).unwrap())
}

fn assert_independent(reason: &str, hold: impl Sized, work: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(work));
        let _ = tx.send(result);
    });
    let timed = rx.recv_timeout(Duration::from_secs(2));
    drop(hold);
    if timed.is_err() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
    worker.join().unwrap();
    match timed {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => std::panic::resume_unwind(payload),
        Err(_) => panic!("{reason}"),
    }
}

#[test]
fn getdata_enqueue_does_not_wait_on_lookup_resolved() {
    let q = open();
    let hold = q.bq_resolved.lock().unwrap();
    let q2 = Arc::clone(&q);
    assert_independent(
        "getdata enqueue waited on lookup's decoded stash",
        hold,
        move || {
            q2.block_queue_enqueue(7, [7u8; 32], 1, b"payload").unwrap();
            assert!(q2.block_queue_has_hash(&[7u8; 32]));
        },
    );
}

#[test]
fn lookup_read_does_not_wait_on_decoded_stash() {
    let q = open();
    q.block_queue_enqueue(4, [4u8; 32], 1, b"raw").unwrap();
    let hold = q.bq_resolved.lock().unwrap();
    let q2 = Arc::clone(&q);
    assert_independent(
        "lookup row read waited on the decoded stash",
        hold,
        move || {
            assert!(q2.block_queue_hash_at_height(4).is_some());
            assert!(q2.block_queue_raw_payload(4).unwrap().is_some());
            let intake = q2.block_queue_wave_intake(&[4]);
            assert_eq!(intake.raw.len(), 1);
        },
    );
}

#[test]
fn has_hash_tracks_each_height_until_dequeue() {
    let q = open();
    let hash = [9u8; 32];
    q.block_queue_enqueue(1, hash, 1, b"a").unwrap();
    q.block_queue_enqueue(17, hash, 1, b"b").unwrap();
    assert!(q.block_queue_has_hash(&hash));
    assert_eq!(q.block_queue_dequeue_height(1).unwrap(), 1);
    assert!(q.block_queue_has_hash(&hash));
    assert!(q.block_queue_hash_at_height(17).is_some());
    assert_eq!(q.block_queue_dequeue_height(17).unwrap(), 1);
    assert!(!q.block_queue_has_hash(&hash));
    assert_eq!(q.block_queue_count(), 0);
    assert_eq!(q.block_queue_max_height(), None);
}

#[test]
fn queue_ids_stay_unique_and_list_meta_is_oldest_first() {
    let q = open();
    let older = q.block_queue_enqueue(5, [5u8; 32], 1, b"aa").unwrap();
    let newer = q.block_queue_enqueue(3, [3u8; 32], 1, b"b").unwrap();
    assert_ne!(older, newer);
    let meta = q.block_queue_list_meta();
    assert_eq!(meta.len(), 2);
    assert_eq!(meta[0].id, older);
    assert_eq!(meta[0].height, 5);
    assert_eq!(meta[1].id, newer);
    assert_eq!(meta[1].height, 3);
}

#[test]
fn stats_follow_enqueue_and_dequeue() {
    let q = open();
    q.block_queue_enqueue(5, [5u8; 32], 1, b"aaaaa").unwrap();
    q.block_queue_enqueue(9, [9u8; 32], 1, b"bbb").unwrap();
    let (_stop, bytes, count) = q.block_queue_stats();
    assert_eq!(count, 2);
    assert_eq!(bytes, 8);
    assert_eq!(q.block_queue_max_height(), Some(9));
    q.block_queue_dequeue_height(9).unwrap();
    let (_stop, bytes, count) = q.block_queue_stats();
    assert_eq!(count, 1);
    assert_eq!(bytes, 5);
    assert_eq!(q.block_queue_max_height(), Some(5));
}
