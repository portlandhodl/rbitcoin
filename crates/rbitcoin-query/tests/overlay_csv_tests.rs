//! CSV activation overlay changes confirm, not only the height getter.
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

use bitcoin::blockdata::constants;

use bitcoin::Network;
use rbitcoin_consensus::{mine_regtest_paying, ChainParams, Milestone};

use std::time::{SystemTime, UNIX_EPOCH};

use rbitcoin_query::{accept_and_connect_block, Query};

/// Overlay must change **confirm** BIP68, not just the getter.
#[test]
fn overlay_csv_102_is_lax_at_101_strict_at_102() {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rbtc-overlay-csv-{n}"));
    let q = Query::open_or_create_tiny(&dir).unwrap();
    let mut params = ChainParams::regtest();
    params.apply_test_activation_height("csv", 102).unwrap();
    let genesis = constants::genesis_block(Network::Regtest);
    accept_and_connect_block(&q, &params, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    let (tip, tip_time, cbs) = pad_empty_from(
        &q,
        &params,
        genesis.block_hash(),
        genesis.header.time,
        1,
        100,
        1,
    );
    let spk = ScriptBuf::from_bytes(vec![0x51]);
    let parent = Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: cbs[0],
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_0000_0000 - 1_000),
            script_pubkey: spk.clone(),
        }],
    };
    let b101 = mine_regtest_paying(tip, tip_time + 600, 101, spk.clone(), vec![parent.clone()]);
    accept_and_connect_block(&q, &params, Height(101), &b101, Milestone::NONE)
        .expect("parent spend at 101");

    let child = Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: parent.compute_txid(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::from_consensus(10),
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_0000_0000 - 2_000),
            script_pubkey: spk.clone(),
        }],
    };
    let b102 = mine_regtest_paying(
        b101.block_hash(),
        b101.header.time + 600,
        102,
        spk.clone(),
        vec![child],
    );
    let err =
        accept_and_connect_block(&q, &params, Height(102), &b102, Milestone::NONE).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not final") || msg.contains("nonfinal") || msg.contains("BIP68"),
        "csv@102 must reject BIP68-locked spend at height 102: {msg}"
    );

    let dir2 = std::env::temp_dir().join(format!("rbtc-overlay-csv-lax-{n}"));
    let q2 = Query::open_or_create_tiny(&dir2).unwrap();
    let mut lax = ChainParams::regtest();
    lax.apply_test_activation_height("csv", 200).unwrap();
    accept_and_connect_block(&q2, &lax, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    let (tip2, time2, cbs2) = pad_empty_from(
        &q2,
        &lax,
        genesis.block_hash(),
        genesis.header.time,
        1,
        100,
        1,
    );
    let p_lax = Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: cbs2[0],
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_0000_0000 - 1_000),
            script_pubkey: spk.clone(),
        }],
    };
    let b101l = mine_regtest_paying(tip2, time2 + 600, 101, spk.clone(), vec![p_lax.clone()]);
    accept_and_connect_block(&q2, &lax, Height(101), &b101l, Milestone::NONE).unwrap();
    let c_lax = Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: p_lax.compute_txid(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::from_consensus(10),
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_0000_0000 - 2_000),
            script_pubkey: spk,
        }],
    };
    let b102l = mine_regtest_paying(
        b101l.block_hash(),
        b101l.header.time + 600,
        102,
        ScriptBuf::from_bytes(vec![0x51]),
        vec![c_lax],
    );
    accept_and_connect_block(&q2, &lax, Height(102), &b102l, Milestone::NONE)
        .expect("csv@200: BIP68 not consensus at height 102");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
