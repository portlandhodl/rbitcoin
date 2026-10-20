use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash, CompactTarget, Target};
use rbitcoin_ibd::ibd::reorg::*;
use rbitcoin_log::{info, warn};
use rbitcoin_net::chain::ChainHub;
use rbitcoin_net::error::NetError;
use rbitcoin_net::most_work::{sum_work, work_better, InvalidHashSet};
use rbitcoin_primitives::{Fk, Height};
use std::collections::{HashMap, HashSet};

fn tmp_hub() -> (rbitcoin_query::testutil::TempDir, ChainHub) {
    rbitcoin_net::chain::tiny_regtest_hub_labeled("ibd-reorg")
}

fn mine(prev: BlockHash, time: u32, height: u32) -> Block {
    rbitcoin_consensus::mine_empty_regtest(prev, time, height)
}

#[test]
fn header_hashes_to_best_ancestor_walks_mid_path() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_060_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    // Side path not confirmed: w1 → w2
    let mut w1 = mine(gen, 1_500_060_101, 1);
    if w1.block_hash() == l1.block_hash() {
        let target = Target::from_compact(w1.header.bits);
        for nonce in 0..u32::MAX {
            w1.header.nonce = nonce;
            if w1.header.validate_pow(target).is_ok() && w1.block_hash() != l1.block_hash() {
                break;
            }
        }
    }
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_060_200, 2);
    hub.ensure_header(&w2.header).unwrap();
    let path = header_hashes_to_best_ancestor(&hub, w2.block_hash()).unwrap();
    assert_eq!(
        path,
        vec![w1.block_hash(), w2.block_hash()],
        "oldest-first mid path to LCA"
    );
    // Already on best chain → empty.
    assert!(header_hashes_to_best_ancestor(&hub, l1.block_hash())
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reorg_state_held_awaiting_need_getdata() {
    let mut st = IbdReorgState::new();
    assert!(st.need_getdata().is_empty());
    let gen = BlockHash::from_byte_array([0x11; 32]);
    let bits = CompactTarget::from_consensus(0x207f_ffff);
    let held = mine(gen, 1_300_000_000, 1);
    let mut need_h = mine(gen, 1_300_000_001, 1);
    if need_h.block_hash() == held.block_hash() {
        let target = Target::from_compact(bits);
        for nonce in 0..u32::MAX {
            need_h.header.nonce = nonce;
            if need_h.header.validate_pow(target).is_ok()
                && need_h.block_hash() != held.block_hash()
            {
                break;
            }
        }
    }
    let need = need_h.block_hash();
    st.register_explore([need], None);
    assert_eq!(st.need_getdata(), vec![need]);
    st.hold_body(need_h);
    assert!(st.need_getdata().is_empty(), "held satisfies need");
    let explore = mine(gen, 1_300_000_050, 1);
    let eh = explore.block_hash();
    st.register_explore([eh], Some(eh));
    assert_eq!(st.need_getdata(), vec![eh]);
    st.hold_body(explore);
    assert!(st.need_getdata().is_empty());
    let mut st_cap = IbdReorgState::new();
    let mut held_keys = Vec::new();
    let mut prev = gen;
    for i in 0u32..(IbdReorgState::HELD_CAP as u32 + 4) {
        let b = mine(prev, 1_300_001_000 + i, 1);
        prev = b.block_hash();
        held_keys.push(b.block_hash());
        st_cap.hold_body(b);
    }
    for k in &held_keys {
        st_cap.register_explore([*k], None);
    }
    let still = held_keys
        .iter()
        .filter(|k| !st_cap.need_getdata().contains(k))
        .count();
    assert_eq!(
        still,
        IbdReorgState::HELD_CAP,
        "held map must stay exactly HELD_CAP after overflow inserts"
    );
    let _ = held;
}

fn distinct_sib(mut b: Block, avoid: BlockHash) -> Block {
    if b.block_hash() == avoid {
        let target = Target::from_compact(b.header.bits);
        for nonce in 0..u32::MAX {
            b.header.nonce = nonce;
            if b.header.validate_pow(target).is_ok() && b.block_hash() != avoid {
                break;
            }
        }
    }
    b
}

/// Tip on a 2-block loser; heavier winner headers do not connect at
/// tip+1. Name the connecting hashes (not a dead-tip child).
#[test]
fn heavier_disconnected_path_names_connecting_hashes() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_060_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_060_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    assert_eq!(hub.tip_hash().unwrap(), l2.block_hash());

    let w1 = distinct_sib(mine(gen, 1_500_060_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_060_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_060_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_060_401, 4);
    hub.ensure_header(&w4.header).unwrap();

    // Connected loser child is not a disconnected heavier path.
    let l3 = mine(l2.block_hash(), 1_500_060_300, 3);
    hub.ensure_header(&l3.header).unwrap();
    assert!(
        connecting_hashes_heavier_disconnected(&hub, l3.block_hash())
            .unwrap()
            .is_none(),
        "child of current tip is a normal extension, not a connecting search"
    );

    let path = connecting_hashes_heavier_disconnected(&hub, w4.block_hash())
        .unwrap()
        .expect("heavier winner that does not connect at tip must name a path");
    assert_eq!(
        path,
        vec![
            w1.block_hash(),
            w2.block_hash(),
            w3.block_hash(),
            w4.block_hash()
        ],
        "path must be winner mids from LCA, not the loser tip+1"
    );
    assert!(!path.contains(&l3.block_hash()));
    let _ = std::fs::remove_dir_all(dir);
}

/// Work-path tip+1 / far header (no resume seed) still registers the
/// connecting prefix — the live IBD hook, not BadPrev.
#[test]
fn consider_disconnected_from_work_path_without_resume_seed() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_062_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_062_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_062_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_062_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_062_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_062_401, 4);
    hub.ensure_header(&w4.header).unwrap();

    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(w3.block_hash(), 3);
    st.note_path_prev(w3.block_hash(), w2.block_hash());
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    let shared = std::sync::Mutex::new(st);
    assert!(rewind_off_coord_lock(&shared, &hub, None).unwrap());
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert_eq!(
        hub.tip_height(),
        Some(0),
        "competing tip+1 must rewind to the LCA"
    );
    assert_eq!(st.height_to_hash.get(&1), Some(&w1.block_hash()));
    assert_eq!(st.height_to_hash.get(&2), Some(&w2.block_hash()));
    assert!(
        st.header_fks.contains_key(&w1.block_hash()),
        "a rewound header keeps the fk the body path needs"
    );
    assert!(
        st.reorg.need_getdata().is_empty(),
        "winner is a linear extension after rewind; need={:?}",
        st.reorg.need_getdata()
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A real block whose prev is not a stored header fails the live confirm
/// load as BadPrev. That reject is soft reget: the tip stays, the slot is
/// evicted, and the hash is not blacklisted or fetched again.
#[test]
fn unknown_prev_header_is_soft_reget_not_a_rewind() {
    use rbitcoin_ibd::ibd::confirm::ConfirmRejectClass;
    use rbitcoin_ibd::ibd::events::apply_confirm_reject;

    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let tip = mine(gen, 1_500_080_100, 1);
    hub.accept_block(tip).unwrap();
    assert_eq!(hub.tip_height(), Some(1));

    let unknown = BlockHash::from_byte_array([0x9e; 32]);
    assert!(
        hub.query
            .get_header_by_hash(&unknown.to_byte_array())
            .unwrap()
            .is_none(),
        "prev must not be a stored header"
    );
    let orphan = mine(unknown, 1_500_080_200, 2);
    let err = match hub.confirm_wire_load_phase(&[(Height(2), orphan.clone())]) {
        Err(e) => e,
        Ok(_) => panic!("unknown prev must fail confirm load"),
    };
    assert!(
        matches!(err, NetError::BadPrev),
        "live load maps ConsensusError::BadPrev, got {err}"
    );
    let msg = err.to_string();
    let class = ConfirmRejectClass::from_net(&err);
    assert_eq!(class, ConfirmRejectClass::SoftWire, "{msg}");
    assert!(is_bad_prev_err(&msg), "{msg}");

    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.headers_done = true;
    st.record_height(orphan.block_hash(), 2);
    apply_confirm_reject(
        &mut st,
        2,
        orphan.block_hash(),
        class,
        &msg,
        Some(hub.query.as_ref()),
        Some(&hub),
        1,
        None,
        None,
        rbitcoin_ibd::ibd::confirm::RejectParent::default(),
    );
    assert_eq!(hub.tip_height(), Some(1), "unknown prev must not rewind");
    assert!(!st.headers_done, "soft reget clears the header latch");
    assert!(
        !st.height_to_hash.contains_key(&2),
        "the rejected slot is evicted"
    );
    assert!(
        !st.reorg
            .invalid
            .contains(orphan.block_hash().to_byte_array()),
        "soft reget does not blacklist"
    );
    assert!(
        !st.reorg.need_getdata().contains(&orphan.block_hash()),
        "BadPrev does not re-get the same hash"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Linear headers ahead of tip are a normal extension, not a fork search.
/// (CI: two_node IBD hung at tip=1 after consider treated h=8 as disconnected.)
#[test]
fn linear_ahead_headers_are_not_a_disconnected_fork() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let b1 = mine(gen, 1_500_070_100, 1);
    hub.accept_block(b1.clone()).unwrap();
    let mut prev = b1.block_hash();
    let mut time = 1_500_070_200u32;
    let mut ahead = Vec::new();
    for h in 2..=8u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        time += 100;
        ahead.push((h, prev));
    }
    let last = ahead.last().unwrap().1;
    assert!(
        connecting_hashes_heavier_disconnected(&hub, last)
            .unwrap()
            .is_none(),
        "far header on the same chain as tip is not a disconnected fork"
    );
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    for (h, hash) in &ahead {
        st.record_height(*hash, *h);
    }
    st.max_ordered_height = 8;
    assert!(
        !consider_disconnected_heavier(&mut st, &hub).unwrap(),
        "linear work-path must not register connecting search; need={:?}",
        st.reorg.need_getdata()
    );
    assert!(st.reorg.need_getdata().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// Early IBD: headers sit far above a low tip. The ancestor walk hits its
/// cap on a header-only mid, so join is not a confirmed LCA. That is still
/// a linear extension, not a connecting search.
///
/// Production walk is 10_000; a smaller cap plus ~20 headers hits the same
/// branch without mining a mainnet-scale horizon.
#[test]
fn capped_walk_on_linear_headers_is_not_a_disconnected_fork() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let b1 = mine(gen, 1_500_080_100, 1);
    hub.accept_block(b1.clone()).unwrap();
    let mut prev = b1.block_hash();
    let mut time = 1_500_080_200u32;
    let mut last = prev;
    for h in 2..=20u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        last = prev;
        time += 100;
    }
    let path = connecting_hashes_heavier_disconnected_n(&hub, last, 8).unwrap();
    assert!(
        path.is_none(),
        "capped walk whose join is header-only must not start connecting search; got {path:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_skips_linear_tip_plus_one() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let mut prev = gen;
    let mut time = 1_500_090_100u32;
    let b1 = mine(prev, time, 1);
    hub.accept_block(b1.clone()).unwrap();
    prev = b1.block_hash();
    time += 100;
    let b2 = mine(prev, time, 2);
    hub.accept_block(b2.clone()).unwrap();
    prev = b2.block_hash();
    time += 100;
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let mut last = prev;
    for h in 3..=20u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        last = prev;
        time += 100;
        st.record_height(prev, h);
    }
    st.max_ordered_height = 20;
    assert!(
        connecting_search_candidates(&st, &hub).unwrap().is_empty(),
        "linear tip+1 must not be a connecting-search candidate (avoids a 10k max_ordered walk)"
    );
    assert!(
        !consider_disconnected_heavier(&mut st, &hub).unwrap(),
        "linear tip+1 + far headers must not register; last={last} need={:?}",
        st.reorg.need_getdata()
    );
    assert!(st.reorg.need_getdata().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_uses_recorded_prev_not_the_header_store() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let child = BlockHash::from_byte_array([0xab; 32]);
    st.record_height(child, 1);
    st.note_path_prev(child, BlockHash::from_byte_array([0xcd; 32]));
    assert!(
        hub.query
            .get_header_by_hash(&child.to_byte_array())
            .unwrap()
            .is_none(),
        "this hash was never stored"
    );
    assert_eq!(
        connecting_search_candidates(&st, &hub).unwrap(),
        vec![child],
        "a recorded parent that is not the tip is a competing child"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_candidates_is_tip_plus_one_when_competing() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_091_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_091_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_091_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_091_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_091_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_091_401, 4);
    hub.ensure_header(&w4.header).unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(w3.block_hash(), 3);
    st.note_path_prev(w3.block_hash(), w2.block_hash());
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    assert_eq!(
        connecting_search_candidates(&st, &hub).unwrap(),
        vec![w3.block_hash()],
        "competing tip+1 is the only candidate; far header is the same fork"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_skips_when_tip_plus_one_is_own_child() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_092_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_092_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let l3 = mine(l2.block_hash(), 1_500_092_300, 3);
    hub.ensure_header(&l3.header).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_092_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_092_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_092_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_092_401, 4);
    hub.ensure_header(&w4.header).unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(l3.block_hash(), 3);
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    assert!(
        connecting_search_candidates(&st, &hub).unwrap().is_empty(),
        "own-child tip+1 is linear; heavier far header is not a consider candidate"
    );
    assert!(
        !consider_disconnected_heavier(&mut st, &hub).unwrap(),
        "accepted miss: heavier fork only at max_ordered while tip+1 is our child"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explore_need_and_tips_stay_capped() {
    let mut st = IbdReorgState::new();
    let mut last = BlockHash::from_byte_array([1u8; 32]);
    for i in 0..80u32 {
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&i.to_le_bytes());
        last = BlockHash::from_byte_array(raw);
        st.register_explore([last], Some(last));
    }
    assert!(
        st.explore_need_hashes().len() <= 64,
        "explore_need grew to {}",
        st.explore_need_hashes().len()
    );
    assert!(
        st.explore_tips().len() <= 64,
        "explore_tips grew to {}",
        st.explore_tips().len()
    );
    assert!(st.explore_need_hashes().contains(&last));
    assert!(st.explore_tips().contains(&last));
}
