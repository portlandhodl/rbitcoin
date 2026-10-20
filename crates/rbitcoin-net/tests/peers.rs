use bitcoin::hashes::Hash;
use bitcoin::p2p::address::{AddrV2Message, Address};
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::ServiceFlags;
use bitcoin::{BlockHash, Wtxid};
use rbitcoin_net::error::NetError;
use rbitcoin_net::peers::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::mpsc;

use std::net::Ipv4Addr;

fn ver(ua: &str) -> VersionMessage {
    VersionMessage {
        version: 70016,
        services: ServiceFlags::NETWORK | ServiceFlags::WITNESS | ServiceFlags::P2P_V2,
        timestamp: 0,
        receiver: Address::new(
            &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1),
            ServiceFlags::NONE,
        ),
        sender: Address::new(
            &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2),
            ServiceFlags::NONE,
        ),
        nonce: 1,
        user_agent: ua.into(),
        start_height: 0,
        relay: true,
    }
}

#[test]
fn pending_sendcmpct_from_u8_maps_wire_payload() {
    assert_eq!(PendingSendCmpct::from_u8(0), PendingSendCmpct::None);
    assert_eq!(PendingSendCmpct::from_u8(1), PendingSendCmpct::Lb);
    assert_eq!(PendingSendCmpct::from_u8(2), PendingSendCmpct::Hb);
    assert_eq!(PendingSendCmpct::from_u8(3), PendingSendCmpct::None);
    assert_eq!(PendingSendCmpct::from_u8(255), PendingSendCmpct::None);
    assert_eq!(PendingSendCmpct::None as u8, 0);
    assert_eq!(PendingSendCmpct::Lb as u8, 1);
    assert_eq!(PendingSendCmpct::Hb as u8, 2);
}

#[test]
fn trying_connection_log_is_p2p_not_v1() {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(25, 0, 0, 1)), 8333);
    assert_eq!(
        trying_connection_log(PeerConnType::OutboundFullRelay, addr),
        "p2p: trying connection (outbound-full-relay) to 25.0.0.1:8333"
    );
    assert_eq!(
        trying_connection_log(PeerConnType::AddrFetch, addr),
        "p2p: trying connection (addr-fetch) to 25.0.0.1:8333"
    );
}

#[test]
fn snapshot_hides_fin_completed_keeps_connecting() {
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let local = std::net::TcpStream::connect(addr).unwrap();
    let (far, _) = listener.accept().unwrap();
    drop(far);
    let _ = local.shutdown(std::net::Shutdown::Both);

    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18444);
    let connecting = hub.register_connecting(a, a, true, PeerConnType::Inbound);
    connecting.attach_tcp_shutdown(local.try_clone().unwrap());
    assert_eq!(
        hub.snapshot().len(),
        1,
        "connecting peer stays in getpeerinfo during v2 handshake"
    );

    let done = hub.register(a, a, &ver("/rbitcoin:test/"), false, PeerConnType::Inbound);
    assert_eq!(
        done.sent_feefilter_sat_kvb(),
        -1,
        "registration has not sent a feefilter"
    );
    done.attach_tcp_shutdown(local);
    assert!(
        hub.snapshot().iter().all(|p| p.id != done.id),
        "completed FIN'd peer must not appear in getpeerinfo"
    );
    connecting.request_disconnect();
    assert!(
        hub.snapshot().iter().all(|p| p.id != connecting.id),
        "stopped connecting peer is omitted"
    );
}

#[test]
fn disconnect_id_shuts_down_tcp_so_far_side_sees_eof() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut local = std::net::TcpStream::connect(addr).unwrap();
    let (mut far, _) = listener.accept().unwrap();
    far.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18444);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445);
    let p = hub.register(
        a,
        b,
        &ver("/rbitcoin:0.1.0(testnode0)/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    let killer = local.try_clone().unwrap();
    p.attach_tcp_shutdown(killer);

    assert!(hub.disconnect_id(0));

    let start = Instant::now();
    let mut buf = [0u8; 1];
    let n = far.read(&mut buf);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "far side must observe close promptly, took {:?}",
        start.elapsed()
    );
    match n {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::UnexpectedEof
            ) => {}
        other => panic!("expected EOF/reset after disconnect_id, got {other:?}"),
    }
    let _ = local.write(&[1]);
}

