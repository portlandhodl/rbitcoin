//! bip34_tests (peeled from block.rs).
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

use super::bip34_height_script;

#[test]
fn small_heights_use_op_n() {
    assert_eq!(bip34_height_script(0), vec![0x00]);
    for h in 1u32..=16 {
        assert_eq!(bip34_height_script(h), vec![0x50 + h as u8]);
    }
}

#[test]
fn height_17_uses_push() {
    assert_eq!(bip34_height_script(17), vec![0x01, 0x11]);
}

#[test]
fn height_128_sign_byte() {
    // 128 = 0x80 needs trailing 0x00 so it is not negative
    assert_eq!(bip34_height_script(128), vec![0x02, 0x80, 0x00]);
}

#[test]
fn multi_byte_pad_and_256() {
    assert_eq!(bip34_height_script(255), vec![0x02, 0xff, 0x00]);
    assert_eq!(bip34_height_script(256), vec![0x02, 0x00, 0x01]);
}

#[test]
fn kill_bip34_remaining_edges() {
    // 32768 = 0x8000 -> little endian 0x00 0x80, high bit set -> needs 0x00 pad
    // tests high-bit pad path: last & 0x80!=0 => push 0x00
    assert_eq!(bip34_height_script(32768), vec![0x03, 0x00, 0x80, 0x00]);
    // 700k = real mainnet height, 3 bytes, last byte 0x0a no pad
    assert_eq!(bip34_height_script(700_000), vec![0x03, 0x60, 0xae, 0x0a]);
    // 128 vs 127 distinction kills & with | at last & 0x80
    assert_ne!(bip34_height_script(127), bip34_height_script(128));
}
