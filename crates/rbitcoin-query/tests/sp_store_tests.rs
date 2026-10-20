//! Store-backed silent-payment tweak reads.
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

use rbitcoin_query::sp_read::tweak_records_from_window;
use rbitcoin_query::testutil::FixtureChain;

use bitcoin::secp256k1::{self, PublicKey, Secp256k1, SecretKey};

use rbitcoin_consensus::{mine_empty_regtest, tweak_from_tx, ChainParams, Milestone};

use rbitcoin_query::{accept_and_connect_block, Query, TxApply};

fn tmp_store() -> (rbitcoin_query::testutil::TempDir, Query) {
    rbitcoin_query::testutil::tiny_query_labeled("sp-tweaks")
}

fn header(h: u32, prev_fk: Fk, prev_hash: Option<[u8; 32]>) -> HeaderRecord {
    rbitcoin_query::testutil::header_with_merkle_marker(h, prev_fk, prev_hash, 0xec)
}

#[test]
fn thin_compose_matches_engine_outs_without_parents() {
    let (dir, q) = tmp_store();
    let params = ChainParams::regtest();
    // Reuse the P2WPKH→P2TR spend from the walk test via a tiny local chain.
    use bitcoin::hashes::hash160;
    use bitcoin::secp256k1::SecretKey;
    let secp_ctx = Secp256k1::new();
    let sk = SecretKey::from_slice(&[2u8; 32]).unwrap();
    let pk = bitcoin::secp256k1::PublicKey::from_secret_key(&secp_ctx, &sk);
    let ser = pk.serialize();
    let h160 = hash160::Hash::hash(&ser);
    let mut p2wpkh = vec![0x00, 0x14];
    p2wpkh.extend_from_slice(h160.as_ref());
    let (xonly, _) = pk.x_only_public_key();
    let mut p2tr = vec![0x51, 0x20];
    p2tr.extend_from_slice(&xonly.serialize());
    let mut genesis_txid = [0u8; 32];
    genesis_txid[31] = 0xcb;
    let h0 = header(0, Fk::NULL, None);
    let fk0 = q
        .connect_block(
            Height(0),
            &h0,
            &[TxApply {
                tx: TxRecord {
                    txid: genesis_txid,
                    version: 1,
                    locktime: 0,
                    input_start_fk: Fk::NULL,
                    input_count: 1,
                    output_start_fk: Fk::NULL,
                    output_count: 1,
                },
                inputs: vec![InputRecord::coinbase(u32::MAX, vec![0x00], vec![])],
                outputs: vec![OutputRecord::unspent(50_0000_0000, p2wpkh.clone())],
            }],
        )
        .unwrap();
    let create_fk = q.block_tx_fks(Height(0)).unwrap()[0];
    let mut spend_txid = [0u8; 32];
    spend_txid[0] = 0x11;
    spend_txid[31] = 0xcd;
    let h1 = header(1, fk0, Some(h0.hash));
    q.connect_block(
        Height(1),
        &h1,
        &[TxApply {
            tx: TxRecord {
                txid: spend_txid,
                version: 2,
                locktime: 0,
                input_start_fk: Fk::NULL,
                input_count: 1,
                output_start_fk: Fk::NULL,
                output_count: 1,
            },
            inputs: vec![InputRecord {
                prev_txid: genesis_txid,
                parent: rbitcoin_store::InputParent::Prevout { create_fk, vout: 0 },
                sequence: u32::MAX,
                script_sig: vec![],
                witness: vec![vec![0u8; 64], ser.to_vec()],
            }],
            outputs: vec![OutputRecord::unspent(49_0000_0000, p2tr.clone())],
        }],
    )
    .unwrap();

    let naive = rbitcoin_query::tweaks_for_height(&q, &params, Height(1)).unwrap();
    let t = naive.get(&spend_txid).unwrap();
    let spend_fk = q.block_tx_fks(Height(1)).unwrap()[0];
    let (_rec, _ins, outs) = q.store().get_tx_full(spend_fk).unwrap();
    let composed = rbitcoin_query::tweaks_from_thin_and_body(&[(
        spend_txid,
        Some(t.tweak),
        Some(outs.as_slice()),
    )])
    .unwrap();
    let c = composed.get(&spend_txid).unwrap();
    assert_eq!(c.tweak, t.tweak);
    assert_eq!(c.output_pubkeys, t.output_pubkeys);

    let miss = rbitcoin_query::tweaks_from_thin_and_body(&[(spend_txid, Some(t.tweak), None)]);
    assert!(
        matches!(miss, Err(StoreError::Corrupt(m)) if m.contains("invariant")),
        "got {miss:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tweaks_for_height_unknown_is_empty() {
    let (dir, q) = tmp_store();
    let params = ChainParams::regtest();
    assert!(rbitcoin_query::tweaks_for_height(&q, &params, Height(0))
        .unwrap()
        .is_empty());
    let main = ChainParams::mainnet();
    assert!(rbitcoin_query::tweaks_for_height(&q, &main, Height(100))
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A connect with tweaks on seals that height. Disconnect truncates, and
/// the replacement block is sealed on the next connect.
#[test]
fn live_connect_seals_tweaks_and_reorg_truncates() {
    let (dir, q) = tmp_store();
    let params = ChainParams::regtest();
    q.set_sptweaks_enabled(true, Height(0)).unwrap();
    prepare_live_indexes(&q).unwrap();
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    accept_and_connect_block(&q, &params, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(1)));
    let thin0 = q.load_thin_tweaks(Height(0)).unwrap().expect("indexed");
    assert!(
        thin0.is_empty(),
        "coinbase is not eligible — no Class A join"
    );

    let b1 = mine_empty_regtest(genesis.block_hash(), genesis.header.time + 600, 1);
    accept_and_connect_block(&q, &params, Height(1), &b1, Milestone::NONE).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(2)));

    q.disconnect_tip().unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(1)));
    assert!(q.load_thin_tweaks(Height(1)).unwrap().is_none());

    let b1b = mine_empty_regtest(genesis.block_hash(), genesis.header.time + 601, 1);
    accept_and_connect_block(&q, &params, Height(1), &b1b, Milestone::NONE).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(2)));
    assert!(q.load_thin_tweaks(Height(1)).unwrap().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Contiguous connects seal tweaks. The materialize does not rewrite them.
#[test]
fn live_connect_seals_tweaks_builder_leaves_them() {
    let (dir, q) = tmp_store();
    let params = ChainParams::regtest();
    q.set_sptweaks_enabled(true, Height(0)).unwrap();
    prepare_live_indexes(&q).unwrap();
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    accept_and_connect_block(&q, &params, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    let b1 = mine_empty_regtest(genesis.block_hash(), genesis.header.time + 600, 1);
    accept_and_connect_block(&q, &params, Height(1), &b1, Milestone::NONE).unwrap();
    let b2 = mine_empty_regtest(b1.block_hash(), b1.header.time + 600, 2);
    accept_and_connect_block(&q, &params, Height(2), &b2, Milestone::NONE).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(3)));

    q.release_index_writebehind(Height(2));
    build_indexes_released(&q).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(3)));
    for h in 0..=2 {
        assert!(
            q.load_thin_tweaks(Height(h)).unwrap().is_some(),
            "height {h}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

struct SpKeys {
    p2wpkh: Vec<u8>,
    p2tr: Vec<u8>,
    pubkey: Vec<u8>,
}

fn sp_keys() -> SpKeys {
    use bitcoin::hashes::hash160;
    let sk = SecretKey::from_slice(&[2u8; 32]).unwrap();
    let pk = PublicKey::from_secret_key(&secp256k1::Secp256k1::new(), &sk);
    let ser = pk.serialize();
    let mut p2wpkh = vec![0x00, 0x14];
    p2wpkh.extend_from_slice(hash160::Hash::hash(&ser).as_ref());
    let mut p2tr = vec![0x51, 0x20];
    p2tr.extend_from_slice(&pk.x_only_public_key().0.serialize());
    SpKeys {
        p2wpkh,
        p2tr,
        pubkey: ser.to_vec(),
    }
}

fn sp_spend(
    tag: u8,
    prev: ([u8; 32], Fk, u32),
    witness: Vec<Vec<u8>>,
    outs: Vec<OutputRecord>,
) -> TxApply {
    let mut txid = [0u8; 32];
    txid[0] = tag;
    TxApply {
        tx: TxRecord {
            txid,
            version: 2,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: outs.len() as u32,
        },
        inputs: vec![InputRecord {
            prev_txid: prev.0,
            parent: rbitcoin_store::InputParent::Prevout {
                create_fk: prev.1,
                vout: prev.2,
            },
            sequence: u32::MAX,
            script_sig: vec![],
            witness,
        }],
        outputs: outs,
    }
}

fn seal_tweaks(q: &Query, through: u32) {
    q.release_index_writebehind(Height(through));
    build_indexes_released(q).unwrap();
    assert_eq!(q.sptweaks_next_height(), Some(Height(through + 1)));
}

/// Height 1 carries two eligible P2WPKH→P2TR spends around a spend with
/// fat seqsigwit and a fat output script. The naive walk, the index
/// window reader, the engine on the rebuilt wire tx, and the served
/// index all give the same tweaks, and the served read spans the
/// middle txout but not its seqsigwit.
fn pin_tweaks_agree_and_thin_serve_skips_fat_witness(
    q: &Query,
    params: &ChainParams,
    keys: &SpKeys,
    a: [u8; 32],
    b: [u8; 32],
) {
    let naive = rbitcoin_query::tweaks_for_height(q, params, Height(1)).unwrap();
    assert_eq!(naive.len(), 2, "the OP_TRUE spend is not eligible");
    let fks = q.block_tx_fks(Height(1)).unwrap();
    let wire_a = q.reconstruct_tx(fks[0]).unwrap();
    let engine_prev = [TxOut {
        value: Amount::from_sat(20_0000_0000),
        script_pubkey: ScriptBuf::from_bytes(keys.p2wpkh.clone()),
    }];
    let engine = tweak_from_tx(&wire_a, &engine_prev).unwrap();
    assert_eq!(naive[&a].tweak, engine.tweak);
    assert_eq!(naive[&a].output_pubkeys, engine.output_pubkeys);

    let want = vec![Some(naive[&a].tweak), None, Some(naive[&b].tweak)];
    for start in [0, 1] {
        let heights = q.index_heights(start, 1, Some(0)).unwrap();
        let window = q.read_index_window(&heights).unwrap();
        let i = window.blocks.len() - 1;
        assert_eq!(
            tweak_records_from_window(&window, i).unwrap(),
            want,
            "window from {start}: the parent inside or outside the window"
        );
    }

    let mid_seqsigwit = q.store().tx_seqsigwit_range(fks[1]).unwrap().1;
    let mid_txout = q.store().txs.body_range(fks[1]).unwrap().1;
    let _ = q.sample_reset_thin_tweak_body_bytes();
    let rows = q.load_thin_tweaks(Height(1)).unwrap().expect("indexed");
    let read = q.sample_reset_thin_tweak_body_bytes();
    assert_eq!(
        rows.iter().map(|r| r.txid).collect::<Vec<_>>(),
        [a, b],
        "ineligible txs are not joined from Class A"
    );
    assert_eq!((rows[0].p2tr.len(), rows[1].p2tr.len()), (2, 1));
    assert!(
        read >= mid_txout,
        "the txout span covers the ineligible hole (read={read} txout={mid_txout})"
    );
    assert!(
        read < mid_seqsigwit,
        "the fat seqsigwit stays out (read={read} seqsigwit={mid_seqsigwit})"
    );
    let indexed = rbitcoin_query::tweaks_at_height(q, params, Height(1)).unwrap();
    for txid in [a, b] {
        assert_eq!(indexed[&txid].tweak, naive[&txid].tweak);
        assert_eq!(indexed[&txid].output_pubkeys, naive[&txid].output_pubkeys);
    }
}

fn pin_thin_range_limits_and_hole(q: &Query, a: [u8; 32], c: [u8; 32]) {
    use rbitcoin_query::ThinTweakRangeLimits;

    let all = q
        .load_thin_tweaks_range(Height(0), ThinTweakRangeLimits::default())
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|(h, rows)| (h.0, rows.len()))
            .collect::<Vec<_>>(),
        [(0, 0), (1, 2), (2, 1)]
    );
    assert_eq!(all[2].1[0].txid, c);
    for (h, rows) in &all {
        let single = q.load_thin_tweaks(*h).unwrap().expect("indexed");
        assert_eq!(single.len(), rows.len());
        for (s, r) in single.iter().zip(rows) {
            assert_eq!((s.txid, s.tweak, &s.p2tr), (r.txid, r.tweak, &r.p2tr));
        }
    }
    let limited = |max_heights, max_eligible| {
        q.load_thin_tweaks_range(
            Height(1),
            ThinTweakRangeLimits {
                max_heights,
                max_eligible,
                ..ThinTweakRangeLimits::default()
            },
        )
        .unwrap()
    };
    assert_eq!(limited(1, 8192).len(), 1);
    assert_eq!(
        limited(10, 1).len(),
        1,
        "the eligible budget stops after the first height that spends it"
    );
    assert!(q
        .load_thin_tweaks_range(Height(3), ThinTweakRangeLimits::default())
        .unwrap()
        .is_empty());
    let only2 = q
        .load_thin_tweaks_range(Height(2), ThinTweakRangeLimits::default())
        .unwrap();
    assert_eq!(only2.len(), 1);
    assert_eq!(only2[0].0, Height(2));

    let cut = |cut_through| {
        q.load_thin_tweaks_range(
            Height(1),
            ThinTweakRangeLimits {
                cut_through,
                ..ThinTweakRangeLimits::default()
            },
        )
        .unwrap()
    };
    let kept = cut(true);
    let row_a = kept[0].1.iter().find(|r| r.txid == a).unwrap();
    assert_eq!(
        row_a.p2tr.iter().map(|o| o.0).collect::<Vec<_>>(),
        [1],
        "cut-through drops the P2TR output spent at height 2"
    );
    let hist = cut(false);
    assert_eq!(
        hist[0].1.iter().find(|r| r.txid == a).unwrap().p2tr.len(),
        2
    );
}

