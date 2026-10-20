use super::super::assign::dummy_slot;
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
use rbitcoin_ibd::ibd::peer_io::ibd_mono_ms;
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

use rbitcoin_ibd::ibd::events::{apply_peer_dead, take_addr_tokens};

/// IBD peers spend the same addr token bucket as tip-follow sessions: one
/// token at connect, so a 1000-address flood teaches the book one address.
#[test]
fn ibd_peer_addr_flood_takes_one_token() {
    let mut slot = dummy_slot(1);
    slot.connected_ms = ibd_mono_ms();
    let mut st = IbdWorkState::new(vec![slot], None, Some(0));
    assert_eq!(take_addr_tokens(&mut st, 1, 1_000), 1);
    assert_eq!(
        take_addr_tokens(&mut st, 1, 1_000),
        0,
        "the bucket is spent"
    );
    let mut book = AddrMan::new();
    apply_peer_dead(&mut st, &mut book, 1, "bye".to_string());
    assert!(
        st.addr_buckets.is_empty(),
        "a dead peer's bucket is dropped"
    );
}

/// An outbound `addr` whitelist exempts an IBD peer as it does a
/// tip-follow session.
#[test]
fn ibd_addr_permission_peer_is_not_rate_limited() {
    let mut slot = dummy_slot(1);
    slot.connected_ms = ibd_mono_ms();
    let ip = slot.addr.ip();
    let mut st = IbdWorkState::new(vec![slot], None, Some(0));
    let peers = rbitcoin_net::peers::PeerHub::new();
    let mut table = rbitcoin_net::net_permissions::NetPermTable::default();
    table
        .whitelist
        .push(rbitcoin_net::net_permissions::parse_whitelist(&format!("addr,out@{ip}")).unwrap());
    peers.set_net_perms(table);
    st.perms = Some(peers);
    assert_eq!(take_addr_tokens(&mut st, 1, 1_000), 1_000);
    assert_eq!(
        take_addr_tokens(&mut st, 1, 1_000),
        1_000,
        "an addr-permission peer is never limited"
    );
}
