use rbitcoin_primitives::ScriptNumError;
use std::fmt;

#[derive(Debug)]
pub enum ConsensusError {
    /// Programmer invariant inside a rule helper. Not a block reject and not a
    /// store fault. The confirm pipeline must not cache it as a failed block.
    Internal(&'static str),
    BadHeader(&'static str),
    BadBlock(&'static str),
    BadTx(&'static str),
    Script(String),
    MissingPrevout,
    PrevoutSpent,
    InvalidPow,
    BadPrev,
    /// BIP34/66/65 outdated `nVersion` (`bad-version(0x…)`).
    BadVersion(i32),
    /// Cooperative abort (IBD SIGINT / confirm cancel) — not a block reject.
    Cancelled,
}

impl fmt::Display for ConsensusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsensusError::Internal(s) => write!(f, "{s}"),
            ConsensusError::BadHeader(s) => write!(f, "bad header: {s}"),
            ConsensusError::BadBlock(s) => write!(f, "bad block: {s}"),
            ConsensusError::BadTx(s) => write!(f, "bad transaction: {s}"),
            ConsensusError::Script(s) => write!(f, "script verification failed: {s}"),
            ConsensusError::MissingPrevout => f.write_str("missing prevout"),
            ConsensusError::PrevoutSpent => f.write_str("prevout already spent on best chain"),
            ConsensusError::InvalidPow => f.write_str("pow invalid"),
            ConsensusError::BadPrev => f.write_str("unexpected previous header"),
            ConsensusError::BadVersion(v) => write!(f, "bad-version(0x{v:08x})"),
            ConsensusError::Cancelled => f.write_str("confirm cancelled"),
        }
    }
}

impl std::error::Error for ConsensusError {}

impl From<ScriptNumError> for ConsensusError {
    fn from(e: ScriptNumError) -> Self {
        ConsensusError::Script(e.to_string())
    }
}

/// Core `SCRIPT_VERIFY_*` parenthetical for `*-script-verify-flag-failed (…)`.
pub fn script_flag_paren(token: &str) -> &str {
    let inner = token.split(" txid=").next().unwrap_or(token);
    match inner {
        "CSV negative" | "CLTV negative" => "Negative locktime",
        "CSV" | "CSV version" | "CLTV" | "CLTV type" | "CLTV final sequence" => {
            "Locktime requirement not satisfied"
        }
        "stack empty" | "stack size" => "Operation not valid with the current stack size",
        "NULLDUMMY" => "Dummy CHECKMULTISIG argument must be zero",
        "SIG_DER" => "Non-canonical DER signature",
        other => other,
    }
}

/// Core `BlockValidationState` / `debug.log` reject needle for a confirm error.
///
/// P2P `assert_debug_log` and `submitblock` share this. Script messages keep
/// our internal tokens (`CSV negative`); only this helper speaks Core English.
pub fn block_reject_reason(err: &ConsensusError) -> String {
    match err {
        ConsensusError::BadTx("not final" | "bad-txns-nonfinal") => "bad-txns-nonfinal".into(),
        ConsensusError::BadTx("no inputs") => "bad-txns-vin-empty".into(),
        ConsensusError::BadTx("no outputs") => "bad-txns-vout-empty".into(),
        ConsensusError::BadTx("coinbase immature") => "bad-txns-premature-spend-of-coinbase".into(),
        ConsensusError::BadTx("in < out") => "bad-txns-in-belowout".into(),
        // Core: the second spend finds the coin already spent.
        ConsensusError::BadTx("double spend in block") => "bad-txns-inputs-missingorspent".into(),
        ConsensusError::BadTx(s) => (*s).into(),
        ConsensusError::BadBlock("no transactions" | "block stripped size too large") => {
            "bad-blk-length".into()
        }
        ConsensusError::BadBlock("first tx not coinbase" | "merkle mutated by a 64-byte tx") => {
            "bad-cb-missing".into()
        }
        ConsensusError::BadBlock("coinbase excess value") => "bad-cb-amount".into(),
        ConsensusError::BadBlock("coinbase not first") => "bad-cb-multiple".into(),
        // Core has no such CheckBlock rule: the second copy spends a spent
        // input at connect. bad-txns-duplicate is the mutated-merkle reason.
        ConsensusError::BadBlock("duplicate txid") => "bad-txns-inputs-missingorspent".into(),
        ConsensusError::BadBlock("merkle root mismatch") => "bad-txnmrklroot".into(),
        ConsensusError::BadBlock("bip34 height encoding" | "bip34 coinbase script empty") => {
            "bad-cb-height".into()
        }
        ConsensusError::BadBlock("block weight too large") => "bad-blk-weight".into(),
        ConsensusError::BadBlock(s) => (*s).into(),
        ConsensusError::BadHeader("timestamp <= median-time-past") => "time-too-old".into(),
        ConsensusError::BadHeader("timestamp too far in future") => "time-too-new".into(),
        ConsensusError::BadHeader("incorrect proof of work bits" | "target above pow limit") => {
            "bad-diffbits".into()
        }
        ConsensusError::BadHeader(s) => (*s).into(),
        ConsensusError::InvalidPow => "high-hash".into(),
        ConsensusError::MissingPrevout | ConsensusError::PrevoutSpent => {
            "bad-txns-inputs-missingorspent".into()
        }
        ConsensusError::BadVersion(v) => format!("bad-version(0x{v:08x})"),
        ConsensusError::Script(s) => {
            format!("block-script-verify-flag-failed ({})", script_flag_paren(s))
        }
        other => other.to_string(),
    }
}

/// Core debug.log line for a rejected block (`feature_dersig` / `feature_cltv`).
pub fn block_reject_log_line(hash: impl std::fmt::Display, reason: &str) -> String {
    if reason.starts_with("bad-version") {
        format!("{hash}, {reason}")
    } else {
        format!("Block validation error: {reason}")
    }
}