#[test]
fn connecting_peer_v2_timeout_log_before_transport() {
    rbitcoin_log::capture_logs(true);
    let hub = PeerHub::new();
    hub.set_peer_timeout_secs(3);
    hub.set_mock_now(1_700_000_000);
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let p = hub.register_connecting(a, a, true, PeerConnType::Inbound);
    assert!(!p.handshake_complete());
    hub.set_mock_now(1_700_000_002);
    assert!(!p.stop.load(Ordering::SeqCst), "still inside peertimeout");
    hub.set_mock_now(1_700_000_003);
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    assert!(
        p.stop.load(Ordering::SeqCst),
        "peertimeout must disconnect pre-verack"
    );
    assert!(
        hub.get(p.id).is_none(),
        "timed-out connecting peer is dropped"
    );
    assert!(
        logs.iter()
            .any(|(_, m)| m.contains("V2 handshake timeout, disconnecting peer=0")),
        "expected V2 handshake timeout, got {logs:?}"
    );
}

#[test]
fn p2p_timeouts_v2_logs_all_three_connecting_peer_ids() {
    rbitcoin_log::capture_logs(true);
    let hub = PeerHub::new();
    hub.set_peer_timeout_secs(3);
    hub.set_mock_now(1_700_000_000);
    let addr = |p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p);
    let peers: Vec<_> = (1..=3)
        .map(|p| {
            let s = hub.register_connecting(addr(p), addr(p), true, PeerConnType::Inbound);
            s.mark_v2_transport_ready();
            s
        })
        .collect();
    assert_eq!(peers.iter().map(|p| p.id).collect::<Vec<_>>(), [0, 1, 2]);
    hub.set_mock_now(1_700_000_003);
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    let got: Vec<u64> = logs
        .iter()
        .filter_map(|(_, m)| {
            [0u64, 1, 2].into_iter().find(|id| {
                m.contains(&format!(
                    "version handshake timeout, disconnecting peer={id}"
                ))
            })
        })
        .collect();
    assert_eq!(
        got,
        vec![0, 1, 2],
        "timeout needles before TCP close, id order, got {logs:?}"
    );
}

#[test]
fn handshake_timeout_tick_logs_when_stop_already_set() {
    rbitcoin_log::capture_logs(true);
    let hub = PeerHub::new();
    hub.set_peer_timeout_secs(3);
    hub.set_mock_now(1_700_000_000);
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let p = hub.register_connecting(a, a, true, PeerConnType::Inbound);
    p.mark_v2_transport_ready();
    p.request_disconnect();
    let _ = rbitcoin_log::take_logs();
    hub.mock_now.store(1_700_000_003, Ordering::Release);
    let policy = rbitcoin_net::peer::HandshakePolicy {
        hub: None,
        peers: Some(&hub),
        session: Some(p.as_ref()),
        conn_type: PeerConnType::Inbound,
    };
    let err = rbitcoin_net::peer::fail_if_handshake_timed_out(&policy);
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    assert!(
        matches!(err, Err(rbitcoin_net::error::NetError::Timeout)),
        "got {err:?}"
    );
    assert!(
        logs.iter()
            .any(|(_, m)| m.contains("version handshake timeout, disconnecting peer=0")),
        "pre-verack ping vs mocktime must still log the needle, got {logs:?}"
    );
}

