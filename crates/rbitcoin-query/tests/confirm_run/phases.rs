use bitcoin::hashes::Hash;
use rbitcoin_query::confirm_run::*;
use rbitcoin_query::FkMap;
use rbitcoin_query::Query;

use bitcoin::blockdata::constants;
use bitcoin::ScriptBuf;
use rbitcoin_consensus::mine_empty_regtest;
use rbitcoin_consensus::{ChainParams, Milestone};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::accept_and_connect_block;
use rbitcoin_query::confirm_run::phases::*;
use rbitcoin_query::TxPrecompute;
use rbitcoin_store::HeaderRecord;
use std::sync::Arc;

#[test]
fn mtp_window_keeps_the_last_eleven_times() {
    let mut window: Vec<u32> = (1..=11).collect();
    trim_mtp_window(&mut window);
    assert_eq!(window, (1..=11).collect::<Vec<_>>());
    window.push(12);
    trim_mtp_window(&mut window);
    assert_eq!(window, (2..=12).collect::<Vec<_>>());
    window.push(13);
    window.push(14);
    trim_mtp_window(&mut window);
    assert_eq!(window, (4..=14).collect::<Vec<_>>());
}

fn tiny() -> (rbitcoin_query::testutil::TempDir, rbitcoin_query::Query) {
    rbitcoin_query::testutil::tiny_query_labeled("phases-pin")
}

#[test]
fn signet_challenge_applies_above_genesis() {
    let (path, q) = tiny();
    let mut params = ChainParams::regtest();
    params.signet_challenge = Some(ScriptBuf::from_bytes(vec![0x51]));
    let genesis = constants::genesis_block(bitcoin::Network::Regtest);
    accept_and_connect_block(&q, &params, Height::GENESIS, &genesis, Milestone::NONE).unwrap();
    let b1 = mine_empty_regtest(genesis.block_hash(), genesis.header.time + 600, 1);
    let block = Arc::new(b1);
    let pres: Arc<[TxPrecompute]> = block
        .txdata
        .iter()
        .map(TxPrecompute::from_tx)
        .collect::<Vec<_>>()
        .into();
    let meta = BodyMeta {
        height: Height(1),
        hash: block.block_hash().to_byte_array(),
        header_fk: Fk(1),
        header_rec: HeaderRecord {
            prev_fk: Fk(1),
            version: block.header.version.to_consensus(),
            timestamp: block.header.time,
            bits: block.header.bits.to_consensus(),
            nonce: block.header.nonce,
            merkle_root: block.header.merkle_root.to_byte_array(),
            hash: block.block_hash().to_byte_array(),
            size: 0,
            weight: 0,
        },
        tx_fks: Vec::new(),
        txids: block
            .txdata
            .iter()
            .map(|tx| tx.compute_txid().to_byte_array())
            .collect(),
        pres,
    };
    let err = match assemble_run(
        &q,
        &params,
        Milestone::NONE,
        vec![meta],
        std::slice::from_ref(&block),
        &rbitcoin_query::BatchParents::new(),
        &rbitcoin_query::SpendEdges::default(),
    ) {
        Ok(_) => panic!("height 1 with a challenge must reject a block that has no solution"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("signet"),
        "height 1 with a challenge must check the solution, got {msg}"
    );
    let _ = std::fs::remove_dir_all(&path);
}
