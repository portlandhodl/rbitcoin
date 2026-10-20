use rbitcoin_consensus::error::*;
use rbitcoin_primitives::ScriptNumError;
use std::fmt;

#[test]
fn block_reject_reason_bip113_and_bip112_needles() {
    assert_eq!(
        block_reject_reason(&ConsensusError::BadTx("not final")),
        "bad-txns-nonfinal"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadTx("bad-txns-nonfinal")),
        "bad-txns-nonfinal"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::Script("CSV negative".into())),
        "block-script-verify-flag-failed (Negative locktime)"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::Script("CSV negative txid=ab vin=0".into())),
        "block-script-verify-flag-failed (Negative locktime)"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::Script("stack empty".into())),
        "block-script-verify-flag-failed (Operation not valid with the current stack size)"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::Script("CSV".into())),
        "block-script-verify-flag-failed (Locktime requirement not satisfied)"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadBlock("no transactions")),
        "bad-blk-length"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadBlock("block stripped size too large")),
        "bad-blk-length"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadBlock("first tx not coinbase")),
        "bad-cb-missing"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadBlock("coinbase excess value")),
        "bad-cb-amount"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::BadTx("no inputs")),
        "bad-txns-vin-empty"
    );
    for bip34 in ["bip34 height encoding", "bip34 coinbase script empty"] {
        assert_eq!(
            block_reject_reason(&ConsensusError::BadBlock(bip34)),
            "bad-cb-height"
        );
    }
    assert_eq!(
        block_reject_reason(&ConsensusError::BadHeader("incorrect proof of work bits")),
        "bad-diffbits"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::InvalidPow),
        "high-hash"
    );
    assert_eq!(
        block_reject_reason(&ConsensusError::Script("SIG_DER".into())),
        "block-script-verify-flag-failed (Non-canonical DER signature)"
    );
    // feature_cltv.py five BIP65 paren reasons (stack / negative / type /
    // locktime / SEQUENCE_FINAL).
    for (token, paren) in [
        (
            "stack empty",
            "Operation not valid with the current stack size",
        ),
        ("CLTV negative", "Negative locktime"),
        ("CLTV type", "Locktime requirement not satisfied"),
        ("CLTV", "Locktime requirement not satisfied"),
        ("CLTV final sequence", "Locktime requirement not satisfied"),
    ] {
        assert_eq!(
            block_reject_reason(&ConsensusError::Script(token.into())),
            format!("block-script-verify-flag-failed ({paren})")
        );
        assert_eq!(
            block_reject_log_line(
                "abcd",
                &format!("block-script-verify-flag-failed ({paren})")
            ),
            format!("Block validation error: block-script-verify-flag-failed ({paren})")
        );
    }
    assert_eq!(
        block_reject_reason(&ConsensusError::BadVersion(2)),
        "bad-version(0x00000002)"
    );
    assert_eq!(
        block_reject_log_line("abcd", "bad-version(0x00000002)"),
        "abcd, bad-version(0x00000002)"
    );
    assert_eq!(
        block_reject_log_line(
            "abcd",
            "block-script-verify-flag-failed (Non-canonical DER signature)"
        ),
        "Block validation error: block-script-verify-flag-failed (Non-canonical DER signature)"
    );
}
