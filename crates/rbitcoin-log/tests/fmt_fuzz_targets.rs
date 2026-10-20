//! `#[path]` modules so `cargo fmt --all` formats `fuzz/fuzz_targets`.
#![cfg(any())]
use rbitcoin_log::api_log::PARAMS_MAX;
use rbitcoin_log::api_log::{api_call, close_api_log, init_api_log};
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "../../../fuzz/fuzz_targets/addrv2_wire.rs"]
mod addrv2_wire;
#[path = "../../../fuzz/fuzz_targets/block_csv_differential.rs"]
mod block_csv_differential;
#[path = "../../../fuzz/fuzz_targets/block_differential.rs"]
mod block_differential;
#[path = "../../../fuzz/fuzz_targets/block_fork_differential.rs"]
mod block_fork_differential;
#[path = "../../../fuzz/fuzz_targets/block_reorg_n_differential.rs"]
mod block_reorg_n_differential;
#[path = "../../../fuzz/fuzz_targets/block_spend_differential.rs"]
mod block_spend_differential;
#[path = "../../../fuzz/fuzz_targets/block_wire.rs"]
mod block_wire;
#[path = "../../../fuzz/fuzz_targets/chain_review_differential.rs"]
mod chain_review_differential;
#[path = "../../../fuzz/fuzz_targets/cmpct_differential.rs"]
mod cmpct_differential;
#[path = "../../../fuzz/fuzz_targets/cmpct_reorg_differential.rs"]
mod cmpct_reorg_differential;
#[path = "../../../fuzz/fuzz_targets/electrum_json.rs"]
mod electrum_json;
#[path = "../../../fuzz/fuzz_targets/inv_getdata_wire.rs"]
mod inv_getdata_wire;
#[path = "../../../fuzz/fuzz_targets/mempool_differential.rs"]
mod mempool_differential;
#[path = "../../../fuzz/fuzz_targets/p2p_sequence_differential.rs"]
mod p2p_sequence_differential;
#[path = "../../../fuzz/fuzz_targets/script_differential.rs"]
mod script_differential;
#[path = "../../../fuzz/fuzz_targets/script_kernel_differential.rs"]
mod script_kernel_differential;
#[path = "../../../fuzz/fuzz_targets/script_verify_differential.rs"]
mod script_verify_differential;
#[path = "../../../fuzz/fuzz_targets/store_reorg.rs"]
mod store_reorg;
#[path = "../../../fuzz/fuzz_targets/v2_contents.rs"]
mod v2_contents;
#[path = "../../../fuzz/fuzz_targets/v2_session.rs"]
mod v2_session;
