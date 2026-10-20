use rbitcoin_primitives::Fk;
use rbitcoin_store::address_head::HeadLayout;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::io_handle::IoHandle;
use rbitcoin_store::sp_tweaks_uring::*;
use rbitcoin_store::tx_table::{
    decode_packed_tx_outs_with_spender_rels_secret, decode_seqsigwit_secret, InputRecord,
    OutputRecord, TxRecord, TxTable,
};
use rbitcoin_store::uring_session::{self, UringSession};
use rbitcoin_store::U64Map;
use std::path::Path;

fn tmp_dir() -> rbitcoin_store::testutil::TempDir {
    rbitcoin_store::testutil::TempDir::labeled("sp-uring").unwrap()
}

fn tiny_table(dir: &std::path::Path) -> TxTable {
    TxTable::create_with_head_layout(
        dir,
        HeadLayout::new(rbitcoin_store::address_head::TINY_BITS).unwrap(),
    )
    .unwrap()
}

#[test]
fn filter_marks_only_p2tr_need_seqsigwit() {
    let dir = tmp_dir();
    let t = tiny_table(&dir);
    let p2tr = {
        let mut s = vec![0x51, 0x20];
        s.extend_from_slice(&[3u8; 32]);
        s
    };
    let op_true = vec![0x51];
    let a = TxRecord {
        txid: [1u8; 32],
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let b = TxRecord {
        txid: [2u8; 32],
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    };
    let ins = vec![InputRecord::coinbase(u32::MAX, vec![0x01], vec![])];
    let fks = t
        .put_full_batch_indexed(
            &[
                (a, ins.clone(), vec![OutputRecord::unspent(1, op_true)]),
                (b, ins, vec![OutputRecord::unspent(2, p2tr)]),
            ],
            true,
        )
        .unwrap();
    let wave = load_tweak_wave(&t, &fks).unwrap();
    assert_eq!(wave.txs.len(), 2);
    assert!(
        !wave.txs[0].need_seqsigwit,
        "OP_TRUE must not load seqsigwit"
    );
    assert!(wave.txs[0].inputs.is_none());
    assert!(wave.txs[1].need_seqsigwit, "P2TR must need seqsigwit");
    assert_eq!(wave.txs[1].rec.input_count, 1);
    assert_eq!(wave.txs[1].inputs.as_ref().map(Vec::len), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}