/// One PeerHub while peers come and go: compact high-bandwidth
/// selection and its evictions, compact fill slots, per-peer block and
/// header marks, the single initial headers-sync peer, and each peer's
/// announced-wtxid set.
#[allow(clippy::cognitive_complexity)] // one hub, many peer-state arms
#[test]
fn peerhub_hb_select() {
    let hub = PeerHub::new();
    let join = |port: u16, inbound: bool| {
        let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let conn = if inbound {
            PeerConnType::Inbound
        } else {
            PeerConnType::OutboundFullRelay
        };
        hub.register(a, a, &ver("/rbitcoin:0.1.0(testnode0)/"), inbound, conn)
    };
    let hb = |p: &LivePeer| p.hb_to.load(Ordering::Relaxed);
    let pending = |p: &LivePeer| p.pending_sendcmpct.load(Ordering::Relaxed);
    let sendcmpct = |rx: &mut mpsc::UnboundedReceiver<PeerOut>| match rx
        .try_recv()
        .expect("sendcmpct")
        .expect_msg()
    {
        NetworkMessage::SendCmpct(sc) => {
            assert_eq!(sc.version, 2);
            sc.send_compact
        }
        other => panic!("expected sendcmpct, got {other:?}"),
    };

    // An unknown id is ignored. A lone outbound is selected first.
    hub.maybe_select_hb(9_999);
    let out = join(18444, false);
    hub.maybe_select_hb(out.id);
    assert!(hb(&out));

    // Without a writer the HB choice waits as pending; with one it goes
    // out as sendcmpct at once.
    let a = join(18445, true);
    hub.maybe_select_hb(a.id);
    hub.maybe_select_hb(a.id);
    assert!(hb(&a));
    assert_eq!(pending(&a), PendingSendCmpct::Hb as u8);
    let b = join(18446, true);
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    b.attach_out(b_tx);
    hub.maybe_select_hb(b.id);
    assert!(hb(&b) && sendcmpct(&mut b_rx));
    assert_eq!(pending(&b), 0);

    // A third inbound evicts the oldest inbound and keeps the lone
    // outbound. The evicted peer's LB choice waits as pending.
    let c = join(18447, true);
    hub.maybe_select_hb(c.id);
    assert!(hb(&out) && hb(&b) && hb(&c));
    assert!(
        !hb(&a),
        "lone outbound stays; first inbound is the eviction"
    );
    assert_eq!(pending(&a), PendingSendCmpct::Lb as u8);

    // Re-selecting b refreshes it (Core LRU), so the next inbound evicts c.
    hub.maybe_select_hb(b.id);
    let d = join(18448, true);
    hub.maybe_select_hb(d.id);
    assert!(hb(&b), "re-selected inbound stays HB");
    assert!(!hb(&c), "oldest unrefreshed inbound is evicted");
    assert!(hb(&d));

    // d leaves. Its slot is free, so e does not evict a live HB peer.
    hub.unregister(d.id);
    let e = join(18449, true);
    hub.maybe_select_hb(e.id);
    assert!(hb(&out) && hb(&b) && hb(&e));

    // The outbound leaves too. With no outbound the oldest HB peer, b,
    // is evicted, and its writer gets sendcmpct LB.
    hub.unregister(out.id);
    let f = join(18450, true);
    hub.maybe_select_hb(f.id);
    let g = join(18451, true);
    hub.maybe_select_hb(g.id);
    assert!(!hb(&b) && !sendcmpct(&mut b_rx));
    assert_eq!(pending(&b), 0);
    assert!(hb(&e) && hb(&f) && hb(&g));

    // Two compact fill slots per block. A release frees one; a peer that
    // leaves releases its slot.
    let blk = BlockHash::from_byte_array([0x11; 32]);
    assert!(hub.try_cmpct_fill_slot(blk, true));
    assert!(hub.try_cmpct_fill_slot(blk, true));
    assert!(!hub.try_cmpct_fill_slot(blk, true));
    hub.release_cmpct_fill(blk, true);
    assert!(hub.try_cmpct_fill_slot(blk, true));
    assert!(!hub.try_cmpct_fill_slot(blk, true));
    hub.release_cmpct_fill(blk, true);
    hub.release_cmpct_fill(blk, true);
    let blk = BlockHash::from_byte_array([0x22; 32]);
    assert!(g.try_cmpct_fill(blk));
    hub.unregister(g.id);
    assert!(hub.try_cmpct_fill_slot(blk, true));
    assert!(hub.try_cmpct_fill_slot(blk, true));
    assert!(!hub.try_cmpct_fill_slot(blk, true));

    // A block e sent is not announced back to e, once. Header marks.
    let blk = BlockHash::from_byte_array([0xab; 32]);
    assert!(!e.take_block_from_peer(&blk));
    e.note_block_from_peer(blk);
    assert!(e.take_block_from_peer(&blk));
    assert!(!e.take_block_from_peer(&blk));
    assert!(e.try_ask_headers_for_inv());
    assert!(!e.try_ask_headers_for_inv());
    e.note_best_header_sent(blk);
    e.note_best_known(blk);
    assert_eq!(e.header_marks(), (Some(blk), Some(blk)));
    assert!(e.advertises_network());

    // With an old tip only one peer runs initial headers sync. Inv adds
    // at most one extra getheaders peer per hash. When the sync peer
    // leaves, another may start.
    let now = 1_700_000_000;
    let genesis_time = 1_231_006_505;
    assert!(hub.try_start_headers_sync(&a, now, genesis_time));
    assert!(a.is_sync_started());
    assert!(!hub.try_start_headers_sync(&c, now, genesis_time));
    assert!(!c.is_sync_started());
    let (h1, h2) = (
        BlockHash::from_byte_array([1u8; 32]),
        BlockHash::from_byte_array([2u8; 32]),
    );
    assert!(hub.should_getheaders_for_inv(&a, h1));
    assert!(hub.should_getheaders_for_inv(&c, h1));
    assert!(!hub.should_getheaders_for_inv(&f, h1));
    assert!(hub.should_getheaders_for_inv(&f, h2));
    hub.unregister(a.id);
    assert!(!a.is_sync_started());
    assert!(hub.try_start_headers_sync(&c, now, genesis_time));

    // Announced wtxids are per peer, roll the oldest at the cap, and
    // tx inv is due on a timer or on request.
    let wtxid_n = |i: u32| {
        let mut w = [0u8; 32];
        w[..4].copy_from_slice(&i.to_le_bytes());
        Wtxid::from_byte_array(w)
    };
    e.note_announced_wtx(wtxid_n(0));
    assert!(e.has_announced_wtx(&wtxid_n(0)));
    assert!(!e.has_announced_wtx(&wtxid_n(1)));
    assert!(!f.has_announced_wtx(&wtxid_n(0)));
    assert!(!e.take_tx_inv_due(1_700_000_000));
    assert!(!e.take_tx_inv_due(1_700_000_010));
    assert!(e.take_tx_inv_due(1_700_000_040));
    e.request_tx_inv();
    assert!(e.take_tx_inv_due(1_700_000_041));
    for i in 1..=50_000 {
        e.note_announced_wtx(wtxid_n(i));
    }
    assert!(!e.has_announced_wtx(&wtxid_n(0)));
    assert!(e.has_announced_wtx(&wtxid_n(1)));
    assert!(e.has_announced_wtx(&wtxid_n(50_000)));
}

