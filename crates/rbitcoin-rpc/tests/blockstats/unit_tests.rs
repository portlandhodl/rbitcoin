use bitcoin::hashes::Hash;
use bitcoin::{Amount, Block, BlockHash, OutPoint, ScriptBuf, TxOut};
use rbitcoin_primitives::Height;
use rbitcoin_query::StampedTxstatBlock;
use rbitcoin_rpc::blockstats::*;
use rbitcoin_rpc::methods::{
    map_query, parse_hash32_display, rpc_error, RpcContext, RpcParams, ERR_INVALID_ADDRESS_OR_KEY,
    ERR_INVALID_PARAMETER, ERR_MISC,
};
use rbitcoin_store::{HeaderRecord, TxStatRow};
use serde_json::{json, Map, Value};

#[test]
fn truncated_median_even_averages_two_middles() {
    assert_eq!(truncated_median(vec![]), 0);
    assert_eq!(truncated_median(vec![3]), 3);
    assert_eq!(truncated_median(vec![1, 2, 3]), 2);
    assert_eq!(truncated_median(vec![1, 2, 3, 4]), 2);
    // rpc_getblockstats.py height 103: (2880 + 36600) / 2
    assert_eq!(truncated_median(vec![2880, 2880, 36600, 43200]), 19740);
}

#[test]
fn percentiles_match_core_rpc_tests() {
    let mut scores = Vec::new();
    for _ in 0..100 {
        scores.push((1, 1));
    }
    for _ in 0..100 {
        scores.push((2, 1));
    }
    assert_eq!(percentiles_by_weight(scores, 200), [1, 1, 1, 2, 2]);

    let scores = vec![(1, 9), (2, 16), (4, 50), (5, 10), (9, 15)];
    assert_eq!(percentiles_by_weight(scores, 100), [2, 2, 4, 4, 9]);

    let scores = vec![(1, 9), (2, 11), (2, 5), (4, 50), (5, 10), (9, 15)];
    assert_eq!(percentiles_by_weight(scores, 100), [2, 2, 4, 4, 9]);

    let scores = vec![(1, 100), (2, 1), (3, 1), (3, 1), (999999, 1)];
    assert_eq!(percentiles_by_weight(scores, 104), [1, 1, 1, 1, 1]);
}

#[test]
fn op_return_is_unspendable() {
    let script = ScriptBuf::from_bytes({
        let mut s = vec![0x41];
        s.extend_from_slice(&[0u8; 65]);
        s.push(0xac);
        s
    });
    assert!(!is_unspendable(script.as_bytes()));
    assert!(is_unspendable(&[0x6a, 0x01, 0x21]));
}
