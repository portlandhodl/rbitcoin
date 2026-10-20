use bitcoin::consensus::{deserialize, Encodable};
use bitcoin::hashes::Hash;
use bitcoin::{Amount, Block, BlockHash, ScriptBuf, Txid};
use rbitcoin_primitives::{median_time_past_times, Height};
use rbitcoin_rpc::methods::chain::*;
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Instant;

#[test]
fn wait_timeout_ms_caps_at_two_minutes() {
    let huge = RpcParams::positional(vec![json!(500_000)]);
    assert_eq!(wait_timeout_ms(&huge, 0, "timeout").unwrap(), 120_000);
    let absent = RpcParams::positional(vec![]);
    assert_eq!(wait_timeout_ms(&absent, 0, "timeout").unwrap(), 30_000);
    let short = RpcParams::positional(vec![json!(50)]);
    assert_eq!(wait_timeout_ms(&short, 0, "timeout").unwrap(), 50);
}
