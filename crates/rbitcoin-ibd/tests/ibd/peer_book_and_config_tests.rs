use bitcoin::p2p::Magic;
use rbitcoin_ibd::ibd::archive::{
    rehydrate_block_queue_into_confirm, rehydrate_class_a_into_body_queue,
};
use rbitcoin_ibd::ibd::assign::bq_pipeline_saturated;
use rbitcoin_ibd::ibd::assign::{assign_work_ordered, AssignDepth};
use rbitcoin_ibd::ibd::assign_plan::need_ready_headroom;
use rbitcoin_ibd::ibd::assign_plan::want_headers_beyond_soft_cap;
use rbitcoin_ibd::ibd::cadence::IbdLoopCadence;
use rbitcoin_ibd::ibd::confirm::{
    offer_confirm_ready, spawn_confirm_engine, ConfirmEvent, ConfirmFeed,
};
use rbitcoin_ibd::ibd::confirm::{ConfirmRejectClass, RejectParent};
use rbitcoin_ibd::ibd::dial::{
    admit_cooldown_fallback, alive_dial_addrs, apply_dial_result, dial_batch, dial_blocked_addrs,
    disconnect_relative_slow_block_peers, disconnect_stalled_block_peers, expire_addr_cooldown,
    redial_want, release_peer_block_work,
};
use rbitcoin_ibd::ibd::events::drain_ready_peer_and_body_events;
use rbitcoin_ibd::ibd::events::{
    apply_confirm_events, apply_peer_event, disconnect_all_peers, update_confirm_lag,
};
use rbitcoin_ibd::ibd::events::{apply_confirm_reject, apply_headers_event};
use rbitcoin_ibd::ibd::exit::{
    all_peers_dead_action, best_chain_remainder, dark_empty_after, empty_path_header_fan,
    header_lag_behind_peers, ibd_caught_up, may_spawn_redial, path_drained,
    should_unlatch_headers_done, AllPeersDead,
};
use rbitcoin_ibd::ibd::header_walk::{
    candidate_tip, candidate_work, forget_walk_peer, header_poll, header_status_view,
    install_origin, load_origin, note_block_inv, note_reservation, origin_input, send_getheaders,
    settle_walk_peer, take_header_ask, HeaderPoll, HeaderPollIn,
};
use rbitcoin_ibd::ibd::peer_io::{empty_header_fk_bus, PeerCmd, PeerEvent};
use rbitcoin_ibd::ibd::peer_io::{sample_peer_rates, PeerEventSinks};
use rbitcoin_ibd::ibd::peer_io::{solicit_track, GetdataWire, PeerSlot};
use rbitcoin_ibd::ibd::perf_log::{read_platform_rss, tip_perf_json, ProcessRss, TipPerfLog};
use rbitcoin_ibd::ibd::progress::{log_progress_tick, StatusClock};
use rbitcoin_ibd::ibd::progress::{read_progress_published, StatusSnap};
use rbitcoin_ibd::ibd::progress::{
    spawn_status_thread, work_chain_progress_at, StatusBoard, TipRateTracker,
};
use rbitcoin_ibd::ibd::published::spawn_header_thread;
use rbitcoin_ibd::ibd::state::IbdWorkState;
use rbitcoin_ibd::ibd::state::{enqueue_header_cmd, HeaderCmd};
use rbitcoin_ibd::ibd::status::LoopStats;
use rbitcoin_ibd::ibd::PeerBookSession;
use rbitcoin_log::{info, warn};
use rbitcoin_net::chain::ChainHub;
use rbitcoin_net::codec::MAX_HEADERS_RESULTS;
use rbitcoin_net::connect_timeout_for;
use rbitcoin_net::error::NetError;
use rbitcoin_net::seeds::AddrMan;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

fn sa(o: u8) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, o)), 18444)
}

#[test]
fn peer_book_session_injects_seeds_and_flushes_on_drop() {
    let shared = Arc::new(Mutex::new(AddrMan::new()));
    {
        let mut sess = PeerBookSession::new(
            Some(Arc::clone(&shared)),
            &[
                rbitcoin_net::NetAddr::Ip(sa(1)),
                rbitcoin_net::NetAddr::Ip(sa(2)),
            ],
        );
        assert!(sess.book().entry(&sa(1)).is_some());
        assert!(sess.book().entry(&sa(2)).is_some());
        // Mutate book via book_mut.
        sess.book_mut().add(sa(3));
        assert!(sess.book().entry(&sa(3)).is_some());
        // Shared not yet flushed until drop/flush.
        assert!(shared.lock().unwrap().entry(&sa(3)).is_none());
        sess.flush();
        assert!(shared.lock().unwrap().entry(&sa(3)).is_some());
    }
    // Drop flushes again (idempotent).
    assert!(shared.lock().unwrap().entry(&sa(1)).is_some());

    // No shared book — seeds only, flush is a no-op.
    let sess2 = PeerBookSession::new(None, &[rbitcoin_net::NetAddr::Ip(sa(9))]);
    assert!(sess2.book().entry(&sa(9)).is_some());
    sess2.flush();
}
