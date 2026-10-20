use bitcoin::block::Block;
use bitcoin::hashes::{sha256d, Hash};
use bitcoin::script::{Script, ScriptBuf};
use bitcoin::{Amount, Transaction, TxOut, Witness};
use rbitcoin_consensus::block::facts::{
    add_fee, add_sigops, add_value_in, fee_from_values, fee_sat, header_gap_on_chain,
    median_or_bad_header, reject_assemble_block_shape, reject_batch_starts_at, reject_bits,
    reject_carried_header_hash, reject_confirm_batch, reject_csv_create_height,
    reject_duplicate_input, reject_empty_confirm_batch, reject_genesis_coinbase_spend,
    reject_non_final, reject_prev_link, reject_prevout_spent, reject_same_block_create,
    reject_sequence_locks, reject_spend_cursor, reject_tx_without_inputs,
    reject_tx_without_outputs, reject_txid_count, reject_unresolved_spend,
    reject_unspendable_prevout, reject_vout_past_n_out, reject_witness_before_segwit,
    require_linked_parent, require_next_work, require_retarget_first, require_same_block_output,
    unresolved_parent_create,
};
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::milestone::Milestone;
use rbitcoin_consensus::params::ChainParams;
use rbitcoin_consensus::TxPrecompute;
use rbitcoin_consensus::TxidHasher;
use rbitcoin_primitives::script_sigop_count;
use rbitcoin_primitives::Height;
use std::borrow::Borrow;
use std::hash::BuildHasherDefault;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Instant;
mod bip34_tests;
mod block_866342;
mod fact_reject_tests;
mod finality_tests;
mod sigop_cost_tests;
