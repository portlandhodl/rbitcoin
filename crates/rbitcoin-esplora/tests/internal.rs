use axum::body::Bytes;
use axum::extract::{Path, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bitcoin::absolute::LockTime;
use bitcoin::hashes::Hash;
use bitcoin::Txid;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use rbitcoin_consensus::{ChainParams, Milestone};
use rbitcoin_esplora::handlers::{outspend_json, spawn_join};
use rbitcoin_esplora::internal::*;
use rbitcoin_esplora::server::not_found;
use rbitcoin_esplora::server::{block_hash_hex, parse_hash32, pin_or_reject, store_err, AppState};
use rbitcoin_esplora::server::{run_esplora, EsploraConfig, EsploraListen};
use rbitcoin_esplora::tx_json::{
    build_tx_json, build_tx_json_from_tx, build_tx_json_from_tx_with_status, tx_status_json,
};
use rbitcoin_net::{MempoolHub, MempoolTxSnapEntry};
use rbitcoin_query::ChainViewKind;
use rbitcoin_store::StoreError;
use serde::Deserialize;
use serde_json::{json, Value};

use rbitcoin_primitives::Height;
use rbitcoin_query::accept_and_connect_block;
use rbitcoin_query::Query;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[test]
fn join_cached_objects_skips_none() {
    assert_eq!(
        join_cached_objects(&[Some("{\"a\":1}"), None, Some("{\"b\":2}")]),
        "[{\"a\":1},{\"b\":2}]"
    );
    assert_eq!(join_cached_objects(&[None, None]), "[]");
}

#[test]
fn cache_json_str_does_not_store_empty_on_err() {
    let slot = std::sync::OnceLock::<Box<str>>::new();
    assert!(cache_json_str(&slot, || Err::<String, ()>(())).is_none());
    assert!(slot.get().is_none());
    assert_eq!(
        cache_json_str(&slot, || Ok::<_, ()>("{\"ok\":true}".into())),
        Some("{\"ok\":true}")
    );
    assert_eq!(slot.get().map(|s| s.as_ref()), Some("{\"ok\":true}"));
}

fn spend_true(cb: Txid, fee: u64, spk: ScriptBuf) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: cb, vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_0000_0000 - fee),
            script_pubkey: spk,
        }],
    }
}

struct Pad {
    dir: rbitcoin_query::testutil::TempDir,
    q: Arc<Query>,
    hub: Arc<MempoolHub>,
    cbs: Vec<Txid>,
    genesis: bitcoin::BlockHash,
}

