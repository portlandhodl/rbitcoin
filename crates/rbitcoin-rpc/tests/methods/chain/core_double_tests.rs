use bitcoin::consensus::{deserialize, Encodable};
use bitcoin::hashes::Hash;
use bitcoin::{Amount, Block, BlockHash, ScriptBuf, Txid};
use rbitcoin_primitives::{median_time_past_times, Height};
use rbitcoin_rpc::methods::chain::format_core_double;
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Instant;

#[test]
fn sixteen_significant_digits() {
    assert_eq!(format_core_double(1.0), "1");
    assert_eq!(format_core_double(0.0), "0");
    assert_eq!(format_core_double(132757073449487.52), "132757073449487.5");
}
