use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::{OutPoint, Transaction, Txid};
use rbitcoin_net::MempoolHub;
use rbitcoin_primitives::Height;
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};
mod fee_look_tests;
mod smart_fee_tests;
