use bitcoin::absolute::LockTime;
use bitcoin::bip158::{BlockFilter, FilterHash, FilterHeader, GcsFilterWriter};
use bitcoin::block::{Header, Version as BlockVersion};
use bitcoin::hashes::Hash;
use bitcoin::{Block, OutPoint};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::{Query, QueryError};
use rbitcoin_store::{
    read_index_window, BlockFilterRecord, BlockFilterSlot, BlockFilterTable, IndexHeight,
    IndexWindow, StoreError,
};
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};

use bitcoin::transaction::Version;
use bitcoin::{Amount, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};

#[test]
fn wire_filter_missing_prevout_is_corrupt() {
    let (_dir, q) = rbitcoin_query::testutil::tiny_query_labeled("filter-missing-prevout");
    let spend = Transaction {
        version: Version::ONE,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([0x7a; 32]),
                vout: 1,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let block = Block {
        header: Header {
            version: BlockVersion::ONE,
            prev_blockhash: bitcoin::BlockHash::from_byte_array([0; 32]),
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([0; 32]),
            time: 2,
            bits: bitcoin::CompactTarget::from_consensus(0x207fffff),
            nonce: 0,
        },
        txdata: vec![spend],
    };
    let err = q.basic_filter_from_wire_block(&block).unwrap_err();
    assert!(
        err.to_string().contains("blockfilter prevout missing"),
        "{err}"
    );
}
