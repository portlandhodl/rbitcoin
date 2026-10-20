use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_ibd::ibd::state::IbdWorkState;
use rbitcoin_ibd::ibd::MAX_ORDERED_HEADERS;
use rbitcoin_log::{info, warn};
use rbitcoin_net::chain::ChainHub;
use std::time::Instant;

use rbitcoin_ibd::ibd::path::work_path_tips;

use rbitcoin_query::testutil::FixtureChain;
fn h(n: u8) -> BlockHash {
    let mut b = [0u8; 32];
    b[0] = n;
    BlockHash::from_byte_array(b)
}

#[test]
fn work_path_tips_from_ordered_newest_first() {
    let mut st = IbdWorkState::new(Vec::new(), None, Some(10));
    // ordered is tip→far (front near tip); tips take from the back (highest).
    for n in 1u8..=6 {
        let hash = h(n);
        st.ordered.push_back(hash);
        st.ordered_set.insert(hash);
        st.record_height(hash, 10 + u32::from(n));
    }
    let tips = work_path_tips(&st);
    assert_eq!(tips.len(), 4);
    assert_eq!(tips[0], h(6));
    assert_eq!(tips[1], h(5));
    assert_eq!(tips[2], h(4));
    assert_eq!(tips[3], h(3));
}

#[test]
fn work_path_tips_skips_ghosts_and_falls_back_to_hash_height() {
    let mut st = IbdWorkState::new(Vec::new(), None, Some(0));
    // Ghost: in deque but not ordered_set.
    st.ordered.push_back(h(1));
    st.ordered.push_back(h(2));
    // No live ordered members → fall back to max height in hash_height.
    st.record_height(h(9), 99);
    st.record_height(h(8), 50);
    let tips = work_path_tips(&st);
    assert_eq!(tips, vec![h(9)]);

    // Empty everything → empty tips.
    let empty = IbdWorkState::new(Vec::new(), None, None);
    assert!(work_path_tips(&empty).is_empty());
}

/// One hub: with no tip there is nothing to plant. After genesis, a
/// connected child and its extension known only by hash height are
/// planted as the work path.
#[test]
fn plant_valid_tip_child_from_no_tip_to_connected_headers() {
    use bitcoin::absolute::LockTime;
    use bitcoin::block::{Header, Version};
    use bitcoin::script::ScriptBuf;
    use bitcoin::transaction::Version as TxVersion;
    use bitcoin::{
        Amount, Block, CompactTarget, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness,
    };

    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("plant-tip-child");
    let mut st = IbdWorkState::new(Vec::new(), None, None);
    rbitcoin_ibd::ibd::path::plant_valid_tip_child(&mut st, &hub);
    assert!(st.ordered.is_empty());
    assert!(st.height_to_hash.is_empty());

    hub.ensure_genesis().unwrap();
    let mine = |prev: BlockHash, time: u32, height: u32| {
        let mut ss = rbitcoin_consensus::bip34_height_script(height);
        while ss.len() < 2 {
            ss.push(0x00);
        }
        let coinbase = Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(ss),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_0000_0000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        };
        let mut block = Block {
            header: Header {
                version: Version::from_consensus(4),
                prev_blockhash: prev,
                merkle_root: bitcoin::TxMerkleNode::from_byte_array([0u8; 32]),
                time,
                bits: CompactTarget::from_consensus(0x207f_ffff),
                nonce: 0,
            },
            txdata: vec![coinbase],
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        rbitcoin_consensus::grind_regtest_pow(&mut block.header);
        block
    };
    let child = mine(hub.tip_hash().unwrap(), 1_300_000_600, 1);
    let ext = mine(child.block_hash(), 1_300_001_200, 2);
    hub.ensure_header(&child.header).unwrap();
    hub.ensure_header(&ext.header).unwrap();

    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.hash_height.insert(child.block_hash(), 1);
    st.note_path_prev(child.block_hash(), hub.tip_hash().unwrap());
    st.hash_height.insert(ext.block_hash(), 2);
    st.note_path_prev(ext.block_hash(), child.block_hash());
    rbitcoin_ibd::ibd::path::plant_valid_tip_child(&mut st, &hub);
    assert!(st.ordered_set.contains(&child.block_hash()));
    assert!(st.ordered_set.contains(&ext.block_hash()));
    assert_eq!(st.height_to_hash.get(&1), Some(&child.block_hash()));
    assert_eq!(st.height_to_hash.get(&2), Some(&ext.block_hash()));
    let _ = std::fs::remove_dir_all(dir);
}

