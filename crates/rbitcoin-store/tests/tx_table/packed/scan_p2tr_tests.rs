use rbitcoin_primitives::write_uleb128;
use rbitcoin_primitives::Fk;
use rbitcoin_primitives::{read_compact_size, write_compact_size};
use rbitcoin_store::compact::{amount_exp_mantissa, SCRIPT_KIND_V17_P2TR};
use rbitcoin_store::tx_table::packed::*;
use rbitcoin_store::tx_table::*;

fn packed_p2tr_body(value: u64) -> Vec<u8> {
    let meta = TxRecord {
        txid: [0u8; 32],
        version: 2,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 0,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let mut raw = Vec::new();
    meta.encode_body_meta_into(&mut raw);
    if value > i64::MAX as u64 {
        raw.push(SCRIPT_KIND_V17_P2TR);
        write_uleb128(&mut raw, value);
    } else {
        let (exp, mantissa) = amount_exp_mantissa(value);
        raw.push(SCRIPT_KIND_V17_P2TR | (exp << 4));
        write_uleb128(&mut raw, mantissa);
    }
    raw.extend_from_slice(&[0u8; 32]);
    raw
}

#[test]
fn scan_packed_p2tr_outs_ok_value() {
    let raw = packed_p2tr_body(50_000);
    let rows = scan_packed_p2tr_outs(&raw, 1, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 0);
    assert_eq!(rows[0].2, 50_000);
    OutputRecord::decode_at_secret(&raw[TxRecord::decode_body_meta(&raw).unwrap().1..], None)
        .unwrap();
}

#[test]
fn scan_packed_p2tr_outs_exp_mantissa_is_sats() {
    let meta = TxRecord {
        txid: [0u8; 32],
        version: 2,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 0,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let mut raw = Vec::new();
    meta.encode_body_meta_into(&mut raw);
    raw.push(SCRIPT_KIND_V17_P2TR | (8 << 4));
    raw.push(1);
    raw.extend_from_slice(&[0u8; 32]);
    let rows = scan_packed_p2tr_outs(&raw, 1, None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, 100_000_000);
}

#[test]
fn scan_packed_p2tr_outs_overflow_is_corrupt() {
    let raw = packed_p2tr_body(i64::MAX as u64 + 1);
    let err = scan_packed_p2tr_outs(&raw, 1, None).unwrap_err();
    assert!(format!("{err}").contains("output value too large"), "{err}");
    let meta_n = TxRecord::decode_body_meta(&raw).unwrap().1;
    let dec = OutputRecord::decode_at_secret(&raw[meta_n..], None).unwrap_err();
    assert!(format!("{dec}").contains("output value too large"), "{dec}");
    let skip = OutputRecord::skip_at(&raw[meta_n..]).unwrap_err();
    assert!(
        format!("{skip}").contains("output value too large"),
        "{skip}"
    );
    let visit = visit_packed_script_hashes(&raw, 1, None, |_| Ok(())).unwrap_err();
    assert!(
        format!("{visit}").contains("output value too large"),
        "{visit}"
    );
}

#[test]
fn decode_seqsigwit_secret_zero_count_walks_payload() {
    let ins = vec![InputRecord::coinbase(u32::MAX, vec![0x01, 0x02], vec![])];
    let mut raw = Vec::new();
    encode_seqsigwit_with_secret(&ins, &mut raw, None);
    let got = decode_seqsigwit_secret(&raw, 0, None).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].script_sig, vec![0x01, 0x02]);
    assert!(!got[0].is_coinbase());
}

fn three_out_packed() -> (Vec<u8>, usize) {
    let tx = TxRecord {
        txid: [0x11u8; 32],
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 3,
    };
    let inputs = vec![InputRecord::coinbase(u32::MAX, vec![0x01], vec![])];
    let outputs = vec![
        OutputRecord::unspent(1, vec![0x51]),
        OutputRecord::unspent(2, vec![0x52; 80]),
        OutputRecord::unspent(3, vec![0x53; 80]),
    ];
    let mut raw = Vec::new();
    encode_packed_tx(&tx, &inputs, &outputs, &mut raw);
    let (_, mut off) = TxRecord::decode_body_meta(&raw).unwrap();
    off += OutputRecord::skip_at(&raw[off..]).unwrap();
    (raw, off)
}

#[test]
fn decode_packed_tx_need_outs_truncated_after_last_need() {
    let (raw, after_vout0) = three_out_packed();
    let truncated = &raw[..after_vout0];
    let (meta, live, sparse) =
        decode_packed_tx_need_outs_with_spender_rels_secret(truncated, 3, &[0], None).unwrap();
    assert_eq!(meta.output_count, 3);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].0, 0);
    assert_eq!(live[0].1.script, vec![0x51]);
    assert_eq!(sparse.len(), 1);
    assert_eq!(sparse[0].0, 0);
    let empty_need = decode_packed_tx_need_outs_with_spender_rels_secret(truncated, 3, &[], None);
    assert!(
        empty_need.is_err(),
        "empty need still requires a full outs walk"
    );
    assert!(txout_first_page_covers_need(truncated, 3, &[0]));
    assert!(!txout_first_page_covers_need(truncated, 3, &[]));
}

#[test]
fn decode_packed_tx_need_outs_empty_need_still_pad_checks() {
    let (mut raw, _) = three_out_packed();
    raw.push(0x01);
    let err = decode_packed_tx_need_outs_with_spender_rels_secret(&raw, 3, &[], None).unwrap_err();
    assert!(format!("{err}").contains("trailing non-zero"), "{err}");
    let (meta, live, _) =
        decode_packed_tx_need_outs_with_spender_rels_secret(&raw[..raw.len() - 1], 3, &[0], None)
            .unwrap();
    assert_eq!(meta.output_count, 3);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].0, 0);
}

#[test]
fn encode_unspent_into_matches_output_record() {
    let rec = OutputRecord::unspent(50_0000_0000, vec![0x00, 0x14, 0xaa]);
    let mut from_rec = Vec::new();
    rec.encode_into(&mut from_rec);
    let mut from_parts = Vec::new();
    OutputRecord::encode_unspent_into(rec.value, &rec.script, &mut from_parts);
    assert_eq!(from_rec, from_parts);
    let mut via_secret = Vec::new();
    encode_unspent_output_into_secret(rec.value, &rec.script, &mut via_secret, None);
    assert_eq!(from_rec, via_secret);
}
