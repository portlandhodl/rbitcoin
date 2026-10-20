//! Tests for rbitcoin_ibd::ibd::events:: events helpers (peeled from events.rs).
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

use bitcoin::consensus::Encodable;
use rbitcoin_consensus::{genesis_block, ChainParams};
use rbitcoin_ibd::ibd::events::decode_block_header_prefix;

#[test]
fn decode_block_header_prefix_surface() {
    assert!(decode_block_header_prefix(&[]).is_none());
    assert!(decode_block_header_prefix(&[0u8; 79]).is_none());
    let g = genesis_block(&ChainParams::regtest());
    let mut enc = Vec::new();
    g.header.consensus_encode(&mut enc).unwrap();
    assert!(enc.len() >= 80);
    let h = decode_block_header_prefix(&enc).expect("header");
    assert_eq!(h, g.header);
    // Extra payload after 80 bytes is ignored.
    enc.extend_from_slice(&[0xde, 0xad]);
    let h2 = decode_block_header_prefix(&enc).expect("header+tail");
    assert_eq!(h2, g.header);
    // All-zeros is still a consensus-decodable header shape.
    let zeros = decode_block_header_prefix(&[0u8; 80]);
    assert!(zeros.is_some());
}
