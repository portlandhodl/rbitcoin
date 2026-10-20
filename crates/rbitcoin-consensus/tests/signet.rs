use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::deserialize;
use bitcoin::consensus::{serialize, Encodable};
use bitcoin::hashes::{sha256d, Hash};
use bitcoin::script::{Script, ScriptBuf};
use bitcoin::{Amount, Block, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness};
use rbitcoin_consensus::block::ScriptCheckJob;
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::signet::*;

#[test]
fn default_challenge_parses() {
    let c = default_signet_challenge();
    assert!(c.as_bytes().len() > 70);
    assert_eq!(c.as_bytes()[0], 0x51); // OP_1
}

#[test]
fn extract_signet_header_payload() {
    let mut push = SIGNET_HEADER.to_vec();
    push.extend_from_slice(&[0x01, 0x02, 0x03]);
    let sol = extract_header_payload(&push).unwrap();
    assert_eq!(sol, vec![0x01, 0x02, 0x03]);
}

#[test]
fn fetch_and_clear_rewrites_push() {
    // OP_RETURN + push(header||0xab)
    let mut spk = vec![0x6a, 0x05];
    spk.extend_from_slice(&SIGNET_HEADER);
    spk.push(0xab);
    let (sol, stripped) = fetch_and_clear_signet_section(&spk).unwrap();
    assert_eq!(sol, vec![0xab]);
    // stripped should still start with OP_RETURN
    assert_eq!(stripped[0], 0x6a);
}

#[test]
fn custom_challenge_derives_expected_wire_magic() {
    let challenge = ScriptBuf::from_bytes(vec![0x51]);
    assert_eq!(
        signet_magic(challenge.as_script()),
        [0x54, 0xd2, 0x6f, 0xbd]
    );
    assert_eq!(
        signet_magic(default_signet_challenge().as_script()),
        [0x0a, 0x03, 0xcf, 0x40]
    );
}

#[test]
fn signet_block_1_rejects_mutated_solution() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let mut block: Block = deserialize(raw).expect("decode");
    // Flip a byte in the witness-commitment output (destroys signature).
    let spk = block.txdata[0].output[1].script_pubkey.as_bytes().to_vec();
    let mut bad = spk.clone();
    *bad.last_mut().unwrap() ^= 0xff;
    block.txdata[0].output[1].script_pubkey = ScriptBuf::from_bytes(bad);
    let challenge = default_signet_challenge();
    assert!(validate_signet_block_solution(&block, challenge.as_script()).is_err());
}

#[test]
fn to_spend_script_sig_starts_with_op_0() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let block: Block = deserialize(raw).unwrap();
    let challenge = default_signet_challenge();
    let (to_spend, _) = signet_challenge_transactions(&block, challenge.as_script()).unwrap();
    let ss = to_spend.input[0].script_sig.as_bytes();
    assert_eq!(ss[0], 0x00, "Core CScript(OP_0) then push(block_data)");
}

#[test]
fn genesis_prev_skips_challenge() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let mut block: Block = deserialize(raw).unwrap();
    block.header.prev_blockhash = bitcoin::BlockHash::from_byte_array([0u8; 32]);
    // Empty txs would fail for non-genesis; genesis path returns Ok first.
    block.txdata.clear();
    validate_signet_block_solution(&block, Script::from_bytes(&[0x51])).unwrap();
}

#[test]
fn empty_block_and_missing_commitment_errors() {
    let challenge = default_signet_challenge();
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let mut block: Block = deserialize(raw).unwrap();
    block.txdata.clear();
    assert!(matches!(
        signet_challenge_transactions(&block, challenge.as_script()),
        Err(ConsensusError::BadBlock(_))
    ));
    let mut block: Block = deserialize(raw).unwrap();
    // Strip commitment-looking outputs from coinbase.
    block.txdata[0].output.clear();
    block.txdata[0].output.push(TxOut {
        value: Amount::from_sat(1),
        script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
    });
    assert!(matches!(
        signet_challenge_transactions(&block, challenge.as_script()),
        Err(ConsensusError::BadBlock(_))
    ));
}

