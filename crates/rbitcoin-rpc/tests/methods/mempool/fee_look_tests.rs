use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::{OutPoint, Transaction, Txid};
use rbitcoin_net::MempoolHub;
use rbitcoin_primitives::Height;
use rbitcoin_rpc::methods::mempool::{
    fold_tx_fee_sat, package_dialect_from_env, submitpackage_topology_disallowed, TxFeeLook,
};
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};

#[test]
fn package_dialect_from_env_only_one_and_true() {
    assert!(!package_dialect_from_env(None));
    assert!(!package_dialect_from_env(Some("")));
    assert!(!package_dialect_from_env(Some("0")));
    assert!(!package_dialect_from_env(Some("false")));
    assert!(package_dialect_from_env(Some("1")));
    assert!(package_dialect_from_env(Some("true")));
    assert!(package_dialect_from_env(Some("TRUE")));
}

#[test]
fn topology_gate_only_when_dialect() {
    use bitcoin::absolute::LockTime;
    use bitcoin::hashes::Hash;
    use bitcoin::transaction::Version;
    use bitcoin::{OutPoint, Sequence, Transaction, TxIn, TxOut, Witness};
    let dummy = |n: u8| Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([n; 32]),
                vout: 0,
            },
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: bitcoin::Amount::from_sat(1),
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let txs = [dummy(1), dummy(2)];
    assert!(!submitpackage_topology_disallowed(&txs, false));
    assert!(submitpackage_topology_disallowed(&txs, true));
    assert!(!submitpackage_topology_disallowed(&txs[..1], true));
}

#[test]
fn overflow_in_sum_is_overflow_not_missing() {
    let half = (u64::MAX / 2) + 2;
    match fold_tx_fee_sat([Some(half), Some(half)], Some(0)) {
        TxFeeLook::Overflow => {}
        other => panic!("expected Overflow, got {other:?}"),
    }
}

#[test]
fn missing_prevout_is_not_overflow() {
    match fold_tx_fee_sat([Some(50), None], Some(1)) {
        TxFeeLook::MissingPrevout => {}
        other => panic!("expected MissingPrevout, got {other:?}"),
    }
}

#[test]
fn known_fee_subtracts() {
    match fold_tx_fee_sat([Some(50_000), Some(25_000)], Some(74_000)) {
        TxFeeLook::Fee(1_000) => {}
        other => panic!("expected Fee(1000), got {other:?}"),
    }
}
