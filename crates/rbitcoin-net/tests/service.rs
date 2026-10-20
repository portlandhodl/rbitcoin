use bitcoin::p2p::Magic;
use bitcoin::Block;
use bitcoin::BlockHash;
use rbitcoin_consensus::{signet_magic, ChainParams, Milestone};
use rbitcoin_net::cache::BlockCache;
use rbitcoin_net::chain::{AcceptOutcome, ChainHub};
use rbitcoin_net::error::NetError;
use rbitcoin_net::peer::{
    connect_and_handshake_timed, inbound_connect_and_handshake, peer_session_with,
    FollowSessionMeta, HandshakePolicy, HANDSHAKE_TIMEOUT,
};
use rbitcoin_net::peer_dos::{inbound_semaphore, DEFAULT_MAX_INBOUND};
use rbitcoin_net::peers::{DialRequest, DialTarget, LivePeer, PeerConnType, PeerHub};
use rbitcoin_net::service::*;
use rbitcoin_net::v2::{V2Reader, V2Writer};
use rbitcoin_query::Query;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[test]
fn magic_for_all_networks() {
    assert_eq!(
        magic_for_params(&ChainParams::mainnet()),
        Magic::from(bitcoin::Network::Bitcoin)
    );
    assert_eq!(
        magic_for_params(&ChainParams::testnet()),
        Magic::from(bitcoin::Network::Testnet)
    );
    assert_eq!(
        magic_for_params(&ChainParams::signet()),
        Magic::from(bitcoin::Network::Signet)
    );
    assert_eq!(magic_for_params(&ChainParams::regtest()), Magic::REGTEST);
}

#[test]
fn custom_signet_uses_challenge_derived_magic() {
    use bitcoin::ScriptBuf;

    let challenge = ScriptBuf::from_bytes(vec![0x51]);
    let params = ChainParams::custom_signet(challenge, 60).unwrap();
    assert_eq!(
        magic_for_params(&params),
        Magic::from_bytes([0x54, 0xd2, 0x6f, 0xbd])
    );
}

/// A feeler does not use `prepare_outbound_session`. It still must not
/// open a socket to an address that misbehaved.
#[tokio::test]
async fn feeler_does_not_dial_a_discouraged_address() {
    let peers = rbitcoin_net::peers::PeerHub::new();
    peers.set_mock_now(1_700_000_000);
    let addr = std::net::SocketAddr::from(([1, 2, 3, 4], 8333));
    peers.note_misbehavior_addr(addr.ip());
    let (dir, hub) = rbitcoin_net::chain::tiny_regtest_hub_labeled("feeler-discouraged");
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        run_outbound_session_with_abort(
            rbitcoin_net::peers::DialTarget::Socket(addr),
            None,
            Magic::REGTEST,
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            std::sync::Arc::new(hub),
            peers,
            "/rbitcoin:test/".into(),
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            rbitcoin_net::peers::PeerConnType::Feeler,
            rx,
            rbitcoin_net::socks::Dialer::Direct,
        ),
    )
    .await;
    let _ = std::fs::remove_dir_all(dir.path());
    let err = result
        .expect("a refused feeler returns before the connect")
        .unwrap_err();
    assert!(
        matches!(err, NetError::Protocol("discouraged")),
        "feeler dialed a discouraged address, got {err:?}"
    );
}

