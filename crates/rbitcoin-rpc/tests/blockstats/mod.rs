use bitcoin::hashes::Hash;
use bitcoin::{Amount, Block, BlockHash, OutPoint, ScriptBuf, TxOut};
use rbitcoin_primitives::Height;
use rbitcoin_query::StampedTxstatBlock;
use rbitcoin_rpc::methods::{
    map_query, parse_hash32_display, rpc_error, RpcContext, RpcParams, ERR_INVALID_ADDRESS_OR_KEY,
    ERR_INVALID_PARAMETER, ERR_MISC,
};
use rbitcoin_store::{HeaderRecord, TxStatRow};
use serde_json::{json, Map, Value};
mod unit_tests;