#[test]
fn session_heartbeat_replaces_stalled_headers_sync_peer_only_with_another_preferred() {
    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2);
    let outbound = hub.register(
        a,
        a,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    let wall = hub.now_secs();
    let start = wall.saturating_sub(16 * 60);
    assert!(hub.try_start_headers_sync(&outbound, start, start));
    hub.on_session_heartbeat();
    assert!(
        !outbound.stop.load(Ordering::SeqCst),
        "must not disconnect the only preferred download peer"
    );
    assert!(outbound.is_sync_started());

    // An addnode / --connect peer is `manual`. Core counts it as a
    // preferred download peer, so the stalled sync peer can go.
    let _manual = hub.register(b, b, &ver("/rbitcoin:0.1.0/"), false, PeerConnType::Manual);
    hub.on_session_heartbeat();
    assert!(
        outbound.stop.load(Ordering::SeqCst),
        "a live manual peer must let the stalled headers-sync peer be replaced"
    );
}

#[test]
fn noban_headers_timeout_clears_awaiting_so_a_new_getheaders_can_send() {
    pin_noban_headers_timeout_keep(true);
    pin_noban_headers_timeout_keep(false);
}

fn pin_noban_headers_timeout_keep(via_cidr: bool) {
    let hub = PeerHub::new();
    if via_cidr {
        let mut t = rbitcoin_net::NetPermTable::default();
        t.whitelist
            .push(rbitcoin_net::parse_whitelist("noban@127.0.0.1").unwrap());
        hub.set_net_perms(t);
    } else {
        hub.set_noban(true);
    }
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2);
    let inbound = hub.register(a, a, &ver("/rbitcoin:0.1.0/"), true, PeerConnType::Inbound);
    let _outbound = hub.register(
        b,
        b,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    if via_cidr {
        assert!(!hub.is_noban(), "CIDR noban must not set hub --trusted");
        assert!(inbound.session_noban());
    }
    let now = 1_700_000_000u64;
    let best = 1_231_006_505u64;
    assert!(hub.try_start_headers_sync(&inbound, now, best));
    inbound.note_awaiting_headers();
    assert!(inbound.is_awaiting_headers());
    let deadline = rbitcoin_net::chain::headers_download_timeout_secs(now, best);
    hub.set_mock_now(deadline + 1);
    assert!(
        !inbound.stop.load(Ordering::SeqCst),
        "noban stall must keep the TCP session (via_cidr={via_cidr})"
    );
    assert!(
        !inbound.is_sync_started(),
        "noban timeout must end the stalling sync (via_cidr={via_cidr})"
    );
    assert!(
        !inbound.is_awaiting_headers(),
        "in-flight getheaders must be released so a replacement can send"
    );
    assert!(
        hub.try_start_headers_sync(&inbound, deadline + 1, best),
        "after timeout another getheaders start must be allowed"
    );
}

