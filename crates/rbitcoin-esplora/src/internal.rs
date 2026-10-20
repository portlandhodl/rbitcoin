//! mempool/electrs `/internal/*` bulk REST and `/mempool/txids/page`.

use crate::handlers::{outspend_json, spawn_join};
#[cfg(unix)]
use crate::server::not_found;
use crate::server::{block_hash_hex, parse_hash32, pin_or_reject, store_err, AppState};
#[cfg(unix)]
use crate::tx_json::{
    build_tx_json, build_tx_json_from_tx, build_tx_json_from_tx_with_status, tx_status_json,
};
#[cfg(unix)]
use axum::body::Bytes;
use axum::extract::{Path, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bitcoin::hashes::Hash;
use bitcoin::Txid;
#[cfg(unix)]
use rbitcoin_net::{MempoolHub, MempoolTxSnapEntry};
use rbitcoin_query::ChainViewKind;
#[cfg(unix)]
use rbitcoin_store::StoreError;
use serde::Deserialize;
use serde_json::{json, Value};

#[doc(hidden)]
pub const DEFAULT_MAX_TXS: usize = 10_000;

#[derive(Deserialize)]
pub struct MaxTxs {
    pub max_txs: Option<usize>,
}

#[doc(hidden)]
pub fn cap_max_txs(q: &MaxTxs) -> usize {
    q.max_txs.unwrap_or(DEFAULT_MAX_TXS).min(DEFAULT_MAX_TXS)
}

#[doc(hidden)]
pub fn bad_request(msg: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}

#[cfg(unix)]
#[allow(clippy::result_large_err)] // axum Response
#[doc(hidden)]
pub fn parse_txid_array(body: &[u8]) -> Result<Vec<[u8; 32]>, Response> {
    let v: Value = serde_json::from_slice(body).map_err(|_| bad_request("invalid json"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| bad_request("body must be a JSON array of txid hex"))?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let s = x
            .as_str()
            .ok_or_else(|| bad_request("txid must be a hex string"))?;
        let id = parse_hash32(s).map_err(|_| bad_request("unparseable txid"))?;
        out.push(id);
    }
    Ok(out)
}

#[cfg(unix)]
#[doc(hidden)]
pub fn cache_json_str(
    slot: &std::sync::OnceLock<Box<str>>,
    build: impl FnOnce() -> Result<String, ()>,
) -> Option<&str> {
    if let Some(s) = slot.get() {
        return Some(s.as_ref());
    }
    match build() {
        Ok(s) => {
            let _ = slot.set(s.into());
            slot.get().map(|b| b.as_ref())
        }
        Err(()) => None,
    }
}

#[cfg(unix)]
#[doc(hidden)]
pub fn join_cached_objects(parts: &[Option<&str>]) -> String {
    let mut body = String::from("[");
    let mut first = true;
    for p in parts {
        let Some(s) = p else {
            continue;
        };
        if !first {
            body.push(',');
        }
        first = false;
        body.push_str(s);
    }
    body.push(']');
    body
}

#[cfg(unix)]
#[doc(hidden)]
pub fn entry_json_str<'a>(
    st: &AppState,
    mp: &MempoolHub,
    e: &'a MempoolTxSnapEntry,
) -> Option<&'a str> {
    cache_json_str(&e.json, || {
        let v = build_tx_json_from_tx(
            &st.query,
            &e.tx,
            st.network,
            Some(e.fee_sat as i64),
            Some(mp),
        )
        .map_err(|_| ())?;
        serde_json::to_string(&v).map_err(|_| ())
    })
}

#[cfg(unix)]
#[doc(hidden)]
pub fn entry_json(st: &AppState, mp: &MempoolHub, e: &MempoolTxSnapEntry) -> Option<Value> {
    let raw = entry_json_str(st, mp, e)?;
    serde_json::from_str(raw).ok()
}

#[cfg(unix)]
#[doc(hidden)]
pub fn confirmed_or_mempool_tx(st: &AppState, id: &[u8; 32]) -> Option<Value> {
    if let Ok(Some((fk, _))) = st.query.get_tx_by_txid(id) {
        return build_tx_json(&st.query, fk, st.network).ok();
    }
    mempool_tx_json(st, id)
}

