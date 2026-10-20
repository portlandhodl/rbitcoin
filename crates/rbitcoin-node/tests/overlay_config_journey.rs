fn assert_overlay_help() {
    let h = operator_usage();
    assert!(h.contains("--proxy") && h.contains("--onion") && h.contains("--proxy-randomize"));
    assert!(h.contains("--only-net") && !h.contains("--onlynet"));
    assert!(h.contains("--no-listen") && !h.contains("--nolisten"));
    assert!(h.contains("--tor-control") && h.contains("--tor-control-cookie"));
    assert!(h.contains("--tor-control-password") && !h.contains("--torcontrol"));
    assert!(h.contains("--i2p-sam") && !h.contains("--i2psam"));
    assert!(h.contains("--i2p-accept-incoming") && !h.contains("--i2pacceptincoming"));
}

fn proxy_onion_and_connects(c: &mut NodeConfig) {
    assert_eq!(c.listen.dialer(), rbitcoin_net::Dialer::Direct);
    assert!(c.listen.proxy_randomize);
    let empty = NodeConfig::default()
        .apply_kv("proxy", "")
        .unwrap_err()
        .to_string();
    assert!(empty.contains("proxy"), "{empty}");
    let bad = NodeConfig::default()
        .apply_kv("proxy", "not-an-addr")
        .unwrap_err()
        .to_string();
    assert!(bad.contains("proxy"), "{bad}");

    c.apply_kv("only_net", "onion").unwrap();
    let err = c.validate().unwrap_err().to_string();
    assert!(err.contains("SOCKS") && err.contains("only-net"), "{err}");
    c.apply_kv("proxy", "127.0.0.1:9050").unwrap();
    assert_eq!(c.listen.proxy, Some("127.0.0.1:9050".parse().unwrap()));
    match c.listen.dialer() {
        rbitcoin_net::Dialer::Socks {
            proxy, randomize, ..
        } => {
            assert_eq!(proxy, Some("127.0.0.1:9050".parse().unwrap()));
            assert!(randomize);
        }
        other => panic!("expected socks dialer, got {other:?}"),
    }
    c.validate().unwrap();
    c.apply_kv("onion", "127.0.0.1:9051").unwrap();
    assert_ne!(c.listen.proxy, c.listen.onion);
    match c.listen.dialer() {
        rbitcoin_net::Dialer::Socks { proxy, onion, .. } => {
            assert_eq!(proxy, c.listen.proxy);
            assert_eq!(onion, c.listen.onion);
        }
        other => panic!("expected split socks dialer, got {other:?}"),
    }

    let mut off = c.clone();
    off.apply_kv("proxy_randomize", "0").unwrap();
    match off.listen.dialer() {
        rbitcoin_net::Dialer::Socks { randomize, .. } => assert!(!randomize),
        other => panic!("expected socks dialer, got {other:?}"),
    }
    assert!(c.listen.proxy_randomize);

    c.apply_kv("connect", "1.2.3.4:8333").unwrap();
    c.apply_kv(
        "connect",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333",
    )
    .unwrap();
    assert_eq!(c.listen.connect.len(), 2);
    assert!(matches!(
        c.listen.connect[1],
        rbitcoin_net::NetAddr::Onion { port: 8333, .. }
    ));
    let bad_onion = NodeConfig::default()
        .apply_kv("connect", "short.onion:8333")
        .unwrap_err()
        .to_string();
    assert!(
        bad_onion.contains("connect") || bad_onion.contains("onion") || bad_onion.contains("bad"),
        "{bad_onion}"
    );
    c.apply_kv(
        "seed_node",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333",
    )
    .unwrap();
    assert_eq!(c.listen.seednodes.len(), 1);
    assert!(NodeConfig::default()
        .apply_kv("seed_node", "short.onion:8333")
        .is_err());
}