#[test]
fn snapshot_permissions_are_cidr_table_not_hub_noban() {
    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18444);
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445);
    hub.register(
        a,
        bind,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    hub.set_noban(true);
    assert!(
        hub.snapshot()[0].permissions.is_empty(),
        "hub --trusted is a DoS bypass, not getpeerinfo.permissions"
    );
    let mut t = rbitcoin_net::NetPermTable::default();
    t.whitelist
        .push(rbitcoin_net::parse_whitelist("noban,out@127.0.0.1").unwrap());
    hub.set_net_perms(t);
    assert_eq!(hub.snapshot()[0].permissions, ["noban", "download"]);
}

#[test]
fn cidr_noban_is_per_peer_not_hub_wide() {
    let hub = PeerHub::new();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445);
    let local = hub.register(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18444),
        bind,
        &ver("/rbitcoin:0.1.0/"),
        true,
        PeerConnType::Inbound,
    );
    let other = hub.register(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 18444),
        bind,
        &ver("/rbitcoin:0.1.0/"),
        true,
        PeerConnType::Inbound,
    );
    let mut t = rbitcoin_net::NetPermTable::default();
    t.whitelist
        .push(rbitcoin_net::parse_whitelist("noban@127.0.0.1").unwrap());
    hub.set_net_perms(t);
    assert!(!hub.is_noban(), "a CIDR grant must not set hub --trusted");
    assert!(local.session_noban());
    assert!(
        !other.session_noban(),
        "non-matching inbound must not inherit CIDR noban"
    );
    hub.set_noban(true);
    assert!(
        other.session_noban(),
        "operator --trusted still covers every session"
    );
}

#[test]
fn addnode_unknown_command() {
    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    assert!(hub.addnode(a, "nope").is_err());
}

fn take_dials(rx: &mut mpsc::UnboundedReceiver<DialRequest>) -> Vec<DialRequest> {
    let mut out = Vec::new();
    while let Ok(req) = rx.try_recv() {
        out.push(req);
    }
    out
}

#[test]
fn parse_peer_addr_localhost_and_default_port() {
    let with_port = parse_peer_addr_with_port("localhost:18444", None).expect("localhost:port");
    assert!(with_port.is_ipv4());
    assert_eq!(with_port.port(), 18444);
    let bare = parse_peer_addr_with_port("localhost", Some(18444)).expect("localhost default");
    assert!(bare.is_ipv4());
    assert_eq!(bare.port(), 18444);
    let lit = "127.0.0.1:18444".parse::<SocketAddr>().unwrap();
    assert_eq!(
        parse_peer_addr_with_port("127.0.0.1:18444", None).unwrap(),
        lit
    );
    assert!(parse_peer_addr_with_port("not-a-real-host.invalid", Some(18444)).is_err());
}

#[test]
fn addnode_add_keeps_unresolved_host_for_redial() {
    let hub = PeerHub::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.set_dialer(tx);
    hub.addnode_host("not-a-real-host.invalid", "add", 18444)
        .expect("unresolved add is remembered");
    assert!(take_dials(&mut rx).is_empty());
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9);
    hub.redial_remembered_with(|_| Ok(DialTarget::Socket(addr)));
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 1, "later resolve must dial: {got:?}");
    assert!(matches!(got[0].target, DialTarget::Socket(a) if a == addr));
    assert!(hub
        .addnode_host("not-a-real-host.invalid", "onetry", 18444)
        .is_err());
}