/// One datadir, two node lifetimes. The first is dropped without
/// shutdown. The second binds loopback as with onion listen, adds a
/// second listener, and shuts down with one session task that finishes
/// in the grace window and one that would linger.
#[tokio::test]
async fn service_listen_shutdown() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-p2p-listen-shutdown-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let node = P2PNode::start(
        "127.0.0.1:0".parse().unwrap(),
        Query::open_or_create_tiny(&dir).unwrap(),
        ChainParams::regtest(),
        Milestone::NONE,
    )
    .await
    .unwrap();
    assert!(node.hub.shared_arc().is_some());
    let t0 = std::time::Instant::now();
    drop(node);
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "dropping the node must abort connect-retry without waiting out the runtime"
    );

    let mut node = P2PNode::start_with_dialer(
        "127.0.0.1:0".parse().unwrap(),
        Query::open_or_create_tiny(&dir).unwrap(),
        ChainParams::regtest(),
        Milestone::NONE,
        "/rbitcoin:0.1.0(onion)/".into(),
        rbitcoin_net::DEFAULT_MAX_INBOUND,
        rbitcoin_net::socks::Dialer::Direct,
    )
    .await
    .unwrap();
    assert_eq!(node.local_addr.ip(), std::net::Ipv4Addr::LOCALHOST);
    assert_ne!(node.local_addr.port(), 0);
    let extra = node
        .add_listen("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    assert_ne!(extra, node.local_addr);
    assert!(std::net::TcpStream::connect(extra).is_ok());

    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    node.tasks.push(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        flag.store(true, Ordering::SeqCst);
    }));
    node.tasks.push(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }));
    let t0 = std::time::Instant::now();
    node.shutdown().await;
    assert!(
        done.load(Ordering::SeqCst),
        "shutdown lets a task that finishes within the grace window finish"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "shutdown must not wait out a 30s session task"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_outbound_only_dials_without_listener() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rbitcoin-outbound-only-{n}"));
    std::fs::create_dir_all(dir.join("seed")).unwrap();
    std::fs::create_dir_all(dir.join("follow")).unwrap();
    let qa = Query::open_or_create_tiny(dir.join("seed")).unwrap();
    let qb = Query::open_or_create_tiny(dir.join("follow")).unwrap();
    let params = ChainParams::regtest();
    let seeder = P2PNode::start_with_agent(
        "127.0.0.1:0".parse().unwrap(),
        qa,
        params.clone(),
        Milestone::NONE,
        "/rbitcoin:0.1.0(seed)/".into(),
        rbitcoin_net::DEFAULT_MAX_INBOUND,
    )
    .await
    .unwrap();
    let mut follower = P2PNode::start_outbound_only(
        qb,
        params,
        Milestone::NONE,
        "/rbitcoin:0.1.0(follow)/".into(),
        0,
        rbitcoin_net::socks::Dialer::Direct,
    )
    .await
    .unwrap();
    assert_eq!(
        follower.local_addr,
        "127.0.0.1:0".parse().unwrap(),
        "outbound-only must not bind a P2P port"
    );
    assert_eq!(follower.max_inbound, 0);
    let inbound_err = follower
        .peers
        .addconnection(seeder.local_addr, PeerConnType::Inbound)
        .unwrap_err();
    assert!(
        inbound_err.contains("inbound"),
        "addconnection inbound must refuse: {inbound_err}"
    );

    follower.follow_from(seeder.local_addr).await.unwrap();
    let mut linked = false;
    for _ in 0..100 {
        if follower.follow_live_count() >= 1 && !seeder.peers.snapshot().is_empty() {
            linked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    seeder.shutdown().await;
    follower.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(linked, "outbound-only follower must handshake the seeder");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listen_onion_loopback_inbound_handshake() {
    use bitcoin::p2p::Magic;
    use rbitcoin_net::peer::{connect_and_handshake_timed, HandshakePolicy};
    use tokio::net::TcpStream;

    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rbitcoin-listen-onion-hs-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    let q = Query::open_or_create_tiny(&dir).unwrap();
    let node = P2PNode::start_with_dialer(
        "127.0.0.1:0".parse().unwrap(),
        q,
        ChainParams::regtest(),
        Milestone::NONE,
        "/rbitcoin:0.1.0(onion)/".into(),
        rbitcoin_net::DEFAULT_MAX_INBOUND,
        rbitcoin_net::socks::Dialer::Direct,
    )
    .await
    .unwrap();
    let stream = TcpStream::connect(node.local_addr).await.unwrap();
    let _hs = connect_and_handshake_timed(
        Duration::from_secs(5),
        stream,
        Magic::REGTEST,
        node.local_addr,
        node.local_addr,
        0,
        false,
        "/rbitcoin:test/",
        HandshakePolicy::plain(),
    )
    .await
    .unwrap();
    let mut inbound = false;
    for _ in 0..200 {
        if node.peers.snapshot().iter().any(|p| p.inbound) {
            inbound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        inbound,
        "loopback client must show as inbound in getpeerinfo"
    );
}

/// The 2 s redial pass must not stack a dial that is still connecting,
/// and must not lose the target to one that never finishes. A SOCKS
/// proxy that accepts and never answers holds the first dial in
/// `connect_net`: later passes skip it until the connect times out, then
/// dial again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redial_skips_a_connect_target_still_dialling() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&accepts);
    let stall = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = proxy.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            held.push(sock);
        }
    });
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rbitcoin-redial-in-flight-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    let node = P2PNode::start_outbound_only(
        Query::open_or_create_tiny(&dir).unwrap(),
        ChainParams::regtest(),
        Milestone::NONE,
        "/rbitcoin:0.1.0(redial)/".into(),
        0,
        rbitcoin_net::socks::Dialer::socks(proxy_addr, false),
    )
    .await
    .unwrap();
    node.peers.set_connect_addrs(vec![rbitcoin_net::NetAddr::Ip(
        "127.0.0.1:18444".parse().unwrap(),
    )]);
    node.peers.redial_remembered();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while accepts.load(Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first dial never reached the proxy"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..3 {
        node.peers.redial_remembered();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "a dial still connecting must not be dialled again"
    );
    // The stalled connect times out and frees the target for the next pass.
    let deadline = std::time::Instant::now() + OUTBOUND_CONNECT_TIMEOUT * 3;
    while accepts.load(Ordering::SeqCst) < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "a connect that never finishes must time out and be redialled"
        );
        node.peers.redial_remembered();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stall.abort();
    drop(node);
    let _ = std::fs::remove_dir_all(&dir);
}