fn onion_only_does_not_socks_clearnet() {
    let onion_only = ready_config(["rbitcoin-node", "--onion", "127.0.0.1:9051"]);
    match onion_only.listen.dialer() {
        rbitcoin_net::Dialer::Socks { proxy, onion, .. } => {
            assert!(proxy.is_none(), "onion-only must not SOCKS clearnet");
            assert_eq!(onion, Some("127.0.0.1:9051".parse().unwrap()));
        }
        other => panic!("expected onion-only socks dialer, got {other:?}"),
    }
    match onion_only.listen.isolated_dialer() {
        rbitcoin_net::Dialer::Socks {
            proxy,
            onion,
            randomize,
            ..
        } => {
            assert!(proxy.is_none());
            assert_eq!(onion, Some("127.0.0.1:9051".parse().unwrap()));
            assert!(randomize, "isolated broadcast always randomizes SOCKS creds");
        }
        other => panic!("expected onion-only isolated dialer, got {other:?}"),
    }
}

fn hidden_services_and_cjdns(c: &mut NodeConfig) {
    c.apply_kv("only_net", "i2p").unwrap();
    let err = c.validate().unwrap_err().to_string();
    assert!(err.contains("SAM") && err.contains("i2p"), "{err}");
    c.apply_kv("i2p_sam", "").unwrap();
    c.apply_kv("i2p_accept_incoming", "1").unwrap();
    c.validate().unwrap();
    assert_eq!(c.listen.i2p_sam, Some("127.0.0.1:7656".parse().unwrap()));
    let mut quiet = c.clone();
    quiet.apply_kv("listen", "0").unwrap();
    let err = quiet.validate().unwrap_err().to_string();
    assert!(err.contains("--listen") && err.contains("i2p"), "{err}");
    quiet.apply_kv("listen_onion", "1").unwrap();
    assert_eq!(
        quiet.listen.start_p2p_bind(Network::Regtest),
        Some("127.0.0.1:0".parse().unwrap())
    );

    c.apply_kv("listen_onion", "1").unwrap();
    let err = c.validate().unwrap_err().to_string();
    assert!(err.contains("tor-control"), "{err}");
    c.apply_kv("tor_control", "").unwrap();
    c.apply_kv("tor_control_password", "pw").unwrap();
    assert_eq!(c.tor.control, Some("127.0.0.1:9051".parse().unwrap()));
    assert_eq!(c.tor.password.as_deref(), Some("pw"));
    c.validate().unwrap();
    let mut inbound = c.clone();
    inbound.apply_kv("max_inbound", "0").unwrap();
    let err = inbound.validate().unwrap_err().to_string();
    assert!(
        err.contains("listen-onion") && err.contains("max-inbound"),
        "{err}"
    );

    let mut before = c.clone();
    before
        .apply_kv("connect", "[fc00:1:2:3:4:5:6:7]:8333")
        .unwrap();
    assert!(matches!(
        before.listen.connect.last().unwrap(),
        rbitcoin_net::NetAddr::Cjdns { .. }
    ));
    let err = before.validate().unwrap_err().to_string();
    assert!(err.contains("cjdns-reachable"), "{err}");

    c.apply_kv("only_net", "cjdns").unwrap();
    let err = c.validate().unwrap_err().to_string();
    assert!(err.contains("cjdns-reachable"), "{err}");
    c.apply_kv("cjdns_reachable", "1").unwrap();
    c.apply_kv("connect", "[fc00:1:2:3:4:5:6:7]:8333")
        .unwrap();
    c.apply_kv("listen", "[fc00:1:2:3:4:5:6:7]:8333").unwrap();
    c.validate().unwrap();
    match c.listen.p2p {
        rbitcoin_node::config::P2pListen::Socket(a) => {
            assert_eq!(a.port(), 8333);
            let std::net::IpAddr::V6(ip) = a.ip() else {
                panic!("expected v6");
            };
            assert!(rbitcoin_net::is_cjdns_ip(ip), "{ip}");
        }
        other => panic!("expected socket listen, got {other:?}"),
    }
}

#[test]
fn overlay_config() {
    let _g = OPERATOR_ENV_TEST_LOCK.lock().unwrap();
    assert_overlay_help();
    onion_only_does_not_socks_clearnet();
    let mut c = NodeConfig::default();
    proxy_onion_and_connects(&mut c);
    hidden_services_and_cjdns(&mut c);
}
