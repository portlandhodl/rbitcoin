//! Store wire walk and consensus decode agree on every truncation.
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

use bitcoin::transaction::Version;

fn legacy_1in() -> Transaction {
    Transaction {
        version: Version::ONE,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([0x11; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::from_bytes(vec![0x51, 0x51]),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    }
}

fn p2wpkh_like() -> Transaction {
    let mut tx = legacy_1in();
    tx.input[0].script_sig = ScriptBuf::new();
    tx.input[0].witness = Witness::from_slice(&[vec![0x30; 71], vec![0x02; 33]]);
    tx
}

const FLAG_ZERO_TX: [u8; 10] = [2, 0, 0, 0, 0x00, 0x00, 7, 0, 0, 0];

/// Peer intake walks the wire and lookup decodes it. A payload one side
/// accepts and the other refuses either strands a queued body or drops an
/// honest one, so every truncation and single-byte rewrite must agree.
#[test]
fn block_wire_walk_agrees_with_decode() {
    use bitcoin::consensus::encode::serialize;
    let zero_in = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    // Segwit with one empty and one non-empty witness decodes; rewriting
    // the non-empty stack count to 0 hits the all-empty refusal.
    let mut mixed = p2wpkh_like();
    mixed.input.insert(0, legacy_1in().input[0].clone());
    mixed.input[1].witness = Witness::from_slice(&[[0x01]]);
    let mut raw = vec![0u8; 80];
    raw.push(5);
    for tx in [legacy_1in(), p2wpkh_like(), zero_in, mixed] {
        raw.extend_from_slice(&serialize(&tx));
    }
    raw.extend_from_slice(&FLAG_ZERO_TX);
    let agree = |p: &[u8]| {
        let decoded = rbitcoin_consensus::decode_block_precomputes(p, false)
            .map(|(b, _, _)| b.txdata.iter().map(|t| t.input.len() as u32).sum());
        assert_eq!(
            rbitcoin_store::block_wire_input_count(p),
            decoded,
            "payload {p:02x?}"
        );
        decoded
    };
    assert_eq!(agree(&raw), Some(4));
    for n in 0..raw.len() {
        agree(&raw[..n]);
    }
    for i in 80..raw.len() {
        for v in [0x00, 0x01, 0x02, 0xfd, 0xfe, 0xff] {
            let mut p = raw.clone();
            p[i] = v;
            agree(&p);
        }
    }
}
