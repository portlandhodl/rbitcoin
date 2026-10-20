use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::{CompactTarget, Target};
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::params::{check_genesis_hash, ChainParams};
use rbitcoin_primitives::median_time_past_times;
use rbitcoin_primitives::Height;
mod median_time_past_tests;
