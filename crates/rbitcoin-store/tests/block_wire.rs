use rbitcoin_primitives::read_compact_size;
use rbitcoin_store::block_wire::*;

fn header() -> Vec<u8> {
    vec![0u8; HEADER_LEN]
}

fn compact(n: u64) -> Vec<u8> {
    let mut o = Vec::new();
    rbitcoin_primitives::write_compact_size(&mut o, n);
    o
}

fn coinbase_tx(n_in: u32) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&compact(u64::from(n_in)));
    for _ in 0..n_in {
        t.extend_from_slice(&[0u8; 32]);
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&compact(2));
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    t.extend_from_slice(&compact(1));
    t.extend_from_slice(&0u64.to_le_bytes());
    t.extend_from_slice(&compact(1));
    t.push(0x51);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

fn witness_tx(n_in: u32) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes());
    t.push(0);
    t.push(1);
    t.extend_from_slice(&compact(u64::from(n_in)));
    for _ in 0..n_in {
        t.extend_from_slice(&[0u8; 32]);
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&compact(0));
        t.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    t.extend_from_slice(&compact(1));
    t.extend_from_slice(&1u64.to_le_bytes());
    t.extend_from_slice(&compact(1));
    t.push(0x51);
    for _ in 0..n_in {
        t.extend_from_slice(&compact(1));
        t.extend_from_slice(&compact(1));
        t.push(0x51);
    }
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

fn block(txs: &[Vec<u8>]) -> Vec<u8> {
    let mut p = header();
    p.extend_from_slice(&compact(txs.len() as u64));
    for t in txs {
        p.extend_from_slice(t);
    }
    p
}

#[test]
fn header_only_and_truncated_are_none() {
    assert_eq!(block_wire_input_count(&[]), None);
    assert_eq!(block_wire_input_count(&header()), None);
    let mut p = block(&[coinbase_tx(1)]);
    p.pop();
    assert_eq!(block_wire_input_count(&p), None);
}

#[test]
fn empty_tx_vector_and_trailing_bytes() {
    assert_eq!(block_wire_input_count(&block(&[])), Some(0));
    let mut p = block(&[coinbase_tx(1)]);
    p.push(0xff);
    assert_eq!(block_wire_input_count(&p), Some(1));
}

#[test]
fn coinbase_and_multi_input() {
    assert_eq!(block_wire_input_count(&block(&[coinbase_tx(1)])), Some(1));
    assert_eq!(
        block_wire_input_count(&block(&[coinbase_tx(1), coinbase_tx(3)])),
        Some(4)
    );
}

#[test]
fn witness_flag_counts_vin_not_stack() {
    assert_eq!(block_wire_input_count(&block(&[witness_tx(2)])), Some(2));
    assert_eq!(
        block_wire_input_count(&block(&[coinbase_tx(1), witness_tx(2)])),
        Some(3)
    );
}

#[test]
fn huge_ntx_compactsize_is_none() {
    let mut p = header();
    p.extend_from_slice(&compact(u64::from(u32::MAX)));
    assert_eq!(block_wire_input_count(&p), None);
}

#[test]
fn flag_zero_tx_is_ten_bytes_without_inputs() {
    let dummy = vec![1, 0, 0, 0, 0x00, 0x00, 0, 0, 0, 0];
    let p = block(&[coinbase_tx(1), dummy.clone(), coinbase_tx(2)]);
    assert_eq!(block_wire_input_count(&p), Some(3));
    let mut short = block(&[dummy]);
    short.pop();
    assert_eq!(block_wire_input_count(&short), None);
}

#[test]
fn refuses_what_rust_bitcoin_refuses() {
    let mut flag2 = witness_tx(1);
    flag2[5] = 2;
    assert_eq!(block_wire_input_count(&block(&[flag2])), None);

    let mut bare = witness_tx(1);
    let lock = bare.len() - 4;
    bare.splice(lock - 3..lock, [0u8]);
    assert_eq!(block_wire_input_count(&block(&[bare])), None);

    let mut wide = header();
    wide.extend_from_slice(&[0xfd, 0x01, 0x00]);
    wide.extend_from_slice(&coinbase_tx(1));
    assert_eq!(block_wire_input_count(&wide), None);
}
