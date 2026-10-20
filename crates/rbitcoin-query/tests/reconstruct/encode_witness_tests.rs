use rbitcoin_primitives::Fk;
use rbitcoin_query::reconstruct::*;
use rbitcoin_query::U64Map;
use rbitcoin_query::*;
use rbitcoin_store::{InputRecord, OutputRecord, StoreError, TxRecord};
use std::time::Instant;

#[test]
fn encode_class_a_tx_emits_segwit_marker_when_input_has_witness() {
    let rec = TxRecord {
        txid: [0xee; 32],
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let ins = vec![InputRecord::coinbase(
        u32::MAX,
        vec![0x01],
        vec![vec![0x51]],
    )];
    let outs = vec![OutputRecord::unspent(50, vec![0x51])];
    let mut out = Vec::new();
    encode_class_a_tx(&mut out, &rec, &ins, &outs);
    assert_eq!(&out[4..6], &[0, 1], "BIP141 marker after version");
    let no_wit = vec![InputRecord::coinbase(u32::MAX, vec![0x01], vec![])];
    let mut plain = Vec::new();
    encode_class_a_tx(&mut plain, &rec, &no_wit, &outs);
    assert_ne!(&plain[4..6], &[0, 1]);
}

#[test]
fn txstat_in_sum_reads_same_block_parent() {
    use bitcoin::hashes::Hash;
    let (_dir, q) = rbitcoin_query::testutil::tiny_query_labeled("txstat-in-sum-same");
    let parent = bitcoin::Transaction {
        version: bitcoin::transaction::Version::ONE,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint::null(),
            script_sig: bitcoin::script::ScriptBuf::from_bytes(vec![0x01]),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(50_0000_0000),
            script_pubkey: bitcoin::script::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let child = bitcoin::Transaction {
        version: bitcoin::transaction::Version::ONE,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: parent.compute_txid(),
                vout: 0,
            },
            script_sig: bitcoin::script::ScriptBuf::new(),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(49_0000_0000),
            script_pubkey: bitcoin::script::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let mut same = std::collections::HashMap::new();
    same.insert(parent.compute_txid().to_byte_array(), &parent);
    assert_eq!(
        txstat_in_sum_from_block(&q, &same, &child).unwrap(),
        Some(50_0000_0000)
    );
    let bad_vout = bitcoin::Transaction {
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: parent.compute_txid(),
                vout: 9,
            },
            ..child.input[0].clone()
        }],
        ..child.clone()
    };
    let err = txstat_in_sum_from_block(&q, &same, &bad_vout).unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt(s) if s.contains("stamp same-block vout")),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(_dir.path());
}