#[test]
fn redial_same_endpoint_in_addnode_and_connect_dials_once() {
    let hub = PeerHub::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.set_dialer(tx);
    hub.addnode_host("127.0.0.1:18444", "add", 18444).unwrap();
    assert_eq!(take_dials(&mut rx).len(), 1);
    hub.set_connect_hosts(vec!["127.0.0.1:18444".into()], 18444);
    hub.redial_remembered();
    let got = take_dials(&mut rx);
    assert_eq!(
        got.len(),
        1,
        "addnode and --connect of one endpoint share one dial per pass: {got:?}"
    );
    assert!(matches!(got[0].typ, PeerConnType::Manual));

    // A --connect host on its own redials as `manual` too (Core `-connect`).
    let hub = PeerHub::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.set_dialer(tx);
    hub.set_connect_hosts(vec!["127.0.0.1:18445".into()], 18444);
    hub.redial_remembered();
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(matches!(got[0].typ, PeerConnType::Manual), "{got:?}");
    // A queued request counts as in flight; dropping it ends that dial.
    drop(got);

    // A --connect address (IP or overlay) redials in the same pass. One
    // that meets a hostname at an endpoint shares its dial, and a live
    // session is not dialled again.
    let ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445);
    let onion: rbitcoin_net::NetAddr =
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333"
            .parse()
            .unwrap();
    hub.set_connect_addrs(vec![rbitcoin_net::NetAddr::Ip(ip), onion]);
    hub.redial_remembered();
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|d| d.typ == PeerConnType::Manual), "{got:?}");
    assert_eq!(got[0].target, DialTarget::Socket(ip), "{got:?}");
    assert_eq!(got[1].target, DialTarget::from_net(onion), "{got:?}");
    drop(got);
    let _live = hub.register(
        ip,
        ip,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::Manual,
    );
    hub.redial_remembered();
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 1, "only the onion has no live session: {got:?}");
    assert_eq!(got[0].target, DialTarget::from_net(onion), "{got:?}");
}

#[test]
fn addnode_skips_an_endpoint_that_is_connected_or_dialling() {
    let hub = PeerHub::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.set_dialer(tx);
    let ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18446);
    let live = hub.register(
        ip,
        ip,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    // Core `AlreadyConnectedToHost` / `AlreadyConnectedToAddressPort`.
    hub.addnode_host("127.0.0.1:18446", "onetry", 18444)
        .unwrap();
    hub.addnode_host("127.0.0.1:18446", "add", 18444).unwrap();
    hub.addnode(ip, "onetry").unwrap();
    let got = take_dials(&mut rx);
    assert!(
        got.is_empty(),
        "a connected endpoint is not dialled again: {got:?}"
    );

    let onion: rbitcoin_net::NetAddr =
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333"
            .parse()
            .unwrap();
    let dialing = hub.dial_in_flight(&DialTarget::from_net(onion));
    hub.addnode_host(&onion.to_string(), "onetry", 18444)
        .unwrap();
    let got = take_dials(&mut rx);
    assert!(got.is_empty(), "nor is one still dialling: {got:?}");
    drop(dialing);
    // A dial counts from the moment it is queued, before the dial task
    // picks it up: `onetry`, `add` and a redial pass right behind it
    // queue nothing more.
    hub.addnode_host(&onion.to_string(), "onetry", 18444)
        .unwrap();
    hub.addnode_host(&onion.to_string(), "add", 18444).unwrap();
    hub.redial_remembered();
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 1, "a queued dial is not queued again: {got:?}");
    assert_eq!(got[0].target, DialTarget::from_net(onion), "{got:?}");

    live.request_disconnect();
    hub.addnode_host("127.0.0.1:18446", "onetry", 18444)
        .unwrap();
    let got = take_dials(&mut rx);
    assert_eq!(got.len(), 1, "once the session ends, onetry dials: {got:?}");
    assert_eq!(got[0].target, DialTarget::Socket(ip), "{got:?}");
    assert_eq!(got[0].typ, PeerConnType::Manual, "{got:?}");
}

