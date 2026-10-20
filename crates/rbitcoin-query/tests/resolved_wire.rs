use bitcoin::Block;
use rbitcoin_query::resolved_wire::*;
use rbitcoin_query::TxPrecompute;
use std::sync::Arc;

#[test]
fn resolved_wire_stamps_n_inputs() {
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    let n = genesis
        .txdata
        .iter()
        .map(|tx| tx.input.len() as u32)
        .fold(0u32, u32::saturating_add);
    let pres: Arc<[TxPrecompute]> = genesis
        .txdata
        .iter()
        .map(TxPrecompute::from_tx)
        .collect::<Vec<_>>()
        .into();
    let wire = ResolvedWire::new(Arc::new(genesis), pres);
    assert_eq!(wire.n_inputs, n);
    assert_eq!(n, 1);
    let stamped = ResolvedWire {
        block: Arc::clone(&wire.block),
        pres: Arc::clone(&wire.pres),
        n_inputs: 9,
        header_fk: 7,
        spend_keys: Arc::from([([0x11u8; 32], 3)]),
        sender: None,
    };
    assert_eq!(stamped.n_inputs, 9);
    assert_eq!(stamped.header_fk, 7);
    assert_eq!(wire.header_fk, 0);
    assert_eq!(stamped.spend_keys.as_ref(), &[([0x11u8; 32], 3)]);
}