/// A sibling known only from the header store (no `path_prev` yet) is
/// still the tip child. The unknown-txid IBD pump depends on this after
/// the active tip+1 is marked invalid.
#[test]
fn plant_valid_tip_child_uses_stored_prev_without_path_prev() {
    use bitcoin::absolute::LockTime;
    use bitcoin::block::{Header, Version};
    use bitcoin::script::ScriptBuf;
    use bitcoin::transaction::Version as TxVersion;
    use bitcoin::{
        Amount, Block, CompactTarget, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness,
    };

    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("plant-stored-prev");
    hub.ensure_genesis().unwrap();
    let mine = |prev: BlockHash, time: u32, height: u32| {
        let mut ss = rbitcoin_consensus::bip34_height_script(height);
        while ss.len() < 2 {
            ss.push(0x00);
        }
        let coinbase = Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(ss),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_0000_0000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        };
        let mut block = Block {
            header: Header {
                version: Version::from_consensus(4),
                prev_blockhash: prev,
                merkle_root: bitcoin::TxMerkleNode::from_byte_array([0u8; 32]),
                time,
                bits: CompactTarget::from_consensus(0x207f_ffff),
                nonce: 0,
            },
            txdata: vec![coinbase],
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        rbitcoin_consensus::grind_regtest_pow(&mut block.header);
        block
    };
    let child = mine(hub.tip_hash().unwrap(), 1_300_000_600, 1);
    hub.ensure_header(&child.header).unwrap();
    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.hash_height.insert(child.block_hash(), 1);
    rbitcoin_ibd::ibd::path::plant_valid_tip_child(&mut st, &hub);
    assert_eq!(st.height_to_hash.get(&1), Some(&child.block_hash()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn work_path_tips_respects_live_set_only() {
    let mut st = IbdWorkState::new(Vec::new(), None, Some(1));
    st.ordered.push_back(h(1));
    st.ordered.push_back(h(2));
    st.ordered.push_back(h(3));
    st.ordered_set.insert(h(1));
    st.ordered_set.insert(h(3)); // h(2) is a middle ghost
    let tips = work_path_tips(&st);
    // rev walk: 3 (live), 2 (ghost skip), 1 (live) — only set members.
    assert_eq!(tips, vec![h(3), h(1)]);
}

#[test]
fn work_path_tips_skips_invalid_and_rejected() {
    let mut st = IbdWorkState::new(Vec::new(), None, Some(10));
    for n in 1u8..=4 {
        let hash = h(n);
        st.ordered.push_back(hash);
        st.ordered_set.insert(hash);
        st.record_height(hash, 10 + u32::from(n));
    }
    st.reorg.invalid.mark(h(4).to_byte_array());
    st.body.mark_rejected(h(3));
    let tips = work_path_tips(&st);
    assert!(!tips.contains(&h(4)), "invalid tip must not be a locator");
    assert!(!tips.contains(&h(3)), "rejected tip must not be a locator");
    assert!(tips.contains(&h(2)));
}

/// Exploration tips merge into locator tips (cap 8, dedupe ordered members).
#[test]
fn work_path_tips_includes_explore_tips_capped() {
    let mut st = IbdWorkState::new(Vec::new(), None, Some(10));
    for n in 1u8..=3 {
        let hash = h(n);
        st.ordered.push_back(hash);
        st.ordered_set.insert(hash);
    }
    // Explore tips: one already on ordered (dedupe), plus many unique.
    let mut need = Vec::new();
    for n in 10u8..=20 {
        need.push(h(n));
    }
    st.reorg.register_explore(need, Some(h(20)));
    let tips = work_path_tips(&st);
    assert!(tips.contains(&h(3))); // ordered newest
    assert!(tips.contains(&h(20))); // explore tip
    assert!(tips.len() <= 8);
    // h(1) is on ordered (taken in first 4) so not re-added as explore.
    assert_eq!(tips.iter().filter(|x| **x == h(1)).count(), 1);
}

#[test]
fn seed_work_path_from_empty_and_genesis_store() {
    use rbitcoin_ibd::ibd::path::seed_work_path_from_store;

    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("path-seed");
    // Empty store: tip_hash is None → seed returns immediately.
    let mut st = IbdWorkState::new(Vec::new(), None, None);
    seed_work_path_from_store(&mut st, &hub);
    assert!(st.ordered.is_empty());

    hub.ensure_genesis().unwrap();
    let mut st2 = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    seed_work_path_from_store(&mut st2, &hub);
    // Resume path after tip may be empty (no headers beyond tip).
    assert!(!st2.headers_done); // always left open for peer tip
    let _ = std::fs::remove_dir_all(dir);
}

/// Stored headers ahead of the tip are the milestone path. A restart
/// that already restored the anchor must note them so script checks stay
/// off while those blocks confirm.
#[test]
fn resume_seed_notes_stored_headers_on_the_milestone_path() {
    use bitcoin::block::{Header, Version};
    use bitcoin::{BlockHash, CompactTarget};
    use rbitcoin_consensus::{Milestone, MilestoneAnchor};
    use rbitcoin_ibd::ibd::path::seed_work_path_from_store;

    let (dir, mut hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("path-seed-milestone");
    hub.ensure_genesis().unwrap();
    let anchor = BlockHash::from_byte_array([0xab; 32]);
    hub.milestone = Milestone {
        height: 100,
        anchor: Some(MilestoneAnchor {
            hash: anchor,
            min_work_be: [0u8; 32],
        }),
    };
    hub.query.note_milestone_header(
        100,
        anchor.to_byte_array(),
        [0u8; 32],
        bitcoin::Work::from_be_bytes([0u8; 32]),
        None,
    );

    let mut prev = hub.tip_hash().unwrap();
    let mut first = None;
    for n in 1u32..=2 {
        let mut header = Header {
            version: Version::from_consensus(4),
            prev_blockhash: prev,
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([n as u8; 32]),
            time: 1_600_000_000 + n,
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce: n,
        };
        rbitcoin_consensus::grind_regtest_pow(&mut header);
        hub.ensure_header(&header).unwrap();
        if first.is_none() {
            first = Some(header.block_hash());
        }
        prev = header.block_hash();
    }
    let first = first.unwrap();
    assert!(
        hub.query.milestone_header_at(1).is_none(),
        "the anchor note alone does not record the stored child"
    );

    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    seed_work_path_from_store(&mut st, &hub);
    assert!(
        st.hash_height.contains_key(&first),
        "resume seed queued the stored header"
    );
    assert_eq!(
        hub.query.milestone_header_at(1),
        Some(first.to_byte_array()),
        "a seeded header on the stored path is on the milestone path"
    );
    assert!(
        hub.milestone
            .skips_scripts(1, &first.to_byte_array(), |h| hub
                .query
                .milestone_header_at(h)),
        "script checks stay off for a stored header under the restored anchor"
    );
    assert_eq!(
        hub.query.milestone_header_at(100),
        Some(anchor.to_byte_array()),
        "noting the queue keeps the restored anchor"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Archive headers ahead of tip so resume seed walks a non-empty path
/// (bodies + a body gap for the contiguous-prefix break).
#[test]
fn seed_work_path_archives_ahead_of_tip() {
    use bitcoin::absolute::LockTime;
    use bitcoin::block::{Header, Version};
    use bitcoin::script::ScriptBuf;
    use bitcoin::transaction::Version as TxVersion;
    use bitcoin::{
        Amount, Block, CompactTarget, OutPoint, Sequence, Target, Transaction, TxIn, TxOut, Witness,
    };
    use rbitcoin_ibd::ibd::path::seed_work_path_from_store;
    use rbitcoin_query::prepare_block_for_archive;

    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("path-seed-arch");
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();

    fn coinbase(height: u32) -> Transaction {
        let mut ss = if height == 0 {
            vec![0x00]
        } else {
            rbitcoin_consensus::bip34_height_script(height)
        };
        while ss.len() < 2 {
            ss.push(0x00);
        }
        Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(ss),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_0000_0000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        }
    }
    fn mine(prev: BlockHash, time: u32, height: u32) -> Block {
        let bits = CompactTarget::from_consensus(0x207f_ffff);
        let header = Header {
            version: Version::from_consensus(4),
            prev_blockhash: prev,
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([0u8; 32]),
            time,
            bits,
            nonce: 0,
        };
        let mut block = Block {
            header,
            txdata: vec![coinbase(height)],
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        let target = Target::from_compact(bits);
        for nonce in 0..u32::MAX {
            block.header.nonce = nonce;
            if block.header.validate_pow(target).is_ok() {
                break;
            }
        }
        block
    }

    // Tip stays at genesis; Class A at 1..=2 via the crash helper (not public
    // archive_block). Header-only height 3 breaks the contiguous-prefix.
    let mut tip = gen;
    let time = 1_300_000_000u32;
    for h in 1u32..=2 {
        let b = mine(tip, time + h * 600, h);
        hub.ensure_header(&b.header).unwrap();
        let (rec, txs) = prepare_block_for_archive(&hub.query, &hub.params, &b).unwrap();
        hub.query.commit_class_a_only(&rec, &txs).unwrap();
        tip = b.block_hash();
    }
    let b3 = mine(tip, time + 3 * 600, 3);
    hub.ensure_header(&b3.header).unwrap();
    // no Class A body for h=3 → ready_prefix breaks

    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    seed_work_path_from_store(&mut st, &hub);
    assert!(
        st.ordered.len() >= 2,
        "resume should seed work path after tip, got {}",
        st.ordered.len()
    );
    assert!(st.max_ordered_height >= 2);
    assert!(st.max_ready_height >= 2); // contiguous claim-ready prefix
    assert!(!st.headers_done);
    // Duplicates on re-seed only insert once into ordered_set.
    let n = st.ordered.len();
    seed_work_path_from_store(&mut st, &hub);
    assert_eq!(st.ordered.len(), n);
    let _ = std::fs::remove_dir_all(dir);
}

/// Tip on a short loser; heavier sibling headers in the store → rewind
/// confirmed tip to the LCA and plant the winner as the linear work path.
#[test]
fn seed_rewinds_tip_to_lca_on_heavier_sibling() {
    use bitcoin::absolute::LockTime;
    use bitcoin::block::{Header, Version};
    use bitcoin::script::ScriptBuf;
    use bitcoin::transaction::Version as TxVersion;
    use bitcoin::{
        Amount, Block, CompactTarget, OutPoint, Sequence, Target, Transaction, TxIn, TxOut, Witness,
    };
    use rbitcoin_ibd::ibd::path::seed_work_path_from_store;

    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("path-seed-sib");
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();

    fn mine(prev: BlockHash, time: u32, height: u32) -> Block {
        let bits = CompactTarget::from_consensus(0x207f_ffff);
        let mut ss = if height == 0 {
            vec![0x00]
        } else {
            rbitcoin_consensus::bip34_height_script(height)
        };
        while ss.len() < 2 {
            ss.push(0x00);
        }
        let coinbase = Transaction {
            version: TxVersion::ONE,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::from_bytes(ss),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_0000_0000),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        };
        let header = Header {
            version: Version::from_consensus(4),
            prev_blockhash: prev,
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([0u8; 32]),
            time,
            bits,
            nonce: 0,
        };
        let mut block = Block {
            header,
            txdata: vec![coinbase],
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        let target = Target::from_compact(bits);
        for nonce in 0..u32::MAX {
            block.header.nonce = nonce;
            if block.header.validate_pow(target).is_ok() {
                break;
            }
        }
        block
    }

    let lose = mine(gen, 1_700_000_100, 1);
    let mut win = mine(gen, 1_700_000_101, 1);
    if win.block_hash() == lose.block_hash() {
        let target = Target::from_compact(win.header.bits);
        for nonce in 0..u32::MAX {
            win.header.nonce = nonce;
            if win.header.validate_pow(target).is_ok() && win.block_hash() != lose.block_hash() {
                break;
            }
        }
    }
    hub.accept_block(lose.clone()).unwrap();
    hub.ensure_header(&win.header).unwrap();
    let ext = mine(win.block_hash(), 1_700_000_200, 2);
    hub.ensure_header(&ext.header).unwrap();

    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    seed_work_path_from_store(&mut st, &hub);
    assert_eq!(
        hub.tip_hash(),
        Some(gen),
        "must disconnect the loser so the winner is a linear tip+1"
    );
    assert_eq!(hub.tip_height(), Some(0));
    assert_eq!(st.height_to_hash.get(&1), Some(&win.block_hash()));
    assert_eq!(st.height_to_hash.get(&2), Some(&ext.block_hash()));
    assert!(st.ordered_set.contains(&win.block_hash()));
    assert!(st.ordered_set.contains(&ext.block_hash()));
    assert!(
        st.reorg.need_getdata().is_empty(),
        "winner is above the new tip — no side-channel gather; need={:?}",
        st.reorg.need_getdata()
    );

    assert_eq!(hub.query.lookup_taken_hi(), Some(0));
    let tips = work_path_tips(&st);
    assert!(
        tips.contains(&ext.block_hash()) || tips.contains(&win.block_hash()),
        "locator tips must include the winning path"
    );
    let _ = std::fs::remove_dir_all(dir);
}
