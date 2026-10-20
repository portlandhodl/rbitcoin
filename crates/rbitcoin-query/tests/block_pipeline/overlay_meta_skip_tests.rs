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
use rbitcoin_query::block_pipeline::*;
use rbitcoin_query::{ConfirmError, FkMap, Query, U32Map, U64Map};
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::time::Instant;

fn spends(rows: &[(u32, Fk, Fk, u32)]) -> Vec<([u8; 32], u32, Fk, Fk, u32)> {
    rows.iter()
        .map(|&(vout, sfk, cfk, vin)| ([0u8; 32], vout, sfk, cfk, vin))
        .collect()
}

#[test]
fn overlay_meta_skip_omits_matching_abs() {
    let wave = ClassAWave::new(vec![Fk(10)]);
    let mut map = FkMap::default();
    map.insert(Fk(10), (5, false));
    let run = RunCreateHeight::Map(map);
    assert_eq!(run.create(Fk(10)), Some((5, false)));
    let spends = spends(&[(0, Fk(11), Fk(10), 0)]);
    let mut scratch = StructuralScratch::default();
    fill_overlay_skip(&spends, &wave, &mut scratch);
    assert!(overlay_meta_is_skip(10, 0, Fk(11), 0, &scratch.skip));
    assert!(!overlay_meta_is_skip(10, 0, Fk(11), 1, &scratch.skip));
    assert!(!overlay_meta_is_skip(10, 1, Fk(11), 0, &scratch.skip));
    assert!(!overlay_meta_is_skip(99, 0, Fk(11), 0, &scratch.skip));
}

#[test]
fn overlay_meta_skip_keeps_conflicting_spender_on_disk_list() {
    let wave = ClassAWave::new(vec![Fk(10)]);
    let mut map = FkMap::default();
    map.insert(Fk(10), (5, false));
    let run = RunCreateHeight::Map(map);
    assert_eq!(run.create(Fk(10)), Some((5, false)));
    let spends = spends(&[(0, Fk(11), Fk(10), 0), (0, Fk(12), Fk(10), 0)]);
    let mut scratch = StructuralScratch::default();
    fill_overlay_skip(&spends, &wave, &mut scratch);
    assert!(!overlay_meta_is_skip(10, 0, Fk(11), 0, &scratch.skip));
    assert!(!overlay_meta_is_skip(10, 0, Fk(12), 0, &scratch.skip));
}

#[test]
fn overlay_meta_skip_historical_create_stays_on_disk() {
    let wave = ClassAWave::default();
    let spends = spends(&[(0, Fk(11), Fk(10), 0)]);
    let mut scratch = StructuralScratch::default();
    fill_overlay_skip(&spends, &wave, &mut scratch);
    assert!(!overlay_meta_is_skip(10, 0, Fk(11), 0, &scratch.skip));
}