/// One synthetic chain through the index builder: coinbase at 0, two
/// eligible spends around a fat ineligible one at 1, a spend of one of
/// those P2TR outputs plus a third eligible spend at 2 (sealed with 1 in
/// one window), and at 3 the spend that leaves the first tx all spent.
#[test]
fn sp_tweaks_confirm_life() {
    let (dir, q) = tmp_store();
    let params = ChainParams::regtest();
    let keys = sp_keys();
    q.set_sptweaks_enabled(true, Height(0)).unwrap();
    let sig_wit = || vec![vec![0u8; 64], keys.pubkey.clone()];
    let p2tr = |sat| OutputRecord::unspent(sat, keys.p2tr.clone());

    let mut cb_txid = [0u8; 32];
    cb_txid[31] = 0xcb;
    let h0 = header(0, Fk::NULL, None);
    let fk0 = q
        .connect_block(
            Height(0),
            &h0,
            &[TxApply {
                tx: TxRecord {
                    txid: cb_txid,
                    version: 1,
                    locktime: 0,
                    input_start_fk: Fk::NULL,
                    input_count: 1,
                    output_start_fk: Fk::NULL,
                    output_count: 4,
                },
                inputs: vec![InputRecord::coinbase(u32::MAX, vec![0x00], vec![])],
                outputs: vec![
                    OutputRecord::unspent(20_0000_0000, keys.p2wpkh.clone()),
                    OutputRecord::unspent(20_0000_0000, keys.p2wpkh.clone()),
                    OutputRecord::unspent(5_0000_0000, vec![0x51]),
                    OutputRecord::unspent(5_0000_0000, keys.p2wpkh.clone()),
                ],
            }],
        )
        .unwrap();
    seal_tweaks(&q, 0);
    assert!(
        q.load_thin_tweaks(Height(0)).unwrap().unwrap().is_empty(),
        "a coinbase is not eligible"
    );
    let cb = q.block_tx_fks(Height(0)).unwrap()[0];

    let spend_a = sp_spend(
        0xaa,
        (cb_txid, cb, 0),
        sig_wit(),
        vec![p2tr(12_0000_0000), p2tr(7_0000_0000)],
    );
    let fat = sp_spend(
        0xfe,
        (cb_txid, cb, 2),
        vec![vec![0u8; 16_384]],
        vec![OutputRecord::unspent(4_0000_0000, vec![0x51; 4096])],
    );
    let spend_b = sp_spend(0xbb, (cb_txid, cb, 1), sig_wit(), vec![p2tr(19_0000_0000)]);
    let (a, b) = (spend_a.tx.txid, spend_b.tx.txid);
    let h1 = header(1, fk0, Some(h0.hash));
    let fk1 = q
        .connect_block(Height(1), &h1, &[spend_a, fat, spend_b])
        .unwrap();
    let a_fk = q.block_tx_fks(Height(1)).unwrap()[0];

    let spend_c = sp_spend(0xcc, (cb_txid, cb, 3), sig_wit(), vec![p2tr(4_0000_0000)]);
    let c = spend_c.tx.txid;
    let spend_a0 = sp_spend(
        0xd0,
        (a, a_fk, 0),
        vec![vec![0u8; 64]],
        vec![OutputRecord::unspent(11_0000_0000, keys.p2wpkh.clone())],
    );
    let h2 = header(2, fk1, Some(h1.hash));
    let fk2 = q
        .connect_block(Height(2), &h2, &[spend_c, spend_a0])
        .unwrap();
    assert_eq!(
        q.sptweaks_next_height(),
        Some(Height(1)),
        "confirm writes none"
    );
    seal_tweaks(&q, 2);

    pin_tweaks_agree_and_thin_serve_skips_fat_witness(&q, &params, &keys, a, b);
    pin_thin_range_limits_and_hole(&q, a, c);

    let spend_a1 = sp_spend(
        0xd1,
        (a, a_fk, 1),
        vec![vec![0u8; 64]],
        vec![OutputRecord::unspent(6_0000_0000, vec![0x51])],
    );
    let h3 = header(3, fk2, Some(h2.hash));
    q.connect_block(Height(3), &h3, &[spend_a1]).unwrap();
    seal_tweaks(&q, 3);
    let gone = q
        .load_thin_tweaks_range(
            Height(1),
            rbitcoin_query::ThinTweakRangeLimits {
                cut_through: true,
                ..rbitcoin_query::ThinTweakRangeLimits::default()
            },
        )
        .unwrap();
    assert_eq!(
        gone[0].1.iter().map(|r| r.txid).collect::<Vec<_>>(),
        [b],
        "an all-spent eligible tx is omitted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
