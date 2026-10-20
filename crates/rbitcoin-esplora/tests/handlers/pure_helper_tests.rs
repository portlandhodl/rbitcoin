use axum::body::Bytes;
use axum::extract::{Path, Query as AxumQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bitcoin::address::Address;
use bitcoin::consensus::{deserialize, encode::serialize, Encodable};
use bitcoin::hashes::Hash;
use bitcoin::pow::{CompactTarget, Target};
use bitcoin::{MerkleBlock, Network, OutPoint, Txid};
use rbitcoin_esplora::handlers::{block_summary_json, fee_estimates_from, outspend_json};
use rbitcoin_esplora::server::{
    block_hash_hex, maybe_attach_view, mempool_wire, not_found, parse_hash32, pin_or_reject,
    plain_ok, store_err, AppState, AsOf, GbtCache, JoinClient,
};
use rbitcoin_esplora::tx_json::{
    build_tx_json, build_tx_json_from_tx, history_items_to_tx_json, tx_status_json_in,
    utxo_list_json,
};
use rbitcoin_net::MempoolHub;
use rbitcoin_primitives::Fk;
use rbitcoin_primitives::{median_time_past_times, Height};
use rbitcoin_query::testutil::FixtureChain;
use rbitcoin_query::TxApply;
use rbitcoin_query::{
    ChainViewKind, HistoryFilter, Query, ScriptHashChainStats, ScriptHashTxSummary, ShJoinSlot,
};
use rbitcoin_store::{script_hash, StoreError};
use rbitcoin_store::{HeaderRecord, InputRecord, OutputRecord, TxRecord};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temp_query() -> (rbitcoin_query::testutil::TempDir, Query) {
    rbitcoin_query::testutil::tiny_query_labeled("esplora-pure")
}

fn seed_genesis(q: &Query) -> [u8; 32] {
    let merkle = [0xab; 32];
    let hash = rbitcoin_store::block_header_hash(1, &[0u8; 32], &merkle, 1, 0x207f_ffff, 0);
    let header = HeaderRecord {
        prev_fk: Fk::NULL,
        version: 1,
        timestamp: 1,
        bits: 0x207f_ffff,
        nonce: 0,
        merkle_root: merkle,
        hash,
        size: 0,
        weight: 0,
    };
    let ta = TxApply {
        tx: TxRecord {
            txid: [0xcb; 32],
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
            script_sig: vec![0x00],
            witness: vec![vec![0u8; 32]],
        }],
        outputs: vec![OutputRecord::unspent(50_0000_0000, vec![0x51])],
    };
    let _ = q.connect_block(Height(0), &header, &[ta]).unwrap();
    hash
}

#[test]
fn fee_routes_keep_the_estimator_resolution() {
    // BTC/kvB as the estimator publishes it: whole sat/kvB
    let pairs = [
        (1, 0.000_012_34),
        (3, 0.000_009_87),
        (6, 0.000_003_1),
        (144, 0.000_001_01),
    ];
    let est = fee_estimates_from(&pairs).expect("estimates");
    assert_eq!(est["1"], json!(1.234));
    assert_eq!(est["3"], json!(0.987));
    assert_eq!(est["144"], json!(0.101));
}

#[test]
fn block_summary_bits_u32_and_witness_size_weight() {
    let (dir, q) = temp_query();
    let hash = seed_genesis(&q);
    let summary = block_summary_json(&q, &hash).expect("summary");
    assert!(
        summary["bits"].is_u64(),
        "Esplora bits is u32, not hex: {}",
        summary["bits"]
    );
    assert_eq!(summary["bits"], 0x207f_ffff);
    let block = q.reconstruct_archived_block(&hash).unwrap().unwrap();
    assert_eq!(summary["size"], block.total_size() as u64);
    assert_eq!(summary["weight"], block.weight().to_wu());
    assert_ne!(
        summary["weight"].as_u64().unwrap(),
        summary["size"].as_u64().unwrap().saturating_mul(4),
        "witness block weight is not 4×stripped size"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn outspend_json_spent_includes_vin() {
    let (dir, q) = temp_query();
    let _hash = seed_genesis(&q);
    let create_fk = q.tx_fk_by_txid(&[0xcb; 32]).unwrap().unwrap();
    let spend = (
        TxRecord {
            txid: [0x11; 32],
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: 1,
            output_start_fk: Fk::NULL,
            output_count: 1,
        },
        vec![InputRecord {
            prev_txid: [0xcb; 32],
            parent: rbitcoin_store::InputParent::Prevout { create_fk, vout: 0 },
            sequence: u32::MAX,
            script_sig: vec![],
            witness: vec![],
        }],
        vec![OutputRecord::unspent(1, vec![0x51])],
    );
    let spend_fk = q.store().put_tx_full_batch_indexed(&[spend], true).unwrap()[0];
    q.store()
        .put_spend_create(create_fk, 0, spend_fk, 0)
        .unwrap();
    let view = q.pin_chain_view().unwrap().expect("tip");
    q.store()
        .header_txs
        .put_range(view.header_fk, create_fk, 2)
        .unwrap();
    q.store()
        .strong_tx
        .set_strong(spend_fk, view.header_fk)
        .unwrap();
    q.store().rebuild_height_fence().unwrap();
    let v = outspend_json(&q, None, &[0xcb; 32], 0, Some(&view)).unwrap();
    assert_eq!(v["spent"], true, "{v}");
    assert!(v.get("txid").is_some(), "{v}");
    assert_eq!(v["vin"], 0, "outspend vin from spent slot: {v}");
    let _ = std::fs::remove_dir_all(dir);
}