#[cfg(unix)]
#[doc(hidden)]
pub fn mempool_tx_json(st: &AppState, id: &[u8; 32]) -> Option<Value> {
    let mp = st.mempool.as_ref()?;
    let tid = Txid::from_byte_array(*id);
    if let Some(e) = mp.mempool_tx_snapshot().get(&tid) {
        return entry_json(st, mp, e);
    }
    let tx = mp.get_tx(&tid)?;
    let fee = mp.get_live_meta(&tid).map(|(f, _)| f as i64);
    build_tx_json_from_tx(&st.query, &tx, st.network, fee, Some(mp)).ok()
}

#[cfg(unix)]
pub async fn post_internal_txs(State(st): State<AppState>, body: Bytes) -> Response {
    if body.len() > st.max_body {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    spawn_join(move || {
        let ids = match parse_txid_array(&body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(v) = confirmed_or_mempool_tx(&st, &id) {
                out.push(v);
            }
        }
        Json(out).into_response()
    })
    .await
}

#[cfg(unix)]
pub async fn post_internal_mempool_txs(State(st): State<AppState>, body: Bytes) -> Response {
    if body.len() > st.max_body {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    spawn_join(move || {
        let ids = match parse_txid_array(&body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(v) = mempool_tx_json(&st, &id) {
                out.push(v);
            }
        }
        Json(out).into_response()
    })
    .await
}

#[cfg(unix)]
#[doc(hidden)]
pub fn mempool_tx_page(st: &AppState, last: Option<&Txid>, max: usize) -> Response {
    let Some(mp) = st.mempool.as_ref() else {
        return Json(Value::Array(Vec::new())).into_response();
    };
    let snap = mp.mempool_tx_snapshot();
    let parts: Vec<Option<&str>> = snap
        .page(last, max)
        .iter()
        .map(|e| entry_json_str(st, mp, e))
        .collect();
    let body = join_cached_objects(&parts);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[cfg(unix)]
pub async fn get_internal_mempool_txs(
    State(st): State<AppState>,
    AxumQuery(q): AxumQuery<MaxTxs>,
) -> Response {
    let max = cap_max_txs(&q);
    spawn_join(move || mempool_tx_page(&st, None, max)).await
}

#[cfg(unix)]
pub async fn get_internal_mempool_txs_all(State(st): State<AppState>) -> Response {
    spawn_join(move || mempool_tx_page(&st, None, usize::MAX)).await
}

#[cfg(unix)]
pub async fn get_internal_mempool_txs_cursor(
    State(st): State<AppState>,
    Path(last): Path<String>,
    AxumQuery(q): AxumQuery<MaxTxs>,
) -> Response {
    let Ok(id) = parse_hash32(&last) else {
        return bad_request("unparseable txid");
    };
    let last = Txid::from_byte_array(id);
    let max = cap_max_txs(&q);
    spawn_join(move || mempool_tx_page(&st, Some(&last), max)).await
}

#[doc(hidden)]
pub fn mempool_txid_page(st: &AppState, last: Option<&Txid>, max: usize) -> Vec<String> {
    let Some(mp) = st.mempool.as_ref() else {
        return Vec::new();
    };
    mp.mempool_tx_snapshot()
        .page(last, max)
        .iter()
        .map(|e| block_hash_hex(&e.txid.to_byte_array()))
        .collect()
}

pub async fn get_mempool_txids_page(
    State(st): State<AppState>,
    AxumQuery(q): AxumQuery<MaxTxs>,
) -> Response {
    let max = cap_max_txs(&q);
    spawn_join(move || Json(mempool_txid_page(&st, None, max)).into_response()).await
}

pub async fn get_mempool_txids_page_cursor(
    State(st): State<AppState>,
    Path(last): Path<String>,
    AxumQuery(q): AxumQuery<MaxTxs>,
) -> Response {
    let Ok(id) = parse_hash32(&last) else {
        return bad_request("unparseable txid");
    };
    let last = Txid::from_byte_array(id);
    let max = cap_max_txs(&q);
    spawn_join(move || Json(mempool_txid_page(&st, Some(&last), max)).into_response()).await
}

#[cfg(unix)]
pub async fn get_internal_block_txs(
    State(st): State<AppState>,
    Path(hash_hex): Path<String>,
) -> Response {
    spawn_join(move || {
        let Ok(hash) = parse_hash32(&hash_hex) else {
            return not_found();
        };
        let Some((header_fk, rec)) = (match st.query.get_header_by_hash(&hash) {
            Ok(v) => v,
            Err(e) => return store_err(e),
        }) else {
            return not_found();
        };
        let fks = match st.query.header_tx_fks(header_fk, Some(&hash)) {
            Ok(Some(fks)) => fks,
            Ok(None) => return not_found(),
            Err(e) => return store_err(e),
        };
        let block = match st
            .query
            .reconstruct_archived_block_from_parts(rec, fks.clone())
        {
            Ok(b) => b,
            Err(e) => return store_err(e),
        };
        if block.txdata.len() != fks.len() {
            return store_err(StoreError::Corrupt(
                "invariant: reconstruct tx count != header_txs",
            ));
        }
        let mut out = Vec::with_capacity(fks.len());
        for (fk, tx) in fks.into_iter().zip(block.txdata.iter()) {
            let status = match tx_status_json(&st.query, fk) {
                Ok(s) => s,
                Err(e) => return store_err(e),
            };
            match build_tx_json_from_tx_with_status(&st.query, tx, st.network, status, None, None) {
                Ok(v) => out.push(v),
                Err(e) => return store_err(e),
            }
        }
        Json(out).into_response()
    })
    .await
}

pub fn outspends_for_txid_opts(st: &AppState, ids: Vec<Option<[u8; 32]>>) -> Response {
    let view = match pin_or_reject(&st.query, ChainViewKind::Tip, None) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mp = st.mempool.as_deref();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(id) = id else {
            out.push(json!([]));
            continue;
        };
        let nout = if let Ok(Some(fk)) = st.query.tx_fk_by_txid(&id) {
            match st.query.store().get_tx_meta_and_outputs(fk) {
                Ok((meta, _)) => meta.output_count,
                Err(e) => return store_err(e),
            }
        } else if let Some(mp) = mp {
            let tid = Txid::from_byte_array(id);
            mp.get_tx(&tid)
                .map(|tx| tx.output.len() as u32)
                .unwrap_or(0)
        } else {
            0
        };
        let mut slots = Vec::with_capacity(nout as usize);
        for vout in 0..nout {
            match outspend_json(&st.query, mp, &id, vout, view.as_ref()) {
                Ok(v) => slots.push(v),
                Err(e) => return store_err(e),
            }
        }
        out.push(Value::Array(slots));
    }
    Json(out).into_response()
}

#[cfg(unix)]
pub fn outspends_for_txids(st: &AppState, ids: Vec<[u8; 32]>) -> Response {
    outspends_for_txid_opts(st, ids.into_iter().map(Some).collect())
}

#[cfg(unix)]
pub async fn post_outspends_by_txid(State(st): State<AppState>, body: Bytes) -> Response {
    if body.len() > st.max_body {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    spawn_join(move || {
        let ids = match parse_txid_array(&body) {
            Ok(v) => v,
            Err(r) => return r,
        };
        outspends_for_txids(&st, ids)
    })
    .await
}

#[cfg(unix)]
pub async fn post_outspends_by_outpoint(State(st): State<AppState>, body: Bytes) -> Response {
    if body.len() > st.max_body {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    spawn_join(move || {
        let v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return bad_request("invalid json"),
        };
        let Some(arr) = v.as_array() else {
            return bad_request("body must be a JSON array of txid:vout");
        };
        let view = match pin_or_reject(&st.query, ChainViewKind::Tip, None) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mp = st.mempool.as_deref();
        let mut out = Vec::with_capacity(arr.len());
        for x in arr {
            let Some(s) = x.as_str() else {
                out.push(json!({"spent": false}));
                continue;
            };
            let Some((tid_s, vout_s)) = s.rsplit_once(':') else {
                out.push(json!({"spent": false}));
                continue;
            };
            let Ok(txid) = parse_hash32(tid_s) else {
                out.push(json!({"spent": false}));
                continue;
            };
            let Ok(vout) = vout_s.parse::<u32>() else {
                out.push(json!({"spent": false}));
                continue;
            };
            match outspend_json(&st.query, mp, &txid, vout, view.as_ref()) {
                Ok(v) => out.push(v),
                Err(e) => return store_err(e),
            }
        }
        Json(out).into_response()
    })
    .await
}
