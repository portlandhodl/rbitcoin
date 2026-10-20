//! Short regtest IBD smoke: seed node with a few blocks, peer catch-up via
//! [`P2PNode::sync`]. Hits the main IBD loop, archive pipeline, confirm engine,
//! peer_io, and dial/events paths without live datadirs.
use rbitcoin_ibd::ibd::{
    read_platform_rss, rehydrate_block_queue_residue, tip_perf_json, IbdConfig, ProcessRss,
    TipPerfLog, DEFAULT_BLOCKS_IN_TRANSIT_PER_PEER, DEFAULT_IBD_WINDOW,
};
use rbitcoin_net::asmap::{
    AsMap, ASMAP_EIGHT_TRAILING_ZEROS, ASMAP_JUMP_OFF_END, ASMAP_JUMP_PAST_FILE,
    ASMAP_JUMP_SHORT_TAIL, ASMAP_MATCH_PAST_END, ASMAP_SECOND_DEFAULT, ASMAP_TRAILING_ONE,
    ASMAP_TWO_SHORT_MATCHES, LONE_RETURN_ASMAP, LOOPBACK_ASMAP, TWO_PREFIX_ASMAP,
};
use rbitcoin_net::cache::BlockCache;
use rbitcoin_net::chain::{
    check_block_proposal_with, AcceptOutcome, ChainHub, ChainTipInfo, TipEvent,
};
use rbitcoin_net::compact::{
    classify_v2_cmpct_peer, prefilled_indexes_ok, shortid_map_from_txs, try_reconstruct,
    v2_header_hashes, CmpctPeerFrame,
};
use rbitcoin_net::ephemeral::spawn_isolated_broadcast_loop;
use rbitcoin_net::error::NetError;
use rbitcoin_net::eviction::{select_inbound_eviction, InboundEvictCandidate};
use rbitcoin_net::i2p_sam::{i2p_addr_from_sam_b64, I2pSam};
use rbitcoin_net::most_work::{sum_work, WorkOverflow};
use rbitcoin_net::net_permissions::{
    apply_implicit, parse_whitebind, parse_whitelist, NetPermTable, NetPermissionFlags,
    WhitebindGrant, WhitelistGrant, DEFAULT_WHITELISTFORCERELAY, DEFAULT_WHITELISTRELAY,
};
use rbitcoin_net::netaddr::{is_cjdns_ip, NetAddr, OnlyNet};
use rbitcoin_net::netgroup::netgroup;
use rbitcoin_net::peer::{
    drain_pending_now, flush_tx_invs, force_announce_txid, local_service_flags,
    local_service_flags_pruned, run_feeler_timed, set_compact_filters_service, PendingBlocks,
    V2PlainSession, MAX_SERVE_BLOCKS,
};
use rbitcoin_net::peer_dos::DEFAULT_MAX_INBOUND;
use rbitcoin_net::peers::{
    connection_counts, outbound_time_offset, parse_peer_addr, parse_peer_addr_with_port,
    parse_peer_net, pick_stale_follow_evict, DialRequest, DialTarget, LivePeer, PeerConnType,
    PeerHub, PeerInfo, PeerOut, PingAction,
};
use rbitcoin_net::perf_meter::RequestMeter;
use rbitcoin_net::reactor::BlockingRegion;
use rbitcoin_net::seeds::{
    default_port, default_rpc_port, dns_seeds, fixed_seed_hosts, resolve_all_seeds,
    resolve_dns_seeds, resolve_fixed_seeds, socks_dns_seed_dests, AddrMan, PeerEntry, PeerFlags,
    MAX_ADDR_MAN,
};
use rbitcoin_net::serve_perf::{
    format_serve_perf, sample_reset_serve_perf, serve_perf_totals, ServePerfSample,
};
use rbitcoin_net::service::P2PNode;
use rbitcoin_net::socks::{install_i2p_dialer, Dialer};
use rbitcoin_net::tx_relay::{
    ElectrumMempoolItem, FeeHistoryBackfillStats, MempoolAnnounce, MempoolHub, MempoolPerfSample,
    MempoolTxSnapEntry, MempoolTxSnapshot,
};
use rbitcoin_net::v2::{encode_v2_contents, parse_v2_regtest, parse_v2_regtest_named, WireBytes};
use rbitcoin_net::versionbits_warn::warning_strings;

use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash};
use rbitcoin_consensus::{ChainParams, Milestone};

use rbitcoin_query::Query;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn tmp_dir(label: &str) -> rbitcoin_store::testutil::TempDir {
    rbitcoin_store::testutil::TempDir::labeled(label).unwrap()
}

fn mine(prev: BlockHash, time: u32, height: u32) -> Block {
    rbitcoin_consensus::mine_empty_regtest(prev, time, height)
}

async fn start_node(dir: &std::path::Path) -> P2PNode {
    let q = Query::open_or_create_tiny(dir.join("store")).unwrap();
    P2PNode::start(
        "127.0.0.1:0".parse().unwrap(),
        q,
        ChainParams::regtest(),
        Milestone::NONE,
    )
    .await
    .expect("listen")
}

fn seed_chain(node: &P2PNode, blocks: u32) {
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    node.ingest_block(0, genesis.clone()).unwrap();
    let mut tip = genesis.block_hash();
    let mut time = genesis.header.time;
    for h in 1..=blocks {
        let b = mine(tip, time + 600, h);
        tip = b.block_hash();
        time = b.header.time;
        node.ingest_block(h, b).unwrap();
    }
}

