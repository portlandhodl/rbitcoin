use bitcoin::block::Block;
use bitcoin::hashes::Hash;
use bitcoin::ScriptBuf;
use bitcoin::{Amount, OutPoint, Transaction, TxOut};
use rbitcoin_consensus::block::{
    JobPrevouts, PendingCreates, ScriptCheckJob, ScriptVerifyFlags, TxidMap,
};
use rbitcoin_consensus::ChainParams;
use rbitcoin_primitives::Fk;
use rbitcoin_primitives::Height;
use rbitcoin_query::block_pipeline::RunCreateHeight;
use rbitcoin_query::{ConfirmError, FkMap, Query, U32Map, U64Map};
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::time::Instant;

fn fk(id: u64) -> Fk {
    Fk(id)
}

#[test]
fn run_create_height_span_misses_gaps() {
    let one = [fk(10), fk(11)];
    let idx = RunCreateHeight::from_blocks([(7u32, one.as_slice())]);
    assert!(matches!(idx, RunCreateHeight::Spans(_)));
    assert_eq!(idx.get(fk(10)), Some(7));
    assert_eq!(idx.get(fk(11)), Some(7));
    assert_eq!(idx.get(fk(9)), None);
    assert_eq!(idx.get(fk(12)), None);

    let low = [fk(10), fk(11)];
    let high = [fk(20), fk(21)];
    let idx = RunCreateHeight::from_blocks([(1u32, low.as_slice()), (2, high.as_slice())]);
    assert_eq!(idx.get(fk(15)), None);
    assert_eq!(idx.get(fk(10)), Some(1));
    assert_eq!(idx.get(fk(21)), Some(2));

    let gapped = [fk(1), fk(2), fk(4)];
    let idx = RunCreateHeight::from_blocks([(3u32, gapped.as_slice())]);
    assert!(matches!(idx, RunCreateHeight::Map(_)));
    assert_eq!(idx.get(fk(3)), None);
    assert_eq!(idx.get(fk(1)), Some(3));
    assert_eq!(idx.get(fk(2)), Some(3));
    assert_eq!(idx.get(fk(4)), Some(3));
}

#[test]
fn run_create_height_marks_each_block_first_fk_coinbase() {
    let low = [fk(10), fk(11)];
    let high = [fk(12), fk(13)];
    let idx = RunCreateHeight::from_blocks([(1u32, low.as_slice()), (2, high.as_slice())]);
    assert!(matches!(idx, RunCreateHeight::Spans(_)));
    assert_eq!(idx.create(fk(10)), Some((1, true)));
    assert_eq!(idx.create(fk(11)), Some((1, false)));
    assert_eq!(idx.create(fk(12)), Some((2, true)));
    assert_eq!(idx.create(fk(13)), Some((2, false)));

    let gapped = [fk(20), fk(22)];
    let idx = RunCreateHeight::from_blocks([(1u32, low.as_slice()), (2, gapped.as_slice())]);
    assert!(matches!(idx, RunCreateHeight::Map(_)));
    assert_eq!(idx.create(fk(10)), Some((1, true)));
    assert_eq!(idx.create(fk(11)), Some((1, false)));
    assert_eq!(idx.create(fk(20)), Some((2, true)));
    assert_eq!(idx.create(fk(22)), Some((2, false)));
    assert_eq!(idx.create(fk(21)), None);
}
