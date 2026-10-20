use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_ibd::ibd::assign::clear_hash_inflight;
use rbitcoin_ibd::ibd::assign_plan::{
    remove_from_ordered, should_enqueue_header, want_headers_beyond_soft_cap,
};
use rbitcoin_ibd::ibd::dial::{
    disconnect_peer, note_dead_without_block_bytes, note_misbehavior_dead, release_peer_block_work,
};
use rbitcoin_ibd::ibd::exit::{
    header_lag_behind_peers, should_advance_locator_after_known_batch,
    should_log_empty_headers_lag, should_rerequest_headers_on_empty_lag,
    should_reseed_work_path_on_empty_lag,
};
use rbitcoin_ibd::ibd::peer_io::empty_header_fk_bus;
use rbitcoin_ibd::ibd::peer_io::{note_block_progress, note_block_rx, PeerCmd, PeerEvent};
use rbitcoin_ibd::ibd::state::IbdWorkState;
use rbitcoin_ibd::ibd::status::LoopStats;
use rbitcoin_ibd::ibd::{
    CONTIG_DENSIFY_AHEAD, MAX_ORDERED_HEADERS, MAX_PEER_POOL, ORDERED_HEADERS_SOFT_CAP,
};
use rbitcoin_log::{info, trace, warn};
use rbitcoin_net::chain::ChainHub;
use rbitcoin_net::codec::MAX_HEADERS_RESULTS;
use rbitcoin_net::error::NetError;
use rbitcoin_net::seeds::AddrMan;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use rbitcoin_ibd::ibd::events::run_header_batch;

use std::sync::Mutex;

/// A batch classification drops is not written. The header thread used to
/// persist every batch before it knew the batch was stray.
#[test]
fn stray_batch_is_not_persisted() {
    let (dir, mut hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("stray-batch");
    hub.ensure_genesis().unwrap();
    hub.milestone = rbitcoin_consensus::Milestone {
        height: 1,
        anchor: Some(rbitcoin_consensus::MilestoneAnchor {
            hash: BlockHash::from_byte_array([0xcd; 32]),
            min_work_be: [0xff; 32],
        }),
    };
    let header = {
        let mut h = bitcoin::block::Header {
            version: bitcoin::block::Version::from_consensus(4),
            prev_blockhash: BlockHash::from_byte_array([0xab; 32]),
            merkle_root: bitcoin::TxMerkleNode::from_byte_array([1; 32]),
            time: 1_600_000_000,
            bits: bitcoin::CompactTarget::from_consensus(0x207fffff),
            nonce: 0,
        };
        rbitcoin_consensus::grind_regtest_pow(&mut h);
        h
    };
    let mut st = IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let _ = rbitcoin_ibd::ibd::header_walk::send_getheaders(&mut st, &hub);
    let shared = Mutex::new(st);
    rbitcoin_net::chain::reset_header_store_counts();
    run_header_batch(&shared, &hub, 0, vec![header], &empty_header_fk_bus());
    assert_eq!(
        rbitcoin_net::chain::header_store_counts().0,
        0,
        "a stray batch is classified before any header is persisted"
    );
    assert_eq!(side_count(&hub), 0);
    let _ = std::fs::remove_dir_all(dir);
}

fn side_count(hub: &rbitcoin_net::chain::ChainHub) -> usize {
    hub.chaintips()
        .iter()
        .filter(|t| t.status != "active")
        .count()
}