#[test]
fn missing_signet_section_lets_challenge_script_decide() {
    // Bare BIP141 commitment, no SIGNET_HEADER: Core spends with an empty
    // scriptSig and witness and lets VerifyScript decide for any challenge.
    let cb = coinbase_outputs(vec![bip141_commitment_spk(0x00)]);
    let block = Block {
        header: bitcoin::block::Header {
            version: bitcoin::block::Version::ONE,
            prev_blockhash: bitcoin::BlockHash::from_byte_array([1u8; 32]),
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([0u8; 32]),
            time: 1,
            bits: bitcoin::CompactTarget::from_consensus(0x207f_ffff),
            nonce: 0,
        },
        txdata: vec![cb],
    };
    for challenge in [vec![0x51], vec![0x52], vec![0x74, 0x00, 0x87]] {
        let challenge = ScriptBuf::from_bytes(challenge);
        let (to_spend, to_sign) =
            signet_challenge_transactions(&block, challenge.as_script()).unwrap();
        assert!(to_sign.input[0].script_sig.is_empty());
        assert!(to_sign.input[0].witness.is_empty());
        assert_eq!(to_spend.output[0].script_pubkey, challenge);
        validate_signet_block_solution(&block, challenge.as_script()).unwrap();
    }
    let err =
        validate_signet_block_solution(&block, default_signet_challenge().as_script()).unwrap_err();
    assert!(err.to_string().contains("signet solution invalid"), "{err}");
}

#[test]
fn push_data_all_length_encodings() {
    let mut short = Vec::new();
    push_data(&mut short, &[0xab]);
    assert_eq!(short, vec![0x01, 0xab]);

    let mid = vec![0u8; 0x4c];
    let mut out = Vec::new();
    push_data(&mut out, &mid);
    assert_eq!(out[0], 0x4c);
    assert_eq!(out[1], 0x4c);
    assert_eq!(&out[2..], mid.as_slice());

    let long = vec![0x11u8; 300];
    let mut out2 = Vec::new();
    push_data(&mut out2, &long);
    assert_eq!(out2[0], 0x4d);
    assert_eq!(u16::from_le_bytes([out2[1], out2[2]]), 300);
    assert_eq!(&out2[3..], long.as_slice());
}

#[test]
fn fetch_and_clear_pushdata1_and_pushdata2() {
    // OP_RETURN + PUSHDATA1(header||payload)
    let mut payload = SIGNET_HEADER.to_vec();
    payload.extend_from_slice(&[0xde, 0xad]);
    let mut spk = vec![0x6a, 0x4c, payload.len() as u8];
    spk.extend_from_slice(&payload);
    let (sol, stripped) = fetch_and_clear_signet_section(&spk).unwrap();
    assert_eq!(sol, vec![0xde, 0xad]);
    assert_eq!(stripped[0], 0x6a);

    // PUSHDATA2 path
    let mut payload2 = SIGNET_HEADER.to_vec();
    payload2.push(0xbe);
    let mut spk2 = vec![0x6a, 0x4d];
    spk2.extend_from_slice(&(payload2.len() as u16).to_le_bytes());
    spk2.extend_from_slice(&payload2);
    let (sol2, _) = fetch_and_clear_signet_section(&spk2).unwrap();
    assert_eq!(sol2, vec![0xbe]);
    // A byte after the push must survive. `pc *= n` skips it.
    spk2.push(0x51);
    let (sol2, repl2) = fetch_and_clear_signet_section(&spk2).unwrap();
    assert_eq!(sol2, vec![0xbe]);
    assert!(repl2.ends_with(&[0x51]), "{repl2:?}");

    // Truncated push → None
    assert!(fetch_and_clear_signet_section(&[0x05, 0x01]).is_none());
    assert!(fetch_and_clear_signet_section(&[0x4c, 0x05, 0x01]).is_none());
    assert!(fetch_and_clear_signet_section(&[0x4d, 0x05, 0x00, 0x01]).is_none());
    // OP_0 pass-through without solution
    assert!(fetch_and_clear_signet_section(&[0x00, 0x51]).is_none());
    // Non-header push retained
    let no_hdr = vec![0x03, 0xaa, 0xbb, 0xcc];
    assert!(fetch_and_clear_signet_section(&no_hdr).is_none());
    let _ = no_hdr;
}

