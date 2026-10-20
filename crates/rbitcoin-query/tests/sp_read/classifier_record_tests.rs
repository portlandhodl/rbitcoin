use bitcoin::hashes::Hash;
use bitcoin::{OutPoint, Transaction, TxOut, Witness};
use rbitcoin_consensus::silent_payments::{tweak_from_tx, TaprootOut, TxTweak};
use rbitcoin_consensus::ChainParams;
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::sp_read::{build_tx_and_prevouts, taproot_outs_from_records};
use rbitcoin_query::{ConfirmError, Query};
use rbitcoin_store::{IndexWindow, InputRecord, LoadedTweakTx, OutputRecord, StoreError};
use std::collections::{BTreeMap, HashMap};

#[test]
fn taproot_record_keeps_a_zero_value_and_skips_other_scripts() {
    let mut p2tr = vec![0x51, 0x20];
    p2tr.extend_from_slice(&[0xab; 32]);
    let outs = vec![
        OutputRecord::unspent(0, p2tr),
        OutputRecord::unspent(7, vec![0x51]),
    ];
    let got = taproot_outs_from_records(&outs);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].vout, 0);
    assert_eq!(got[0].value, 0);
    assert_eq!(got[0].xonly, [0xab; 32]);
}

#[test]
fn build_tx_uses_the_parent_output_value() {
    let parent_txid = [0x11; 32];
    let parent_outs = [OutputRecord::unspent(7, vec![0x51])];
    let inp = InputRecord::prevout(parent_txid, Fk(3), 0, 0xffff_ffff, Vec::new(), Vec::new());
    let spend_out = OutputRecord::unspent(1, vec![0x51]);
    let parent = |fk: Fk| {
        if fk == Fk(3) {
            Some((parent_txid, parent_outs.as_slice()))
        } else {
            None
        }
    };
    let (tx, prevouts) = build_tx_and_prevouts(&[inp], &[spend_out], &parent).unwrap();
    assert_eq!(prevouts[0].value.to_sat(), 7);
    assert_eq!(tx.output[0].value.to_sat(), 1);
    assert_eq!(tx.input[0].previous_output.vout, 0);
}