/// Cancel flag exits IBD cooperatively without hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ibd_cancellable_exits_when_flag_set() {
    let seed_dir = tmp_dir("seed-cancel");
    let peer_dir = tmp_dir("peer-cancel");

    let seed = start_node(&seed_dir).await;
    seed_chain(&seed, 4);

    let peer = start_node(&peer_dir).await;
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_c = cancel.clone();
    // Flip cancel after a short delay so dial/handshake may start.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.store(true, Ordering::SeqCst);
    });
    let mut cfg = IbdConfig::for_test();
    cfg.target_peers = 1;
    // Cancelled IBD should return Ok (partial) or complete if race finishes first.
    let _ = rbitcoin_ibd::sync_cancellable(
        &peer,
        &[rbitcoin_net::NetAddr::Ip(seed.local_addr)],
        cfg,
        Some(cancel),
    )
    .await;

    seed.shutdown().await;
    peer.shutdown().await;
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&peer_dir);
}

/// Empty peer list is a clean protocol error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ibd_no_peers_errors() {
    let dir = tmp_dir("empty");
    let node = start_node(&dir).await;
    let err = rbitcoin_ibd::sync(&node, &[], IbdConfig::for_test())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no peers") || err.to_string().contains("protocol"));
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Unreachable peer → dial fail / no peers connected (main-loop error arm).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ibd_unreachable_peer_errors() {
    let dir = tmp_dir("unreachable");
    let node = start_node(&dir).await;
    let mut cfg = IbdConfig::for_test();
    cfg.target_peers = 1;
    cfg.connect_timeout = Duration::from_millis(150);
    // TEST-NET-1 documentation address — closed / non-listening.
    let dead: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let err = rbitcoin_ibd::sync(&node, &[rbitcoin_net::NetAddr::Ip(dead)], cfg)
        .await
        .unwrap_err();
    let s = err.to_string();
    assert!(
        s.contains("no peers") || s.contains("protocol") || s.contains("connect"),
        "unexpected: {s}"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

fn logs_from_sync() -> impl Fn() -> Vec<String> {
    rbitcoin_log::capture_logs(true);
    || {
        rbitcoin_log::take_logs()
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }
}

/// One live seed and a higher target reaches the redial clock. An empty body
/// queue must not claim it noted rows, and a store with no Class A bodies must
/// not announce a seed count of zero.
#[tokio::test(flavor = "current_thread")]
async fn ibd_redials_immediately_and_does_not_invent_a_rehydrate() {
    let seed_dir = tmp_dir("seed-redial");
    let peer_dir = tmp_dir("peer-redial");
    let seed = start_node(&seed_dir).await;
    seed_chain(&seed, 2);
    let peer = start_node(&peer_dir).await;
    peer.hub.ensure_genesis().unwrap();
    let archived = peer.hub.query.archived_block_count().unwrap();
    let mut cfg = IbdConfig::for_test();
    cfg.target_peers = 2;
    let take = logs_from_sync();
    // This chain is two blocks, so catch-up exits on its own.
    let finished = tokio::time::timeout(
        Duration::from_secs(20),
        rbitcoin_ibd::sync_cancellable(
            &peer,
            &[rbitcoin_net::NetAddr::Ip(seed.local_addr)],
            cfg,
            None,
        ),
    )
    .await;
    let logs = take();
    rbitcoin_log::capture_logs(false);
    assert!(finished.is_ok(), "short-chain ibd did not finish: {logs:?}");
    assert!(
        logs.iter().any(|line| line.contains("ibd: redialing")),
        "a short peer set redials on the first loop: {logs:?}"
    );
    assert!(
        logs.iter().all(|line| !line.contains("noted 0")),
        "an empty body queue is not reported as noted: {logs:?}"
    );
    let seed_line = format!("store has {archived} Class A bodies (seed)");
    if archived > 0 {
        assert!(
            logs.iter().any(|line| line.contains(&seed_line)),
            "Class A bodies already in the store are named: {logs:?}"
        );
    } else {
        assert!(
            logs.iter()
                .all(|line| !line.contains("Class A bodies (seed)")),
            "an empty store does not announce a Class A seed: {logs:?}"
        );
    }
    seed.shutdown().await;
    peer.shutdown().await;
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&peer_dir);
}

/// A block left in the RAM body queue above the tip is noted into confirm.
#[tokio::test(flavor = "current_thread")]
async fn ibd_notes_a_body_queue_row_above_the_tip() {
    let seed_dir = tmp_dir("seed-bq");
    let peer_dir = tmp_dir("peer-bq");
    let seed = start_node(&seed_dir).await;
    seed_chain(&seed, 1);
    let peer = start_node(&peer_dir).await;
    peer.hub.ensure_genesis().unwrap();
    let gen = peer.hub.tip_hash().unwrap();
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    let block = mine(gen, genesis.header.time + 600, 1);
    let fk = peer.hub.ensure_header_fk(&block.header).unwrap();
    let raw = bitcoin::consensus::encode::serialize(&block);
    peer.hub
        .query
        .block_queue_enqueue(1, block.block_hash().to_byte_array(), fk.0, &raw)
        .unwrap();
    let mut cfg = IbdConfig::for_test();
    cfg.target_peers = 1;
    let take = logs_from_sync();
    let finished = tokio::time::timeout(
        Duration::from_secs(20),
        rbitcoin_ibd::sync_cancellable(
            &peer,
            &[rbitcoin_net::NetAddr::Ip(seed.local_addr)],
            cfg,
            None,
        ),
    )
    .await;
    let logs = take();
    rbitcoin_log::capture_logs(false);
    assert!(finished.is_ok(), "short-chain ibd did not finish: {logs:?}");
    assert!(
        logs.iter()
            .any(|line| line.contains("ibd: rehydrate: noted 1 in-RAM body queue entries")),
        "the queued block is noted: {logs:?}"
    );
    seed.shutdown().await;
    peer.shutdown().await;
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&peer_dir);
}