#[test]
#[allow(clippy::cognitive_complexity)] // one parser, many length boundaries
fn compact_size_and_solution_parse_errors() {
    use rbitcoin_primitives::read_compact_size_from;
    assert_eq!(read_compact_size_from(&mut (&[0u8][..])).unwrap(), 0);
    assert_eq!(read_compact_size_from(&mut (&[252u8][..])).unwrap(), 252);
    // 253 + u16 LE
    let mut r = &[253u8, 0x01, 0x00][..];
    assert_eq!(read_compact_size_from(&mut r).unwrap(), 1);
    // 254 + u32
    let mut r = &[254u8, 0x02, 0x00, 0x00, 0x00][..];
    assert_eq!(read_compact_size_from(&mut r).unwrap(), 2);
    // 255 + u64
    let mut r = &[255u8, 0x03, 0, 0, 0, 0, 0, 0, 0][..];
    assert_eq!(read_compact_size_from(&mut r).unwrap(), 3);

    assert!(read_compact_size_from(&mut (&[][..])).is_err());
    assert!(read_compact_size_from(&mut (&[253u8][..])).is_err());
    assert!(read_compact_size_from(&mut (&[254u8, 0][..])).is_err());
    assert!(read_compact_size_from(&mut (&[255u8, 0, 0, 0][..])).is_err());

    // A push that is only the header has no payload (`len > header`, not `>=`).
    let mut header_only = vec![SIGNET_HEADER.len() as u8];
    header_only.extend_from_slice(&SIGNET_HEADER);
    assert!(fetch_and_clear_signet_section(&header_only).is_none());
    // Script bytes that end the buffer are not "short"; the witness count is.
    let err = parse_signet_solution(&[0x01, 0xaa]).unwrap_err();
    assert!(!err.to_string().contains("scriptSig short"), "{err}");
    // A witness item that fills the buffer is complete.
    let (ss, wit) = parse_signet_solution(&[0x00, 0x01, 0x01, 0xaa]).unwrap();
    assert!(ss.is_empty());
    assert_eq!(wit.to_vec(), vec![vec![0xaa]]);
    // Empty solution: scriptSig compact 0 + witness count 0.
    let (ss, wit) = parse_signet_solution(&[0x00, 0x00]).unwrap();
    assert!(ss.is_empty());
    assert_eq!(wit.len(), 0);
    // Truncated CompactSize in the solution.
    assert!(parse_signet_solution(&[253]).is_err());
    // Extraneous tail.
    assert!(parse_signet_solution(&[0x00, 0x00, 0xff]).is_err());
    // Short script.
    assert!(parse_signet_solution(&[0x02, 0xaa]).is_err());
    // Short witness item.
    assert!(parse_signet_solution(&[0x00, 0x01, 0x02, 0xaa]).is_err());
    // Non-minimal witness count 0 (253, 0x0000).
    let err = parse_signet_solution(&[0x00, 253, 0x00, 0x00]).unwrap_err();
    assert!(err.to_string().contains("non-minimal"), "{err}");
    // 253 is the smallest minimal 3-byte encoding. `v <= 253` rejects it.
    let mut at_253 = vec![253, 253, 0];
    at_253.extend(vec![0x11u8; 253]);
    at_253.push(0);
    let (ss, wit) = parse_signet_solution(&at_253).unwrap();
    assert_eq!(ss.len(), 253);
    assert_eq!(wit.len(), 0);
    // u16::MAX fits in 3 bytes, so a 5-byte form is non-minimal.
    let err = parse_signet_solution(&[254, 0xff, 0xff, 0x00, 0x00]).unwrap_err();
    assert!(err.to_string().contains("non-minimal"), "{err}");
    let err = parse_signet_solution(&[254, 0x00, 0x00, 0x00]).unwrap_err();
    assert!(err.to_string().contains("compact size"), "{err}");
    let err = parse_signet_solution(&[255, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]).unwrap_err();
    assert!(err.to_string().contains("non-minimal"), "{err}");
    let err = parse_signet_solution(&[255, 0, 0, 0, 0, 0, 0, 0]).unwrap_err();
    assert!(err.to_string().contains("compact size"), "{err}");
    // Huge count must fail before allocating the stack.
    let mut huge = vec![0x00, 255];
    huge.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(parse_signet_solution(&huge).is_err());
}

