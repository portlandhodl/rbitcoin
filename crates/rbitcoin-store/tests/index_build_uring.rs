use rbitcoin_primitives::{Fk, Height};
use rbitcoin_store::create_loc::CreateLocPair;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::index_build_uring::{
    read_index_window, require_body_hwm, IndexHeight, IndexWindow, ReadJob,
};
use rbitcoin_store::input::{decode_edges_span, EdgesPlan, InputEdge};
use rbitcoin_store::io_handle::IoHandle;
use rbitcoin_store::sp_tweaks_uring::LoadedTweakTx;
use rbitcoin_store::tx_table::{
    apply_input_edges, decode_packed_tx_outs_with_spender_rels_secret, decode_seqsigwit_secret,
    OutputRecord, TxTable,
};
use rbitcoin_store::txid_body::{TxidBody, TXID_ENTRY_LEN};
use rbitcoin_store::uring_session::{self, UringSession};
use rbitcoin_store::U64Map;
use std::path::Path;

use rbitcoin_store::tx_table::{InputRecord, TxRecord};

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-index-hwm-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn index_read_above_the_cap_is_refused() {
    let dir = tmp("read-cap");
    let file = std::fs::File::create(dir.join("body")).unwrap();
    let handle = rbitcoin_store::io_handle::IoHandle::from_file(&file);
    let len = rbitcoin_store::var_table::VarTable::BODY_READ_MAX + 1;
    match ReadJob::new((handle, dir.as_path()), 0, len) {
        Err(StoreError::Rejected(msg)) => assert!(msg.contains("too large"), "{msg}"),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("above the body read cap allocated"),
    }
}

fn tx(mark: u8) -> TxRecord {
    let mut txid = [0u8; 32];
    txid[0] = mark;
    TxRecord {
        txid,
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 1,
        output_start_fk: Fk::NULL,
        output_count: 1,
    }
}

fn spend(parent: Fk) -> InputRecord {
    InputRecord {
        prev_txid: [0u8; 32],
        parent: rbitcoin_store::InputParent::Prevout {
            create_fk: parent,
            vout: 0,
        },
        sequence: u32::MAX,
        script_sig: vec![1],
        witness: vec![],
    }
}

/// Child fk 1 spends parent fk 2, and the parent body is written second.
fn child_then_parent(script: Vec<u8>) -> (std::path::PathBuf, TxTable) {
    let dir = tmp("window");
    let t = TxTable::create_tiny(&dir).unwrap();
    let fks = t
        .put_full_batch_indexed(
            &[
                (
                    tx(1),
                    vec![spend(Fk(2))],
                    vec![OutputRecord::unspent(1, script)],
                ),
                (
                    tx(2),
                    vec![InputRecord::coinbase(u32::MAX, vec![2], vec![])],
                    vec![OutputRecord::unspent(50, vec![0x51])],
                ),
            ],
            true,
        )
        .unwrap();
    assert_eq!(fks, vec![Fk(1), Fk(2)]);
    (dir, t)
}

fn one_child(tweaks: bool) -> [IndexHeight; 1] {
    [IndexHeight {
        height: Height(1),
        header_fk: Fk(1),
        first: Fk(1),
        n: 1,
        tweaks,
    }]
}

fn expect_published(err: Result<IndexWindow, StoreError>) {
    match err {
        Err(StoreError::Corrupt(msg)) => assert!(msg.contains("published"), "{msg}"),
        Ok(_) => panic!("a span past the published end was read"),
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn txout_span_past_the_body_hwm_is_corrupt() {
    require_body_hwm(80, 80).unwrap();
    match require_body_hwm(80, 81) {
        Err(StoreError::Corrupt(msg)) => assert!(msg.contains("published"), "{msg}"),
        Ok(()) => panic!("a span past the body HWM was accepted"),
        Err(other) => panic!("{other}"),
    }
}

#[test]
fn parent_txout_past_the_published_end_is_corrupt() {
    let (dir, t) = child_then_parent(vec![0x51]);
    let locs = t.create_loc.range_batch(&[Fk(1), Fk(2)]).unwrap();
    let child = locs[0].unwrap();
    let parent = locs[1].unwrap();
    let child_end = child.txout.0 + child.txout.1;
    assert!(
        parent.txout.0 + parent.txout.1 > child_end,
        "the parent txout must sit past the child"
    );
    t.body.truncate_body_to(2, child_end).unwrap();
    expect_published(read_index_window(&t, &one_child(false)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seqsigwit_past_the_published_end_is_corrupt() {
    let mut script = vec![0x51, 0x20];
    script.extend_from_slice(&[0xab; 32]);
    let (dir, t) = child_then_parent(script);
    let range = t.seqsigwit_loc.range_batch(&[Fk(1)]).unwrap()[0].unwrap();
    assert!(range.1 > 0, "the child seqsigwit span is non-empty");
    t.seqsigwit.truncate_body_to(2, range.0).unwrap();
    expect_published(read_index_window(&t, &one_child(true)));
    let _ = std::fs::remove_dir_all(&dir);
}