fn pad_hub(label: &str, n_cb: u32) -> Pad {
    let (dir, q) = rbitcoin_query::testutil::tiny_query_labeled(label);
    let params = ChainParams::regtest();
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    accept_and_connect_block(&q, &params, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    let (_tip, _time, cbs) = rbitcoin_query::pad_empty_from(
        &q,
        &params,
        genesis.block_hash(),
        genesis.header.time,
        1,
        100 + n_cb,
        n_cb,
    );
    let q = Arc::new(q);
    let mp = dir.join("mp");
    std::fs::create_dir_all(&mp).unwrap();
    let hub = MempoolHub::open(&mp, Arc::clone(&q)).unwrap();
    hub.set_relay_enabled(true);
    Pad {
        dir,
        q,
        hub,
        cbs,
        genesis: genesis.block_hash(),
    }
}

fn parse_http(buf: &[u8]) -> (u16, String) {
    let text = String::from_utf8_lossy(buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    (status, body)
}

async fn http_get_unix(sock: &std::path::Path, path: &str) -> (u16, String) {
    let mut stream = UnixStream::connect(sock).await.expect("unix connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: api\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    parse_http(&buf)
}

async fn http_post_unix(sock: &std::path::Path, path: &str, body: &[u8]) -> (u16, String) {
    let mut stream = UnixStream::connect(sock).await.expect("unix connect");
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: api\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    parse_http(&buf)
}

async fn run_unix(
    dir: &std::path::Path,
    q: Arc<Query>,
    mp: Option<Arc<MempoolHub>>,
) -> (rbitcoin_esplora::server::EsploraHandle, std::path::PathBuf) {
    let sock = dir.join("esplora.sock");
    let cfg =
        EsploraConfig::with_listen(EsploraListen::Unix(sock.clone()), bitcoin::Network::Regtest);
    let handle = run_esplora(cfg, q, mp).await.unwrap();
    (handle, sock)
}

/// `/txs/test` dry-runs; `POST /tx` admits. `GET /broadcast` is gone.
async fn test_then_broadcast(sock: &std::path::Path, pad: &Pad, txs: &[Transaction]) {
    use bitcoin::consensus::encode::serialize_hex;

    let hex = serialize_hex(&txs[0]);
    let (st, body) = http_get_unix(sock, "/broadcast").await;
    assert_eq!(st, 404, "{body}");

    let body = serde_json::to_vec(&json!([&hex])).unwrap();
    let (st, resp) = http_post_unix(sock, "/txs/test", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr[0]["allowed"], true, "{resp}");
    assert!(
        !pad.hub.contains(&txs[0].compute_txid()),
        "test must not admit"
    );
    let (st, resp) = http_post_unix(sock, "/txs/test?maxfeerate=0.00000001", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr[0]["allowed"], false, "{resp}");
    assert_eq!(arr[0]["reject-reason"], "max-fee-exceeded");
    let too: Vec<String> = (0..26).map(|_| hex.clone()).collect();
    let body = serde_json::to_vec(&too).unwrap();
    let (st, resp) = http_post_unix(sock, "/txs/test", &body).await;
    assert_eq!(st, 400, "{resp}");
    assert!(resp.contains("Exceeded maximum of 25"), "{resp}");

    let (st, resp) = http_get_unix(sock, &format!("/broadcast?tx={hex}")).await;
    assert_eq!(st, 404, "{resp}");
    let (st, resp) = http_post_unix(sock, "/tx", hex.as_bytes()).await;
    assert_eq!(st, 200, "{resp}");
    assert_eq!(resp, txs[0].compute_txid().to_string());
    for tx in &txs[1..] {
        let (st, resp) = http_post_unix(sock, "/tx", serialize_hex(tx).as_bytes()).await;
        assert_eq!(st, 200, "{resp}");
    }
    assert!(txs.iter().all(|t| pad.hub.contains(&t.compute_txid())));
}

/// `/mempool` totals, then the public txid pages.
async fn mempool_totals_and_txid_pages(sock: &std::path::Path, pad: &Pad) {
    let live = pad.hub.list_live_meta();
    let expect_fee: u64 = live.iter().map(|(_, f, _)| *f).sum();
    let expect_vsize: u64 = live.iter().map(|(_, _, w)| w.saturating_add(3) / 4).sum();
    let (st, body) = http_get_unix(sock, "/mempool").await;
    assert_eq!(st, 200, "{body}");
    let mem: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(mem["count"].as_u64(), Some(3), "{body}");
    assert_eq!(mem["total_fee"].as_u64(), Some(expect_fee), "{body}");
    assert_eq!(mem["vsize"].as_u64(), Some(expect_vsize), "{body}");

    let (st, p1) = http_get_unix(sock, "/mempool/txids/page?max_txs=1").await;
    assert_eq!(st, 200, "{p1}");
    let a1: Vec<String> = serde_json::from_str(&p1).unwrap();
    assert_eq!(a1.len(), 1);
    let (st, p2) = http_get_unix(sock, &format!("/mempool/txids/page/{}?max_txs=10", a1[0])).await;
    assert_eq!(st, 200, "{p2}");
    let a2: Vec<String> = serde_json::from_str(&p2).unwrap();
    assert_eq!(a2.len(), 2, "{p2}");
    assert!(
        !a2.contains(&a1[0]),
        "cursor page must omit {}: {p2}",
        a1[0]
    );
}

async fn internal_mempool_pages(sock: &std::path::Path) {
    let (st, all) = http_get_unix(sock, "/internal/mempool/txs/all").await;
    assert_eq!(st, 200, "{all}");
    let all_arr: Vec<Value> = serde_json::from_str(&all).unwrap();
    assert_eq!(all_arr.len(), 3);
    let (st, p1) = http_get_unix(sock, "/internal/mempool/txs?max_txs=2").await;
    assert_eq!(st, 200, "{p1}");
    let a1: Vec<Value> = serde_json::from_str(&p1).unwrap();
    assert_eq!(a1.len(), 2);
    assert!(a1.iter().all(|v| v.get("txid").is_some()), "{p1}");
    let last = a1[1]["txid"].as_str().unwrap();
    let (st, p2) = http_get_unix(sock, &format!("/internal/mempool/txs/{last}?max_txs=2")).await;
    assert_eq!(st, 200, "{p2}");
    let a2: Vec<Value> = serde_json::from_str(&p2).unwrap();
    assert_eq!(a2.len(), 1);
    let last2 = a2[0]["txid"].as_str().unwrap();
    let (st, p3) = http_get_unix(sock, &format!("/internal/mempool/txs/{last2}?max_txs=2")).await;
    assert_eq!(st, 200, "{p3}");
    let a3: Vec<Value> = serde_json::from_str(&p3).unwrap();
    assert!(a3.is_empty(), "{p3}");
}

async fn internal_txs_and_block_txs(sock: &std::path::Path, pad: &Pad, mem: &str) {
    let conf = pad.cbs[3].to_string();
    let unknown = "00".repeat(32);
    let body = serde_json::to_vec(&json!([conf, mem, unknown])).unwrap();
    let (st, resp) = http_post_unix(sock, "/internal/txs", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr.len(), 2, "{resp}");
    let (st, resp) = http_post_unix(sock, "/internal/txs", br#"["zz"]"#).await;
    assert_eq!(st, 400, "{resp}");
    let (st, resp) = http_post_unix(sock, "/internal/txs", b"[]").await;
    assert_eq!(st, 200, "{resp}");
    assert_eq!(resp, "[]");

    let body = serde_json::to_vec(&json!([conf, mem])).unwrap();
    let (st, resp) = http_post_unix(sock, "/internal/mempool/txs", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr.len(), 1, "confirmed omitted: {resp}");

    let g = pad.genesis.to_string();
    let (st, body) = http_get_unix(sock, &format!("/internal/block/{g}/txs")).await;
    assert_eq!(st, 200, "{body}");
    let arr: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 1, "genesis coinbase");
    assert_eq!(arr[0]["vin"][0]["is_coinbase"], true);
    let (st, ids) = http_get_unix(sock, &format!("/block/{g}/txids")).await;
    assert_eq!(st, 200, "{ids}");
    let txids: Vec<String> = serde_json::from_str(&ids).unwrap();
    assert_eq!(arr.len(), txids.len());
    assert_eq!(arr[0]["txid"].as_str(), Some(txids[0].as_str()));
    let (st, pubp) = http_get_unix(sock, &format!("/block/{g}/txs")).await;
    assert_eq!(st, 200, "{pubp}");
    let pub_arr: Vec<Value> = serde_json::from_str(&pubp).unwrap();
    assert_eq!(pub_arr.len(), 1);
    let (st, miss) = http_get_unix(sock, &format!("/internal/block/{}/txs", "11".repeat(32))).await;
    assert_eq!(st, 404, "{miss}");
}

/// Mempool spends answer the internal bulk outspends and the public `GET /txs/outspends`.
async fn outspends_by_txid_outpoint_and_query(sock: &std::path::Path, pad: &Pad) {
    let spent = pad.cbs[0].to_string();
    let unknown = "ff".repeat(32);
    let body = serde_json::to_vec(&json!([spent, unknown])).unwrap();
    let (st, resp) = http_post_unix(sock, "/internal/txs/outspends/by-txid", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr.len(), 2, "same-length slots");
    assert_eq!(arr[0][0]["spent"], true);
    assert!(arr[0][0].get("vin").is_some(), "{resp}");
    assert_eq!(arr[1], json!([]));
    let op = format!("{spent}:0");
    let body = serde_json::to_vec(&json!([op, "bad"])).unwrap();
    let (st, resp) = http_post_unix(sock, "/internal/txs/outspends/by-outpoint", &body).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["spent"], true);
    assert_eq!(arr[1]["spent"], false);

    let (st, resp) = http_get_unix(sock, &format!("/txs/outspends?txids={spent},{unknown}")).await;
    assert_eq!(st, 200, "{resp}");
    let arr: Vec<Value> = serde_json::from_str(&resp).unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0][0]["spent"], true);
    assert_eq!(arr[1], json!([]));
    let too_many = (0..51)
        .map(|_| "aa".repeat(32))
        .collect::<Vec<_>>()
        .join(",");
    let (st, resp) = http_get_unix(sock, &format!("/txs/outspends?txids={too_many}")).await;
    assert_eq!(st, 400, "{resp}");
    assert!(resp.contains("Too many txids requested"), "{resp}");
}