#[test]
fn signet_section_pushdata4_and_minimal_reencode() {
    let mut script = vec![0x4e, 8, 0, 0, 0];
    script.extend_from_slice(&SIGNET_HEADER);
    script.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
    let (sol, repl) = fetch_and_clear_signet_section(&script).expect("pushdata4");
    assert_eq!(sol, vec![0xaa, 0xbb, 0xcc, 0xdd]);
    assert_eq!(&repl[..5], &[4, 0xec, 0xc7, 0xda, 0xa2]);
    // Four single-byte opcodes put pc at 5, so `pc * k` is not `pc + k`.
    let mut shifted = vec![0x51, 0x51, 0x51, 0x51, 0x4e, 5, 0, 0, 0];
    shifted.extend_from_slice(&SIGNET_HEADER);
    shifted.push(0x7e);
    // After the payload. `pc *= n` skips this opcode.
    shifted.push(0x52);
    let (sol, repl) = fetch_and_clear_signet_section(&shifted).expect("shifted pushdata4");
    assert_eq!(sol, vec![0x7e]);
    assert_eq!(repl.iter().filter(|b| **b == 0x51).count(), 4);
    assert!(repl.ends_with(&[0x52]), "{repl:?}");
    // Exactly three length bytes: `pc + 3 == len` must not read past the script.
    assert!(fetch_and_clear_signet_section(&[0x51, 0x51, 0x51, 0x51, 0x4e, 0, 0, 0]).is_none());
    // Non-minimal OP_PUSHDATA1 of one byte re-encodes as a direct push.
    let (sol, repl) = fetch_and_clear_signet_section(&[
        0x4c, 8, 0xec, 0xc7, 0xda, 0xa2, 1, 2, 3, 4, 0x4c, 1, 0xab,
    ])
    .expect("pushdata1");
    assert_eq!(sol, vec![1, 2, 3, 4]);
    assert!(repl.ends_with(&[1, 0xab]), "{repl:?}");
    // Truncation after a parsed section keeps the solution.
    let mut partial = vec![0x05];
    partial.extend_from_slice(&SIGNET_HEADER);
    partial.push(0x11);
    partial.push(0x4c);
    let (sol, _) = fetch_and_clear_signet_section(&partial).expect("truncated tail");
    assert_eq!(sol, vec![0x11]);
}

#[test]
fn signet_section_reencodes_pushes_like_core_cscript() {
    // Core FetchAndClearCommitmentSection: an empty push (OP_0 or a zero-length
    // PUSHDATA1/2/4) is re-emitted as its bare opcode; a non-empty push goes
    // through CScript `<<` (direct, PUSHDATA1, PUSHDATA2, or PUSHDATA4).
    let mut spk = vec![0x6a, 0x4c, 0x00, 0x05];
    spk.extend_from_slice(&SIGNET_HEADER);
    spk.extend_from_slice(&[0x11, 0x4d, 0x00, 0x00, 0x4e, 0x00, 0x00, 0x00, 0x00, 0x00]);
    let (sol, repl) = fetch_and_clear_signet_section(&spk).expect("section");
    assert_eq!(sol, vec![0x11]);
    let mut want = vec![0x6a, 0x4c, 0x04];
    want.extend_from_slice(&SIGNET_HEADER);
    want.extend_from_slice(&[0x4d, 0x4e, 0x00]);
    assert_eq!(repl, want);

    let big = vec![0x22u8; 0x1_0000];
    let mut spk = vec![0x05];
    spk.extend_from_slice(&SIGNET_HEADER);
    spk.push(0x11);
    spk.extend_from_slice(&[0x4e, 0x00, 0x00, 0x01, 0x00]);
    spk.extend_from_slice(&big);
    let (_, repl) = fetch_and_clear_signet_section(&spk).expect("section");
    let mut want = vec![0x04];
    want.extend_from_slice(&SIGNET_HEADER);
    want.extend_from_slice(&[0x4e, 0x00, 0x00, 0x01, 0x00]);
    want.extend_from_slice(&big);
    assert_eq!(repl, want);
}

#[test]
fn modified_merkle_odd_leaf_count() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let block: Block = deserialize(raw).unwrap();
    // Two non-coinbase leaves force at least one pairing; invent a second dummy tx leaf
    // by cloning block with an extra empty-ish tx for merkle shape.
    let cb = block.txdata[0].clone();
    let root = modified_merkle_root(&cb, &block).unwrap();
    assert_ne!(root.to_byte_array(), [0u8; 32]);
    if block.txdata.len() > 1 {
        assert_ne!(root.to_byte_array(), cb.compute_txid().to_byte_array());
    }
    // Odd: only coinbase
    let solo = Block {
        header: block.header,
        txdata: vec![cb.clone()],
    };
    let r2 = modified_merkle_root(&cb, &solo).unwrap();
    assert_eq!(r2.to_byte_array(), cb.compute_txid().to_byte_array());
    let _ = cb;
}

