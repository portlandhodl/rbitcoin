use rbitcoin_mempool::MempoolGraphStats;
use rbitcoin_mempool::{AcceptError, SelectBudget, Selected};
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
use rbitcoin_net::*;

#[test]
fn outbound_defaults() {
    assert_eq!(DEFAULT_IBD_TARGET_PEERS, 16);
}
