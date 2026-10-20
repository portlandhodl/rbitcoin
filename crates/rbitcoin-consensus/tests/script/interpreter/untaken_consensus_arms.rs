use bitcoin::hashes::Hash;
use bitcoin::script::{Instruction, Script};
use bitcoin::sighash::SighashCache;
use bitcoin::{Amount, Sequence, Transaction};
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::script::crypto;
use rbitcoin_primitives::{scriptnum_decode_width, scriptnum_encode};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

use bitcoin::absolute::LockTime;
use bitcoin::script::ScriptBuf;
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::{OutPoint, PublicKey, TxIn, TxOut, Witness};
use rbitcoin_consensus::script::interpreter::*;

fn tx_with_inputs(n: usize) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version::ONE,
        lock_time: LockTime::ZERO,
        input: (0..n)
            .map(|_| TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            })
            .collect(),
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    }
}

#[test]
fn find_and_delete_encodes_pushdata4_and_stops_on_a_truncated_push() {
    let encoded = find_and_delete(&[], &vec![0u8; 70_000]);
    assert!(encoded.is_empty());
    assert_eq!(find_and_delete(&[0x4c], &[0x11]), vec![0x4c]);
}

#[test]
fn minimal_push_treats_data_over_64k_as_pushdata4() {
    assert!(check_minimal_push(&vec![0u8; 65_536], 0x4e));
}

#[test]
fn from_job_past_the_prevouts_uses_a_zero_amount() {
    let tx = tx_with_inputs(1);
    let job = rbitcoin_consensus::block::ScriptCheckJob::new(
        vec![TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
        tx,
        rbitcoin_consensus::block::ScriptVerifyFlags::buried(false, false, false, false, false),
    );
    let sc = Script::from_bytes(&[0x51]);
    let ctx = EvalContext::from_job(&job, &job.tx, 3, sc, SigVersion::Base).unwrap();
    assert_eq!(ctx.amount, Amount::ZERO);
}

#[test]
fn checksig_sighash_failure_is_a_soft_false() {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(&[0x11; 32]).unwrap();
    let pk = PublicKey::new(sk.public_key(&secp));
    let mut sig = secp
        .sign_ecdsa(&Message::from_digest([9; 32]), &sk)
        .serialize_der()
        .to_vec();
    sig.push(0x01);
    let tx = tx_with_inputs(1);
    let sc = Script::from_bytes(&[0x51]);
    let ctx = EvalContext::from_eval_parts(
        &tx,
        5,
        Amount::ZERO,
        sc,
        SigVersion::Base,
        false,
        false,
        false,
        Cow::Owned(rbitcoin_consensus::TxPrecompute::from_tx(&tx)),
    );
    let matched = ecdsa_sig_matches(&sig, &pk.to_bytes(), &ctx, &[0x51], PreTapscript::Base);
    assert!(!matched.unwrap());
}

#[test]
fn tapscript_checksig_without_leaf_facts_fails() {
    let tx = tx_with_inputs(1);
    let mut script = vec![0x40];
    script.extend_from_slice(&[0x11; 64]);
    script.push(0x20);
    script.extend_from_slice(&[0x02; 32]);
    script.push(0xac);
    let sc = Script::from_bytes(&script);
    let ctx = EvalContext::from_eval_parts(
        &tx,
        0,
        Amount::ZERO,
        sc,
        SigVersion::TapScript,
        false,
        false,
        false,
        Cow::Owned(rbitcoin_consensus::TxPrecompute::from_tx(&tx)),
    );
    let err = eval_script(sc, &mut Vec::new(), &ctx).unwrap_err();
    assert!(format!("{err}").contains("tapleaf"), "{err}");
}

#[test]
fn tapscript_checksig_rejects_a_signature_that_is_not_64_or_65_bytes() {
    let tx = tx_with_inputs(1);
    let mut script = vec![0x01, 0x11, 0x20];
    script.extend_from_slice(&[0x02; 32]);
    script.push(0xac);
    let sc = Script::from_bytes(&script);
    let ctx = EvalContext::from_eval_parts(
        &tx,
        0,
        Amount::ZERO,
        sc,
        SigVersion::TapScript,
        false,
        false,
        false,
        Cow::Owned(rbitcoin_consensus::TxPrecompute::from_tx(&tx)),
    );
    let err = eval_script(sc, &mut Vec::new(), &ctx).unwrap_err();
    assert!(format!("{err}").contains("CHECKSIG"), "{err}");
}

#[test]
fn tapscript_checksigverify_rejects_an_empty_signature_on_a_32_byte_key() {
    let tx = tx_with_inputs(1);
    let mut script = vec![0x00, 0x20];
    script.extend_from_slice(&[0x02; 32]);
    script.push(0xad);
    let sc = Script::from_bytes(&script);
    let ctx = EvalContext::from_eval_parts(
        &tx,
        0,
        Amount::ZERO,
        sc,
        SigVersion::TapScript,
        false,
        false,
        false,
        Cow::Owned(rbitcoin_consensus::TxPrecompute::from_tx(&tx)),
    );
    let err = eval_script(sc, &mut Vec::new(), &ctx).unwrap_err();
    assert!(format!("{err}").contains("CHECKSIGVERIFY"), "{err}");
}