#[test]
fn witness_commitment_index_finds_magic() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let block: Block = deserialize(raw).unwrap();
    assert!(witness_commitment_index(&block.txdata[0]).is_some());
}

fn bip141_commitment_spk(hash_byte: u8) -> Vec<u8> {
    let mut v = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    v.extend(std::iter::repeat_n(hash_byte, 32));
    v
}

fn coinbase_outputs(spks: Vec<Vec<u8>>) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version::ONE,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from_bytes(vec![0x00, 0x00]),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: spks
            .into_iter()
            .map(|script_pubkey| TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::from_bytes(script_pubkey),
            })
            .collect(),
    }
}

#[test]
fn witness_commitment_index_last_exact_38_byte() {
    let first = bip141_commitment_spk(0x00);
    let last = bip141_commitment_spk(0x11);
    let tx = coinbase_outputs(vec![first, last]);
    assert_eq!(witness_commitment_index(&tx), Some(1));

    let substring = vec![0x6a, 0x04, 0xaa, 0x21, 0xa9, 0xed];
    let tx = coinbase_outputs(vec![substring, bip141_commitment_spk(0x22)]);
    assert_eq!(witness_commitment_index(&tx), Some(1));

    let only_sub = coinbase_outputs(vec![vec![0x6a, 0x04, 0xaa, 0x21, 0xa9, 0xed]]);
    assert!(witness_commitment_index(&only_sub).is_none());
}

fn challenge_pair(spk: ScriptBuf) -> (Transaction, Transaction) {
    let to_spend = Transaction {
        version: bitcoin::transaction::Version::non_standard(0),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: spk,
        }],
    };
    let txid = to_spend.compute_txid();
    let to_sign = Transaction {
        version: bitcoin::transaction::Version::non_standard(0),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid, vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ZERO,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::from_bytes(vec![0x6a]),
        }],
    };
    (to_spend, to_sign)
}

#[test]
fn signet_challenge_op_true_twice_is_not_cleanstack() {
    let challenge = ScriptBuf::from_bytes(vec![0x51, 0x51]);
    let (to_spend, to_sign) = challenge_pair(challenge.clone());
    verify_challenge_spend(&to_spend, &to_sign, challenge.as_script()).unwrap();
}

#[test]
fn signet_challenge_p2wpkh_empty_witness_rejected() {
    let mut spk = vec![0x00, 0x14];
    spk.extend([0u8; 20]);
    let challenge = ScriptBuf::from_bytes(spk);
    let (to_spend, to_sign) = challenge_pair(challenge.clone());
    assert!(verify_challenge_spend(&to_spend, &to_sign, challenge.as_script()).is_err());
}

#[test]
fn push_data_mid_and_unknown_op_passthrough() {
    // push_data uses PUSHDATA2 for 0xff < len <= 0xffff.
    let mid = vec![0u8; 300];
    let mut out = Vec::new();
    push_data(&mut out, &mid);
    assert_eq!(out[0], 0x4d);
    assert_eq!(u16::from_le_bytes([out[1], out[2]]), 300);
    // Truncated PUSHDATA4 stops the scan (no solution).
    assert!(fetch_and_clear_signet_section(&[0x4e, 0x05, 0x00, 0x00, 0x00, 0x01]).is_none());
    // OP_RETURN + OP_TRUE: no signet header → None.
    assert!(fetch_and_clear_signet_section(&[0x6a, 0x51]).is_none());
}

#[test]
fn modified_merkle_three_leaves() {
    let raw = include_bytes!("../tests/fixtures/signet_block_1.bin");
    let block: Block = deserialize(raw).unwrap();
    let cb = block.txdata[0].clone();
    // Build block with coinbase + 2 dummy non-cb (odd non-cb count after strip).
    let dummy = |n: u8| Transaction {
        version: bitcoin::transaction::Version::ONE,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([n; 32]),
                vout: 0,
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
    let multi = Block {
        header: block.header,
        txdata: vec![cb.clone(), dummy(1), dummy(2)],
    };
    let root = modified_merkle_root(&cb, &multi).unwrap();
    assert_ne!(root.to_byte_array(), cb.compute_txid().to_byte_array());
    let _ = cb;
}