#[test]
fn a_queued_feeler_does_not_hold_off_a_manual_dial() {
    let hub = PeerHub::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    hub.set_dialer(tx);
    let ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18447);
    // A feeler's probe has no connect deadline. Counted in flight, a
    // stuck one would keep `addnode` and the redial pass off this
    // endpoint until it ended.
    hub.addconnection(ip, PeerConnType::Feeler).unwrap();
    let feeler = take_dials(&mut rx);
    assert_eq!(feeler.len(), 1, "{feeler:?}");
    assert_eq!(feeler[0].typ, PeerConnType::Feeler, "{feeler:?}");
    hub.addnode(ip, "onetry").unwrap();
    let got = take_dials(&mut rx);
    assert_eq!(
        got.len(),
        1,
        "a queued feeler does not block addnode: {got:?}"
    );
    assert_eq!(got[0].typ, PeerConnType::Manual, "{got:?}");
    drop(feeler);
}

#[tokio::test]
async fn slow_redial_resolve_does_not_stall_runtime() {
    let hub = PeerHub::new();
    hub.set_connect_hosts(vec!["slow.example".into()], 18444);
    let flag = Arc::new(AtomicBool::new(false));
    let flag2 = Arc::clone(&flag);
    let progress = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        flag2.store(true, Ordering::SeqCst);
    });
    let hub2 = Arc::clone(&hub);
    let slow = tokio::task::spawn_blocking(move || {
        hub2.redial_remembered_with(|_| {
            std::thread::sleep(std::time::Duration::from_millis(150));
            Err("slow".into())
        });
    });
    progress.await.unwrap();
    assert!(
        flag.load(Ordering::SeqCst),
        "a blocking resolve must not stall other Tokio tasks"
    );
    slow.await.unwrap();
}

/// Core runs the ping timeout only once `-peertimeout` has passed since
/// connect. Core's functional framework sets `peertimeout=999999999` so a
/// `setmocktime` jump cannot drop peers (`feature_bip68_sequence.py`).
#[test]
fn ping_timeout_waits_for_the_peer_timeout() {
    let hub = PeerHub::new();
    hub.set_mock_now(1_700_000_000);
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18444);
    let p = hub.register(a, a, &ver("/rbitcoin:0.1.0/"), true, PeerConnType::Inbound);
    let now = hub.now_secs();
    let Some(PingAction::Send { .. }) = p.take_ping_action(now) else {
        panic!("expected send");
    };
    hub.set_peer_timeout_secs(999_999_999);
    assert!(
        !matches!(
            p.take_ping_action(now + 6_000),
            Some(PingAction::Timeout { .. })
        ),
        "inside the peer timeout the ping does not time out"
    );
    hub.set_peer_timeout_secs(60);
    assert!(
        !matches!(
            p.take_ping_action(now + 20 * 60),
            Some(PingAction::Timeout { .. })
        ),
        "the ping timeout starts the second after twenty minutes"
    );
    assert!(matches!(
        p.take_ping_action(now + 20 * 60 + 1),
        Some(PingAction::Timeout { .. })
    ));

    let late = hub.register(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 18445),
        &ver("/rbitcoin:0.1.0/"),
        true,
        PeerConnType::Inbound,
    );
    let Some(PingAction::Send { .. }) = late.take_ping_action(now) else {
        panic!("expected send");
    };
    hub.set_peer_timeout_secs(2_000);
    assert!(
        !matches!(
            late.take_ping_action(now + 2_000),
            Some(PingAction::Timeout { .. })
        ),
        "inactivity checks start the second after the peer timeout"
    );
    assert!(matches!(
        late.take_ping_action(now + 2_001),
        Some(PingAction::Timeout { .. })
    ));
}

#[test]
fn pick_stale_follow_evict_none_on_empty() {
    assert!(pick_stale_follow_evict(&[], 7, &[]).is_none());
}

#[test]
fn pick_stale_follow_evict_picks_only_from_candidates() {
    let ids = [3u64, 9, 12];
    let groups = [1u64, 2, 3];
    for salt in 0..16u64 {
        let got = pick_stale_follow_evict(&ids, salt, &groups).unwrap();
        assert!(ids.contains(&got), "salt={salt} got={got}");
    }
    assert_eq!(pick_stale_follow_evict(&ids, 0, &groups), Some(3));
    assert_eq!(pick_stale_follow_evict(&ids, 1, &groups), Some(9));
    assert_eq!(pick_stale_follow_evict(&ids, 2, &groups), Some(12));
    assert_eq!(pick_stale_follow_evict(&ids, 3, &groups), Some(3));
}