/// mempool.space's backend on the group-restricted socket: it tests and
/// broadcasts three spends, pages the mempool, then pulls the bulk routes.
#[tokio::test]
async fn esplora_unix_internal() {
    use std::os::unix::fs::PermissionsExt;

    let pad = pad_hub("unix-internal", 4);
    let (handle, sock) = run_unix(
        pad.dir.as_ref(),
        Arc::clone(&pad.q),
        Some(Arc::clone(&pad.hub)),
    )
    .await;
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o660,
        "esplora socket is group-restricted, got {mode:o}"
    );
    let (st, body) = http_get_unix(&sock, "/blocks/tip/height").await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body, pad.q.tip_height().unwrap().0.to_string());

    let spk = ScriptBuf::from_bytes(vec![0x51]);
    let txs: Vec<Transaction> = (0..3)
        .map(|i| spend_true(pad.cbs[i], 1_000 + i as u64, spk.clone()))
        .collect();
    test_then_broadcast(&sock, &pad, &txs).await;
    mempool_totals_and_txid_pages(&sock, &pad).await;
    internal_mempool_pages(&sock).await;
    internal_txs_and_block_txs(&sock, &pad, &txs[0].compute_txid().to_string()).await;
    outspends_by_txid_outpoint_and_query(&sock, &pad).await;

    handle.shutdown().await;
    let _ = pad.dir;
}
