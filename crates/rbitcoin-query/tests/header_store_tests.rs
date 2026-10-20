//! Store-backed header reads. Rule rejects stay in `rbitcoin_consensus::check_header`.
#![cfg(test)]
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

use rbitcoin_query::testutil::FixtureChain;

use rbitcoin_consensus::{genesis_block, ChainParams, ConsensusError};
use rbitcoin_primitives::median_time_past_times;

use std::time::{SystemTime, UNIX_EPOCH};

use rbitcoin_query::{Query, TxApply};

fn temp_q() -> (std::path::PathBuf, Query) {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rbitcoin-hdr-mtp-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let q = Query::open_or_create_tiny(dir.join("store")).unwrap();
    (dir, q)
}

/// Write-gate-safe synthetic coinbase: non-null `prev_fk` must commit `parent_hash`.
fn coinbase(h: u32, prev: Fk, parent_hash: Option<[u8; 32]>) -> (HeaderRecord, TxApply) {
    let version = 1;
    let timestamp = 1_000 + h * 10;
    let bits = 0x207fffff;
    let nonce = h;
    let mut merkle = [0u8; 32];
    merkle[0..4].copy_from_slice(&h.to_le_bytes());
    merkle[4] = 0xcd;
    let hash = match parent_hash {
        None => merkle,
        Some(ph) => {
            rbitcoin_store::block_header_hash(version, &ph, &merkle, timestamp, bits, nonce)
        }
    };
    let header = HeaderRecord {
        prev_fk: prev,
        version,
        timestamp,
        bits,
        nonce,
        merkle_root: merkle,
        hash,
        size: 0,
        weight: 0,
    };
    let mut txid = [0u8; 32];
    txid[0..4].copy_from_slice(&h.to_le_bytes());
    txid[31] = 0xcb;
    let ta = TxApply {
        tx: TxRecord {
            txid,
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: 1,
        },
        inputs: vec![InputRecord {
            prev_txid: [0u8; 32],
            parent: rbitcoin_store::InputParent::Coinbase,
            sequence: u32::MAX,
            script_sig: vec![h as u8],
            witness: vec![],
        }],
        outputs: vec![OutputRecord::unspent(50, vec![0x51])],
    };
    (header, ta)
}