#[test]
fn pick_stale_follow_evict_prefers_duplicate_group() {
    let ids = [1u64, 2, 3];
    let groups = [10u64, 10, 20];
    assert_eq!(pick_stale_follow_evict(&ids, 0, &groups), Some(1));
    assert_eq!(pick_stale_follow_evict(&ids, 1, &groups), Some(2));
    for salt in 0..16u64 {
        let got = pick_stale_follow_evict(&ids, salt, &groups).unwrap();
        assert!(got == 1 || got == 2, "salt={salt} got={got}");
    }
}

#[test]
fn outbound_full_relay_ids_skips_inbound_and_noban() {
    let hub = PeerHub::new();
    let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2);
    let c = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3);
    let _in = hub.register(a, a, &ver("/rbitcoin:0.1.0/"), true, PeerConnType::Inbound);
    let out = hub.register(
        b,
        b,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::OutboundFullRelay,
    );
    let _br = hub.register(
        c,
        c,
        &ver("/rbitcoin:0.1.0/"),
        false,
        PeerConnType::BlockRelay,
    );
    let ids = hub.outbound_full_relay_ids();
    assert_eq!(ids, vec![out.id]);
    hub.set_noban(true);
    assert!(
        hub.outbound_full_relay_ids().is_empty(),
        "noban hub must not offer rotate victims"
    );
}

fn fill_addrman(n: usize) -> rbitcoin_net::seeds::AddrMan {
    let mut am = rbitcoin_net::seeds::AddrMan::new();
    for i in 0..n {
        let a = (i >> 8) as u8;
        let b = (i & 0xff) as u8;
        am.add_with_flags(
            SocketAddr::from(([a, b, 1, 1], 8333)),
            rbitcoin_net::seeds::PeerFlags::empty(),
        );
    }
    am
}

fn addr_ips(v: &[(u32, Address)]) -> Vec<IpAddr> {
    v.iter()
        .filter_map(|(_, a)| a.socket_addr().ok().map(|s| s.ip()))
        .collect()
}

#[test]
fn getaddr_cache_bind_key_and_ttl() {
    let hub = PeerHub::new();
    hub.set_mock_now(1_700_000_000);
    hub.set_addrman(Arc::new(Mutex::new(fill_addrman(5_000))));
    let a = addr_ips(&hub.addr_response_for_bind(SocketAddr::from(([127, 0, 0, 1], 18444))));
    let b = addr_ips(&hub.addr_response_for_bind(SocketAddr::from(([127, 0, 0, 1], 18445))));
    let c = addr_ips(&hub.addr_response_for_bind(SocketAddr::from(([127, 0, 0, 1], 18446))));
    let mapped = addr_ips(&hub.addr_response_for_bind(SocketAddr::from((
        Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(),
        18444,
    ))));
    assert_eq!(a.len(), 1000);
    assert_eq!(b.len(), 1000);
    assert_eq!(c.len(), 1000);
    assert_eq!(
        a, mapped,
        "IPv4-mapped IPv6 must share the clearnet cache key"
    );
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(b, c);
    hub.set_mock_now(1_700_000_000 + 24 * 60 * 60);
    let expired = addr_ips(&hub.addr_response_for_bind(SocketAddr::from(([127, 0, 0, 1], 18444))));
    assert_eq!(expired.len(), 1000);
    assert_ne!(a, expired);
}

#[test]
fn getaddr_omits_unroutable_clearnet() {
    let hub = PeerHub::new();
    hub.set_mock_now(1_700_000_000);
    let mut am = rbitcoin_net::seeds::AddrMan::new();
    for addr in ["10.0.0.1:8333", "100.64.0.1:8333", "127.0.0.1:8333"] {
        assert!(am.add_learned(addr.parse().unwrap(), 64));
    }
    hub.set_addrman(Arc::new(Mutex::new(am)));
    let got = hub.addr_response_for_bind(SocketAddr::from(([127, 0, 0, 1], 18444)));
    assert!(
        got.is_empty(),
        "getaddr must not advertise unroutable clearnet: {got:?}"
    );
}

include!("overlay_addrman_journey.rs");
