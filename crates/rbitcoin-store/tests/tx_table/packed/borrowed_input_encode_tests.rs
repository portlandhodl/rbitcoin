use rbitcoin_primitives::Fk;
use rbitcoin_primitives::{read_compact_size, write_compact_size};
use rbitcoin_store::tx_table::packed::*;
use rbitcoin_store::tx_table::*;

fn witness_refs(items: &[Vec<u8>]) -> Vec<&[u8]> {
    items.iter().map(|i| i.as_slice()).collect()
}

fn assert_matches(rec: &InputRecord, secret: Option<&rbitcoin_store::store_secret::StoreSecret>) {
    let wit = witness_refs(&rec.witness);
    let mut owned = Vec::new();
    encode_seqsigwit_with_secret(std::slice::from_ref(rec), &mut owned, secret);
    let mut from_wire = Vec::new();
    encode_wire_seqsigwit_input(
        rec.sequence,
        rec.script_sig.as_slice(),
        wit.iter().copied(),
        &mut from_wire,
        secret,
    );
    assert_eq!(owned, from_wire);
    let upper = seqsigwit_input_len_upper(rec.script_sig.len(), wit.iter().map(|i| i.len()));
    assert!(upper >= rec.encoded_len_exact());
    assert_eq!(upper, rec.encoded_len());
}

#[test]
fn borrowed_input_encode_matches_record() {
    let secret = rbitcoin_store::store_secret::StoreSecret::from_bytes([0x5au8; 32]);
    let cases = [
        InputRecord::coinbase(u32::MAX, vec![0x01, 0x02], vec![]),
        InputRecord::coinbase(1, vec![], vec![vec![0x00; 32]]),
        InputRecord {
            prev_txid: [0u8; 32],
            parent: InputParent::Prevout {
                create_fk: Fk(9),
                vout: 3,
            },
            sequence: 0xffff_fffe,
            script_sig: vec![0xab; 40],
            witness: vec![],
        },
        InputRecord {
            prev_txid: [0u8; 32],
            parent: InputParent::Prevout {
                create_fk: Fk(9),
                vout: 1,
            },
            sequence: u32::MAX,
            script_sig: vec![],
            witness: vec![vec![0x30; 70], vec![0x21; 33]],
        },
        InputRecord {
            prev_txid: [0u8; 32],
            parent: InputParent::Prevout {
                create_fk: Fk(4),
                vout: 0,
            },
            sequence: 7,
            script_sig: vec![0x16, 0x00, 0x14],
            witness: vec![vec![0x30; 71], vec![0x21; 33], vec![0x51]],
        },
    ];
    for rec in &cases {
        assert_matches(rec, None);
        assert_matches(rec, Some(&secret));
    }
}