#[test]
fn mtp_from_confirmed_chain_and_missing_above_tip() {
    let (dir, q) = temp_q();
    let mut prev = Fk::NULL;
    let mut parent_hash: Option<[u8; 32]> = None;
    for h in 0..3u32 {
        let (hdr, ta) = coinbase(h, prev, parent_hash);
        parent_hash = Some(hdr.hash);
        prev = q.connect_block(Height(h), &hdr, &[ta]).unwrap();
    }
    let mtp = median_time_past(&q, Height(2)).unwrap();
    // times: 1000, 1010, 1020 → middle 1010
    assert_eq!(mtp, 1010);
    let (n, buf) = q
        .store()
        .mtp_times_at(Height(2))
        .expect("ring covers confirmed tip");
    assert_eq!(n, 3);
    assert_eq!(median_time_past_times(&buf[..3]).unwrap(), 1010);

    // Height above tip with no plan → incomplete load error (not BadPrev).
    let err = median_time_past(&q, Height(5)).unwrap_err();
    assert!(
        format!("{err}").contains("load incomplete"),
        "above tip must be the retryable load error, got {err:?}"
    );

    let (empty_dir, empty) = temp_q();
    let err = median_time_past(&empty, Height(0)).unwrap_err();
    assert!(
        matches!(err, ConfirmError::Rules(ConsensusError::BadPrev)),
        "a missing header at the tip is BadPrev, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&empty_dir);

    // expected_next_bits: height 0 + regtest no-retarget.
    let params = ChainParams::regtest();
    let gbits = expected_next_bits(&q, &params, Height(0), 0).unwrap();
    assert_eq!(gbits, genesis_block(&params).header.bits);
    let b1 = expected_next_bits(&q, &params, Height(1), 0).unwrap();
    let (_fk, rec0) = q.header_at_height(Height(0)).unwrap().unwrap();
    assert_eq!(b1.to_consensus(), rec0.bits);

    // Bad prev header height.
    assert!(expected_next_bits(&q, &params, Height(99), 0).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sequential_tip_mtp_matches_ring_and_survives_pop() {
    let (dir, q) = temp_q();
    let mut prev = Fk::NULL;
    let mut parent_hash: Option<[u8; 32]> = None;
    let mut times = Vec::new();
    for h in 0..12u32 {
        let (hdr, ta) = coinbase(h, prev, parent_hash);
        times.push(hdr.timestamp);
        parent_hash = Some(hdr.hash);
        prev = q.connect_block(Height(h), &hdr, &[ta]).unwrap();
    }
    let want11 = median_time_past_times(&times[1..]).unwrap();
    assert_eq!(median_time_past_store(&q, Height(11)).unwrap(), want11);
    assert_eq!(median_time_past(&q, Height(11)).unwrap(), want11);
    let (n, buf) = q.store().mtp_times_at(Height(11)).expect("ring at tip");
    assert_eq!(n, 11);
    assert_eq!(median_time_past_times(&buf[..11]).unwrap(), want11);
    assert!(
        q.store().mtp_times_at(Height(5)).is_none(),
        "historical MTP is not the tip ring"
    );
    assert_eq!(
        median_time_past_store(&q, Height(5)).unwrap(),
        median_time_past_times(&times[0..=5]).unwrap()
    );

    q.disconnect_tip().unwrap();
    let want10 = median_time_past_times(&times[0..=10]).unwrap();
    assert_eq!(median_time_past_store(&q, Height(10)).unwrap(), want10);
    let (n, buf) = q
        .store()
        .mtp_times_at(Height(10))
        .expect("ring rebuilt after pop");
    assert_eq!(median_time_past_times(&buf[..n as usize]).unwrap(), want10);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_header_genesis_and_bad_prev() {
    let (dir, q) = temp_q();
    let params = ChainParams::regtest();
    let g = genesis_block(&params);
    // Wrong genesis hash at height 0.
    let mut bad = g.header;
    bad.nonce ^= 1;
    let err = validate_header(&q, &params, Height(0), &bad).unwrap_err();
    assert!(
        matches!(err, ConfirmError::Rules(ConsensusError::BadHeader(_))),
        "{err:?}"
    );

    // Synthetic tip for prev linkage (child hash commits to parent).
    let (h0, ta0) = coinbase(0, Fk::NULL, None);
    let prev = q.connect_block(Height(0), &h0, &[ta0]).unwrap();
    let (h1, ta1) = coinbase(1, prev, Some(h0.hash));
    q.connect_block(Height(1), &h1, &[ta1]).unwrap();

    // Header with wrong prev hash at height 1.
    let mut hdr = g.header;
    hdr.prev_blockhash = bitcoin::BlockHash::from_byte_array([0xee; 32]);
    hdr.time = 2_000;
    hdr.bits = bitcoin::CompactTarget::from_consensus(0x207fffff);
    let err = validate_header(&q, &params, Height(1), &hdr).unwrap_err();
    assert!(
        matches!(
            err,
            ConfirmError::Rules(
                ConsensusError::BadPrev
                    | ConsensusError::BadHeader(_)
                    | ConsensusError::BadVersion(_)
            )
        ),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn testnet_min_difficulty_after_20_minute_gap() {
    let (dir, q) = temp_q();
    let params = ChainParams::testnet();
    assert!(params.allow_min_difficulty_blocks());
    let (h0, ta0) = coinbase(0, Fk::NULL, None);
    let prev_fk = q.connect_block(Height(0), &h0, &[ta0]).unwrap();
    let limit = params.pow_limit.to_compact_lossy();
    assert_ne!(CompactTarget::from_consensus(h0.bits), limit);

    let spacing = params.btc.pow_target_spacing as u32;
    let gap = expected_next_bits(&q, &params, Height(1), h0.timestamp + 2 * spacing + 1).unwrap();
    assert_eq!(gap, limit);
    let eq_boundary =
        expected_next_bits(&q, &params, Height(1), h0.timestamp + 2 * spacing).unwrap();
    assert_eq!(eq_boundary.to_consensus(), h0.bits);

    let (mut h1, ta1) = coinbase(1, prev_fk, Some(h0.hash));
    h1.timestamp = h0.timestamp + 2 * spacing + 1;
    h1.bits = limit.to_consensus();
    h1.hash = rbitcoin_store::block_header_hash(
        h1.version,
        &h0.hash,
        &h1.merkle_root,
        h1.timestamp,
        h1.bits,
        h1.nonce,
    );
    q.connect_block(Height(1), &h1, &[ta1]).unwrap();
    let walked = expected_next_bits(&q, &params, Height(2), h1.timestamp + 100).unwrap();
    assert_eq!(walked.to_consensus(), h0.bits);

    let _ = std::fs::remove_dir_all(&dir);
}
