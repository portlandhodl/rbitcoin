use bitcoin::consensus::Encodable;
use bitcoin::hashes::Hash;
use rbitcoin_consensus::ChainParams;
use rbitcoin_electrum::server::*;
use rbitcoin_net::RequestMeter;
use rbitcoin_net::{BlockingRegion, MempoolHub};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::{ChainView, ChainViewKind, HistoryFilter, Query, ShJoinSlot};
use rbitcoin_store::{script_hash, StoreError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Notify, Semaphore};
use tokio::task::JoinHandle;

#[test]
fn silent_payment_log_drops_the_scan_secret() {
    let secret = "ab".repeat(32);
    let line = redact_sp_params(&json!([secret, "02ff", 1]));
    assert!(!line.contains(&secret), "{line}");
    assert!(line.contains("<redacted>"), "{line}");
}
