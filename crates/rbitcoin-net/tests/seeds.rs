use bitcoin::p2p::ServiceFlags;
use rbitcoin_net::asmap::AsMap;
use rbitcoin_net::netaddr::{addr_allowed, NetAddr, OnlyNet};
use rbitcoin_net::netgroup::{netgroup, select_diverse};
use rbitcoin_net::seeds::*;
use rbitcoin_primitives::Network;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn addr(o: u8) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, o)), 8333)
}

#[test]
fn inject_drops_unroutable_and_add_learned_keeps_it() {
    use std::net::Ipv6Addr;
    let mut book = AddrMan::new();
    let public = addr(4);
    book.inject([
        "10.0.0.1:8333".parse().unwrap(),
        "127.0.0.1:8333".parse().unwrap(),
        "192.0.2.1:8333".parse().unwrap(),
        "0.0.0.0:8333".parse().unwrap(),
        "100.64.0.1:8333".parse().unwrap(),
        "198.18.0.1:8333".parse().unwrap(),
        "240.0.0.1:8333".parse().unwrap(),
        "[2001:db8::1]:8333".parse().unwrap(),
        "[2001:10::1]:8333".parse().unwrap(),
        "[2001:20::1]:8333".parse().unwrap(),
        "[100::1]:8333".parse().unwrap(),
        "0.1.2.3:8333".parse().unwrap(),
        "[2002:0a00:1::]:8333".parse().unwrap(),
        "[2002:0102:0304::1]:8333".parse().unwrap(),
        "[64:ff9b::a00:1]:8333".parse().unwrap(),
        "[::a00:1]:8333".parse().unwrap(),
        "[::ffff:0:a00:1]:8333".parse().unwrap(),
        "[::ffff:a00:1]:8333".parse().unwrap(),
        "[64:ff9b:1::a00:1]:8333".parse().unwrap(),
        "[64:ff9b:1:1::102:304]:8333".parse().unwrap(),
        "[2001:0:102:304::f5ff:fffe]:8333".parse().unwrap(),
        "[3fff::1]:8333".parse().unwrap(),
        "[2001:2::1]:8333".parse().unwrap(),
        "[2001:2:0:ffff:ffff:ffff:ffff:ffff]:8333".parse().unwrap(),
        public,
    ]);
    assert_eq!(book.len(), 1);
    assert!(book.entry(&public).is_some());
    let sixto4_public: SocketAddr = "[2002:0102:0304::]:8333".parse().unwrap();
    let nat64_public: SocketAddr = "[64:ff9b::102:304]:8333".parse().unwrap();
    let compatible_public: SocketAddr = "[::102:304]:8333".parse().unwrap();
    let siit_public: SocketAddr = "[::ffff:0:102:304]:8333".parse().unwrap();
    let nat64_local_public: SocketAddr = "[64:ff9b:1::102:304]:8333".parse().unwrap();
    let mapped_public: SocketAddr = "[::ffff:102:304]:8333".parse().unwrap();
    let bench_outside: SocketAddr = "[2001:2:1::1]:8333".parse().unwrap();
    book.inject([
        sixto4_public,
        nat64_public,
        compatible_public,
        siit_public,
        nat64_local_public,
        mapped_public,
        bench_outside,
    ]);
    assert!(
        book.entry(&sixto4_public).is_some(),
        "6to4 of a public IPv4 stays when the suffix is zero"
    );
    assert!(
        book.entry(&nat64_public).is_some(),
        "NAT64 of a public IPv4 stays"
    );
    assert!(book.entry(&compatible_public).is_some());
    assert!(book.entry(&siit_public).is_some());
    assert!(book.entry(&nat64_local_public).is_some());
    assert!(book.entry(&mapped_public).is_some());
    assert!(
        book.entry(&bench_outside).is_some(),
        "2001:2:1::/48 is outside the benchmarking prefix"
    );
    let lan: SocketAddr = "10.0.0.1:8333".parse().unwrap();
    assert!(book.add_learned(lan, 64));
    assert!(book.entry(&lan).is_some());
    let cjdns: SocketAddr =
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)), 8333);
    book.set_cjdns_reachable(true);
    book.inject([cjdns]);
    assert!(
        book.entries()
            .iter()
            .any(|e| matches!(e.addr, NetAddr::Cjdns { .. })),
        "fc00::/8 is CJDNS, not a unique-local bogon"
    );
}

fn addr_n(i: u32) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(
            11,
            ((i >> 16) & 0xff) as u8,
            ((i >> 8) & 0xff) as u8,
            (i & 0xff) as u8,
        )),
        8333,
    )
}

#[test]
fn dial_tier_preferred_vs_last_resort() {
    assert_eq!(PeerFlags::empty().dial_tier(), 0); // untried
    let mut f = PeerFlags::empty();
    f.insert(PeerFlags::HAS_CONNECTED);
    assert_eq!(f.dial_tier(), 0);
    f.insert(PeerFlags::FAST);
    assert_eq!(f.dial_tier(), 0);

    let mut slow = PeerFlags::empty();
    slow.insert(PeerFlags::HAS_CONNECTED);
    slow.insert(PeerFlags::SLOW);
    assert_eq!(slow.dial_tier(), 1);

    let mut bad = PeerFlags::empty();
    bad.insert(PeerFlags::FAILED_LAST_CONNECT);
    assert_eq!(bad.dial_tier(), 2);
    let mut inc = PeerFlags::empty();
    inc.insert(PeerFlags::INCOMPATIBLE);
    assert_eq!(inc.dial_tier(), 2);
}

#[test]
fn speed_sample_sets_fast_or_slow() {
    let mut f = PeerFlags::empty();
    f.apply_speed_sample(50, PeerFlags::FAST_BPS_MIN + 1);
    assert!(f.is_fast());
    assert!(!f.is_slow());
    f.apply_speed_sample(300, 1000);
    assert!(f.is_slow());
    assert!(!f.is_fast());
}

#[test]
fn take_dial_prefers_untried_and_good_over_failed() {
    let mut am = AddrMan::new();
    let good = addr(1);
    let untried = addr(2);
    let failed = addr(3);
    let incompat = addr(4);
    am.add(failed);
    am.add(incompat);
    am.add(good);
    am.add(untried);
    am.note_connected(good);
    am.note_connect_failed(failed, false);
    am.note_connect_failed(incompat, true);

    let got = am.take_dial_candidates(4, &HashSet::new(), &[]);
    assert_eq!(got.len(), 3);
    assert!(!got.contains(&incompat));
    let tiers: Vec<u8> = got.iter().map(|a| am.flags(a).dial_tier()).collect();
    assert_eq!(tiers[0], 0);
    assert_eq!(tiers[1], 0);
    assert_eq!(tiers[2], 2);
    assert!(am.flags(&got[2]).failed_last_connect());
}

#[test]
fn take_dial_skips_incompatible_while_good_remain() {
    let mut am = AddrMan::new();
    let good = addr(1);
    let untried = addr(2);
    let failed = addr(3);
    let incompat_a = addr(4);
    let incompat_b = addr(5);
    am.add(good);
    am.add(untried);
    am.add(failed);
    am.add(incompat_a);
    am.add(incompat_b);
    am.note_connected(good);
    am.note_connect_failed(failed, false);
    am.note_connect_failed(incompat_a, true);
    am.note_connect_failed(incompat_b, true);

    let got = am.take_dial_candidates(48, &HashSet::new(), &[]);
    assert!(
        got.iter().all(|a| !am.flags(a).is_incompatible()),
        "INCOMPATIBLE must not fill the batch while any other addr remains: {got:?}"
    );
    assert!(got.contains(&good));
    assert!(got.contains(&untried));
    assert!(got.contains(&failed), "FAILED_LAST_CONNECT stays retryable");
    assert_eq!(got.len(), 3);
}

#[test]
fn take_dial_incompatible_when_nothing_else() {
    let mut am = AddrMan::new();
    let a = addr(1);
    let b = addr(2);
    am.add(a);
    am.add(b);
    am.note_connect_failed(a, true);
    am.note_connect_failed(b, true);
    let got = am.take_dial_candidates(48, &HashSet::new(), &[]);
    assert_eq!(got.len(), 2);
    assert!(got.contains(&a));
    assert!(got.contains(&b));
}

#[test]
fn exclude_skips_blocked() {
    let mut am = AddrMan::new();
    am.add(addr(1));
    am.add(addr(2));
    let mut ex = HashSet::new();
    ex.insert(addr(1));
    let got = am.take_dial_candidates(10, &ex, &[]);
    assert_eq!(got, vec![addr(2)]);
}

fn slash16(a: u8, b: u8, host: u8) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, 0, host)), 8333)
}

#[test]
fn take_dial_same_slash16_still_fills() {
    let mut am = AddrMan::new();
    for i in 1..=16 {
        am.add(slash16(1, 2, i));
    }
    let got = am.take_dial_candidates(16, &HashSet::new(), &[]);
    assert_eq!(got.len(), 16);
}

#[test]
fn take_dial_mixed_groups_picks_distinct() {
    let mut am = AddrMan::new();
    for g in 1u8..=32 {
        am.add(slash16(g, 0, 1));
    }
    let got = am.take_dial_candidates(16, &HashSet::new(), &[]);
    assert_eq!(got.len(), 16);
    let groups: HashSet<u64> = got.iter().map(|a| netgroup(*a, None)).collect();
    assert_eq!(groups.len(), 16);
}

#[test]
fn take_dial_skips_occupied_group() {
    let mut am = AddrMan::new();
    am.add(slash16(1, 2, 1));
    am.add(slash16(1, 3, 1));
    let occupied = [slash16(1, 2, 9)];
    let got = am.take_dial_candidates(1, &HashSet::new(), &occupied);
    assert_eq!(got, vec![slash16(1, 3, 1)]);
}

#[test]
fn take_dial_occupied_group_tier0_beats_unused_group_last_resort() {
    let mut am = AddrMan::new();
    let good_same = slash16(1, 2, 1);
    let lemon = slash16(9, 9, 1);
    am.add(good_same);
    am.add(lemon);
    am.note_connected(good_same);
    am.note_connect_failed(lemon, false);
    let occupied = [slash16(1, 2, 9)];
    let got = am.take_dial_candidates(1, &HashSet::new(), &occupied);
    assert_eq!(
            got,
            vec![good_same],
            "diversity must not pick a last-resort unused group ahead of a preferred occupied-group addr"
        );
}

#[test]
fn take_dial_skips_recent_attempt_while_others_remain() {
    let mut am = AddrMan::new();
    am.add(addr(1));
    am.add(addr(2));
    am.note_attempt(addr(1));
    let got = am.take_dial_candidates(2, &HashSet::new(), &[]);
    assert_eq!(got, vec![addr(2)]);

    let mut only = AddrMan::new();
    only.add(addr(1));
    only.note_attempt(addr(1));
    assert_eq!(
        only.take_dial_candidates(1, &HashSet::new(), &[]),
        vec![addr(1)],
        "sole remaining addr is still dialed even if recently attempted"
    );

    let mut aged = AddrMan::new();
    aged.add(addr(1));
    aged.add(addr(2));
    let old = Instant::now()
        .checked_sub(DIAL_ATTEMPT_RECENT + Duration::from_secs(1))
        .expect("clock");
    aged.note_attempt_at(addr(1), old);
    let got = aged.take_dial_candidates(2, &HashSet::new(), &[]);
    assert_eq!(got.len(), 2);
    assert!(got.contains(&addr(1)));
    assert!(got.contains(&addr(2)));

    let now = Instant::now();
    let mut edge = AddrMan::new();
    edge.add(addr(1));
    edge.note_attempt_at(
        addr(1),
        now.checked_sub(DIAL_ATTEMPT_RECENT).expect("clock"),
    );
    assert!(
        !edge.recently_attempted(addr(1), now),
        "an attempt exactly ten minutes old is not recent"
    );
    edge.note_attempt_at(
        addr(1),
        now.checked_sub(DIAL_ATTEMPT_RECENT)
            .expect("clock")
            .checked_add(Duration::from_secs(1))
            .expect("clock"),
    );
    assert!(edge.recently_attempted(addr(1), now));
}

#[test]
fn add_learned_evicts_failed_when_at_cap() {
    let mut am = AddrMan::new();
    for i in 1..=3 {
        am.add(addr(i));
        am.note_connect_failed(addr(i), i == 1);
    }
    assert!(am.add_learned(addr(9), 3));
    assert_eq!(am.len(), 3);
    assert!(am.entry(&addr(9)).is_some());
    assert!(
        am.entry(&addr(1)).is_none(),
        "incompatible is evicted before failed-last-connect"
    );
}

#[test]
fn add_learned_keeps_good_when_full() {
    let mut am = AddrMan::new();
    for i in 1..=3 {
        am.add(addr(i));
        am.note_connected(addr(i));
    }
    assert!(!am.add_learned(addr(9), 3));
    assert_eq!(am.len(), 3);
    assert!(am.entry(&addr(9)).is_none());
    assert!(!am.add_learned(addr(1), 3));
}

#[test]
fn add_learned_evicts_oldest_new_when_at_cap() {
    let mut am = AddrMan::new();
    for i in 1..=3 {
        am.add(addr(i));
    }
    assert!(am.add_learned(addr(9), 3));
    assert_eq!(am.len(), 3);
    assert!(am.entry(&addr(9)).is_some());
    assert!(
        am.entry(&addr(1)).is_none(),
        "oldest new is evicted after incompat/failed"
    );
    assert!(am.entry(&addr(2)).is_some());
    assert!(am.entry(&addr(3)).is_some());
}

#[test]
fn add_learned_fills_past_getaddr_cache_pct() {
    const NEED: usize =
        rbitcoin_net::peer::MAX_ADDR_TO_SEND * 100 / rbitcoin_net::peer::MAX_PCT_ADDR_TO_SEND;
    let mut am = AddrMan::new();
    for i in 0..=NEED {
        assert!(
            am.add_learned(addr_n(i as u32), MAX_ADDR_MAN),
            "MAX_ADDR_MAN={} rejected insert {i} (need len > {NEED})",
            MAX_ADDR_MAN
        );
    }
    assert!(am.len() > NEED, "len={} need > {NEED}", am.len());
}

#[test]
fn add_keeps_tried_and_may_exceed_cap() {
    let mut am = AddrMan::new();
    for i in 0..MAX_ADDR_MAN {
        let a = addr_n(i as u32);
        am.add(a);
        am.note_connected(a);
    }
    let extra = addr_n(MAX_ADDR_MAN as u32);
    am.add(extra);
    assert_eq!(am.len(), MAX_ADDR_MAN + 1);
    assert!(am.entry(&extra).is_some());
    assert!(am.entry(&addr_n(0)).is_some());
    assert!(!am.add_learned(addr_n(MAX_ADDR_MAN as u32 + 1), MAX_ADDR_MAN));
    assert_eq!(am.len(), MAX_ADDR_MAN + 1);
    assert!(am.entry(&extra).is_some());
}

#[test]
fn add_evicts_oldest_new_at_cap() {
    let mut am = AddrMan::new();
    for i in 0..MAX_ADDR_MAN {
        am.add(addr_n(i as u32));
    }
    let extra = addr_n(MAX_ADDR_MAN as u32);
    am.add(extra);
    assert_eq!(am.len(), MAX_ADDR_MAN);
    assert!(am.entry(&extra).is_some());
    assert!(am.entry(&addr_n(0)).is_none());
    assert!(am.entry(&addr_n(1)).is_some());
}

#[test]
fn merge_from_trims_to_cap_keeping_tried() {
    let mut a = AddrMan::new();
    for i in 0..MAX_ADDR_MAN {
        let x = addr_n(i as u32);
        a.add(x);
        a.note_connected(x);
    }
    let mut b = AddrMan::new();
    for i in 0..8 {
        b.add(addr_n(MAX_ADDR_MAN as u32 + i));
    }
    a.merge_from(&b);
    assert_eq!(a.len(), MAX_ADDR_MAN);
    assert!(
        a.entry(&addr_n(0)).is_some(),
        "tried addrs must survive merge trim"
    );
    assert!(
        a.entry(&addr_n(MAX_ADDR_MAN as u32)).is_none(),
        "extra new from the other book must not grow past cap"
    );
}

#[test]
fn load_trims_tried_then_new_to_cap() {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-peers-cap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("peers");
    let mut body = String::from("rbitcoin-peers-v1\n");
    let n_tried = 2000u32;
    let n_total = MAX_ADDR_MAN as u32 + 904;
    for i in 0..n_tried {
        body.push_str(&format!("{} 0x01\n", addr_n(i)));
    }
    for i in n_tried..n_total {
        body.push_str(&format!("{} 0x00\n", addr_n(i)));
    }
    std::fs::write(&path, body).unwrap();
    let loaded = AddrMan::load(&path).unwrap();
    assert_eq!(loaded.len(), MAX_ADDR_MAN);
    for i in 0..n_tried {
        assert!(
            loaded.entry(&addr_n(i)).is_some(),
            "tried {i} must survive trim"
        );
        assert!(loaded.flags(&addr_n(i)).has_connected());
    }
    let n_new_kept = MAX_ADDR_MAN - n_tried as usize;
    assert!(loaded.entry(&addr_n(n_tried)).is_some());
    assert!(loaded
        .entry(&addr_n(n_tried + n_new_kept as u32 - 1))
        .is_some());
    assert!(loaded.entry(&addr_n(n_total - 1)).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn peers_file_roundtrip_preserves_flags() {
    let dir = std::env::temp_dir().join(format!("rbitcoin-peers-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("peers");
    let mut am = AddrMan::new();
    let a = addr(10);
    let b = addr(20);
    am.add(a);
    am.note_connected(a);
    am.note_speed(a, 40, PeerFlags::FAST_BPS_MIN + 100);
    am.add(b);
    am.note_connect_failed(b, true);
    am.save(&path).unwrap();

    let loaded = AddrMan::load(&path).unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded.flags(&a).has_connected());
    assert!(loaded.flags(&a).is_fast());
    assert!(loaded.flags(&b).is_incompatible());

    // merge does not wipe flags when re-adding seed
    let mut merged = loaded;
    merged.add(a);
    assert!(merged.flags(&a).is_fast());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seed_lookup_names_x_filter_before_unfiltered() {
    let names = seed_lookup_names(Network::Mainnet);
    assert!(!names.is_empty());
    assert_eq!(names[0][0], "x809.seed.bitcoin.sipa.be");
    assert_eq!(names[0][1], "seed.bitcoin.sipa.be");
    for pair in &names {
        assert_eq!(pair.len(), 2);
        assert!(pair[0].starts_with("x809."), "{pair:?}");
        assert!(!pair[1].starts_with("x809."), "{pair:?}");
    }
    let signet = seed_lookup_names(Network::Signet);
    assert_eq!(
        signet,
        vec![vec![
            "x809.seed.signet.bitcoin.sprovoost.nl".to_string(),
            "seed.signet.bitcoin.sprovoost.nl".to_string(),
        ]]
    );
    assert!(seed_lookup_names(Network::Regtest).is_empty());
}

#[test]
fn pick_seed_results_first_success_wins() {
    let x = addr(1);
    let plain = addr(2);
    assert_eq!(pick_seed_results(&[x], &[plain]), vec![x]);
    assert_eq!(pick_seed_results(&[], &[plain]), vec![plain]);
    assert!(pick_seed_results(&[], &[]).is_empty());
    assert_eq!(pick_seed_results(&[x, addr(3)], &[plain]), vec![x, addr(3)]);
}

#[test]
fn dns_seed_query_host_matches_core_x_filter() {
    use bitcoin::p2p::ServiceFlags;
    let bits = required_seed_services();
    assert!(bits.has(ServiceFlags::NETWORK));
    assert!(bits.has(ServiceFlags::WITNESS));
    assert!(bits.has(ServiceFlags::P2P_V2));
    assert_eq!(bits.to_u64(), 0x809);
    assert_eq!(
        dns_seed_query_host("seed.bitcoin.sipa.be", bits),
        "x809.seed.bitcoin.sipa.be"
    );
    assert_eq!(
        dns_seed_query_host("seed.signet.bitcoin.sprovoost.nl", bits),
        "x809.seed.signet.bitcoin.sprovoost.nl"
    );
}

#[test]
fn network_ports_and_seed_lists() {
    assert_eq!(default_port(Network::Mainnet), 8333);
    assert_eq!(default_port(Network::Testnet), 18333);
    assert_eq!(default_port(Network::Signet), 38333);
    assert_eq!(default_port(Network::Regtest), 18444);
    assert_eq!(default_rpc_port(Network::Mainnet), 8332);
    assert_eq!(default_rpc_port(Network::Testnet), 18332);
    assert_eq!(default_rpc_port(Network::Signet), 38332);
    assert_eq!(default_rpc_port(Network::Regtest), 18443);
    assert!(!dns_seeds(Network::Mainnet).is_empty());
    assert!(!dns_seeds(Network::Testnet).is_empty());
    assert_eq!(dns_seeds(Network::Signet).len(), 1);
    assert!(dns_seeds(Network::Regtest).is_empty());
    assert!(!fixed_seed_hosts(Network::Mainnet).is_empty());
    assert!(!fixed_seed_hosts(Network::Testnet).is_empty());
    assert!(!fixed_seed_hosts(Network::Signet).is_empty());
    assert!(fixed_seed_hosts(Network::Regtest).is_empty());
    // Regtest has no seeds → resolve paths stay empty (no network I/O).
    assert!(resolve_dns_seeds(Network::Regtest).is_empty());
    assert!(resolve_fixed_seeds(Network::Regtest).is_empty());
    assert!(resolve_all_seeds(Network::Regtest).is_empty());
}

#[test]
fn dns_seeds_not_resolved_locally_when_proxy() {
    for net in [
        Network::Mainnet,
        Network::Testnet,
        Network::Signet,
        Network::Regtest,
    ] {
        let dests = socks_dns_seed_dests(net);
        let names = dns_seeds(net);
        assert_eq!(dests.len(), names.len());
        for ((host, port), want) in dests.iter().zip(names.iter()) {
            assert_eq!(host, want);
            assert_eq!(*port, default_port(net));
            assert!(
                host.parse::<std::net::IpAddr>().is_err(),
                "SOCKS seed dest must stay a hostname, got {host}"
            );
        }
    }
}

#[test]
fn peer_flags_set_remove_and_mid_range_speed() {
    let mut f = PeerFlags::empty();
    assert!(f.is_untried());
    f.set(PeerFlags::HAS_CONNECTED, true);
    assert!(f.has_connected());
    f.set(PeerFlags::HAS_CONNECTED, false);
    assert!(!f.has_connected());
    // Mid-range sample leaves prior classification alone.
    f.insert(PeerFlags::FAST);
    f.apply_speed_sample(150, PeerFlags::FAST_BPS_MIN / 2);
    assert!(f.is_fast());
    assert!(!f.is_slow());
}

#[test]
fn note_ibd_slow_clears_fast_and_is_tier_one() {
    let mut am = AddrMan::new();
    let a = addr(11);
    am.note_speed(a, 40, PeerFlags::FAST_BPS_MIN + 100);
    assert!(am.flags(&a).is_fast());
    assert_eq!(am.flags(&a).dial_tier(), 0);
    am.note_ibd_slow(a);
    assert!(am.flags(&a).is_slow(), "IBD outlier must be SLOW");
    assert!(!am.flags(&a).is_fast(), "IBD outlier must not keep FAST");
    assert_eq!(am.flags(&a).dial_tier(), 1);
}

#[test]
fn apply_ibd_dead_speed_outlier_mid_range_does_not_restore_fast() {
    let mut am = AddrMan::new();
    let a = addr(12);
    am.note_speed(a, 40, PeerFlags::FAST_BPS_MIN + 100);
    assert!(am.flags(&a).is_fast());
    // Relative-slow kick sample is often 150–500 KB/s (above SLOW_BPS_MAX).
    am.apply_ibd_dead_speed(a, 150, Some(300_000), true);
    assert!(
        am.flags(&a).is_slow() && !am.flags(&a).is_fast(),
        "cooldown Dead mid-range must not restore FAST: {:?}",
        am.flags(&a)
    );
    assert_eq!(am.flags(&a).dial_tier(), 1);
}

#[test]
fn apply_ibd_dead_speed_without_outlier_keeps_mid_range_fast() {
    let mut am = AddrMan::new();
    let a = addr(13);
    am.note_speed(a, 40, PeerFlags::FAST_BPS_MIN + 100);
    am.apply_ibd_dead_speed(a, 150, Some(300_000), false);
    assert!(
        am.flags(&a).is_fast() && !am.flags(&a).is_slow(),
        "graceful Dead mid-range still leaves prior FAST"
    );
}

#[test]
fn addrman_merge_offset_and_ipv4_sort() {
    use std::net::{IpAddr, Ipv6Addr};
    let v4 = addr(1);
    let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8333);
    let mut a = AddrMan::new();
    a.add(v6);
    a.add(v4);
    a.inject([v6, v4]); // already present no-op + re-sort
    assert_eq!(a.peers()[0], v4, "IPv4 preferred over IPv6");

    let mut b = AddrMan::new();
    b.add_with_flags(addr(9), {
        let mut f = PeerFlags::empty();
        f.insert(PeerFlags::SLOW);
        f
    });
    a.merge_from(&b);
    assert!(a.flags(&addr(9)).is_slow());
    assert!(a.entry(&addr(9)).is_some());
    assert!(!a.is_empty());

    // Ranked outbound + offset wrap.
    let ranked = a.take_outbound(2);
    assert_eq!(ranked.len(), 2);
    let offset = a.take_outbound_offset(3, 1);
    assert_eq!(offset.len(), 3.min(a.len()));
    assert!(a.take_outbound_offset(0, 0).is_empty());
    assert!(a.take_dial_candidates(0, &HashSet::new(), &[]).is_empty());
}

#[test]
fn peers_file_load_errors_and_empty() {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-peers-err-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let missing = dir.join("no-such-peers");
    assert!(AddrMan::load(&missing).unwrap().is_empty());

    // Bad magic.
    let bad = dir.join("bad");
    std::fs::write(&bad, "not-magic\n1.2.3.4:8333 0\n").unwrap();
    assert!(AddrMan::load(&bad).is_err());

    // Comment-only / empty after trim → empty book.
    let emptyish = dir.join("emptyish");
    std::fs::write(&emptyish, "# just a comment\n\n").unwrap();
    assert!(AddrMan::load(&emptyish).unwrap().is_empty());

    // Hex + decimal flags parse.
    let ok = dir.join("ok");
    std::fs::write(
        &ok,
        format!(
            "{}\n{} 0x03\n{} 16\n# trailing\n",
            AddrMan::PEERS_FILE_MAGIC,
            addr(1),
            addr(2)
        ),
    )
    .unwrap();
    let loaded = AddrMan::load(&ok).unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded.flags(&addr(1)).has_connected());
    assert!(loaded.flags(&addr(1)).is_fast()); // bits 0|1
    assert!(loaded.flags(&addr(2)).failed_last_connect()); // bit 4 = 16

    // Bad address token.
    let bad_addr = dir.join("bad-addr");
    std::fs::write(
        &bad_addr,
        format!("{}\nnot-an-addr 0\n", AddrMan::PEERS_FILE_MAGIC),
    )
    .unwrap();
    assert!(AddrMan::load(&bad_addr).is_err());

    // Bad hex flags.
    let bad_hex = dir.join("bad-hex");
    std::fs::write(
        &bad_hex,
        format!("{}\n{} 0xZZ\n", AddrMan::PEERS_FILE_MAGIC, addr(1)),
    )
    .unwrap();
    assert!(AddrMan::load(&bad_hex).is_err());

    // Bad decimal flags + 0X uppercase hex prefix path.
    let bad_dec = dir.join("bad-dec");
    std::fs::write(
        &bad_dec,
        format!("{}\n{} not-a-number\n", AddrMan::PEERS_FILE_MAGIC, addr(1)),
    )
    .unwrap();
    assert!(AddrMan::load(&bad_dec).is_err());

    let ok_upper = dir.join("ok-upper");
    std::fs::write(
        &ok_upper,
        format!("{}\n{} 0X01\n", AddrMan::PEERS_FILE_MAGIC, addr(7)),
    )
    .unwrap();
    let u = AddrMan::load(&ok_upper).unwrap();
    assert!(u.flags(&addr(7)).has_connected());

    // Save creates parent dir; note_connected clears fail/incompat bits.
    let nested = dir.join("nested").join("peers");
    let mut am = AddrMan::new();
    am.add(addr(3));
    am.note_connect_failed(addr(3), true);
    assert!(am.flags(&addr(3)).is_incompatible());
    am.note_connected(addr(3));
    assert!(am.flags(&addr(3)).has_connected());
    assert!(!am.flags(&addr(3)).is_incompatible());
    am.save(&nested).unwrap();
    assert!(nested.is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn peers_file_v1_ipv4_still_loads() {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-peers-v1-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("peers");
    std::fs::write(&path, format!("rbitcoin-peers-v1\n{} 0x01\n", addr(4))).unwrap();
    let loaded = AddrMan::load(&path).unwrap();
    assert!(loaded.entry(&addr(4)).is_some());
    assert!(loaded.flags(&addr(4)).has_connected());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_drops_unroutable_clearnet_and_keeps_cjdns() {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-peers-bogon-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("peers");
    std::fs::write(
        &path,
        "\
rbitcoin-peers-v2
1.2.3.4:8333 0x01
10.0.0.1:8333 0x01
100.64.0.1:8333 0x00
[fc00::1]:8333 0x00
",
    )
    .unwrap();
    let loaded = AddrMan::load(&path).unwrap();
    let public: SocketAddr = "1.2.3.4:8333".parse().unwrap();
    assert!(
        loaded.entry(&public).is_some(),
        "a public address still loads"
    );
    assert!(loaded.flags(&public).has_connected());
    let lan: SocketAddr = "10.0.0.1:8333".parse().unwrap();
    assert!(
        loaded.entry(&lan).is_none(),
        "a remembered RFC1918 address is not dialed again"
    );
    let cgnat: SocketAddr = "100.64.0.1:8333".parse().unwrap();
    assert!(loaded.entry(&cgnat).is_none());
    assert!(
        loaded
            .entries()
            .iter()
            .any(|e| matches!(e.addr, NetAddr::Cjdns { .. })),
        "fc00::/8 stays a CJDNS row"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn onion() -> NetAddr {
    "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333"
        .parse()
        .unwrap()
}

fn i2p() -> NetAddr {
    NetAddr::I2p {
        dest: [7u8; 32],
        port: 8333,
    }
}

fn cjdns() -> NetAddr {
    NetAddr::Cjdns {
        ip: "fc00::1".parse().unwrap(),
        port: 8333,
    }
}

#[test]
fn add_with_flags_drops_a_net_only_net_excludes() {
    let mut am = AddrMan::new();
    am.set_only_net(vec![OnlyNet::Onion]);
    am.set_onion_reachable(true);
    am.add_with_flags_addr(NetAddr::from_socket(addr(1)), PeerFlags::empty());
    assert!(
        am.is_empty(),
        "a peers-file merge must not keep clearnet in an onion-only book"
    );
}

#[test]
fn learned_cjdns_stays_out_until_the_node_can_dial_it() {
    let mut am = AddrMan::new();
    assert!(!am.add_learned_addr(cjdns(), 8));
    assert!(am.is_empty());
    am.set_cjdns_reachable(true);
    assert!(am.add_learned_addr(cjdns(), 8));
}

#[test]
fn onion_and_i2p_stay_out_until_this_process_can_dial_them() {
    let mut am = AddrMan::new();
    am.add_addr(onion());
    assert!(!am.add_learned_addr(i2p(), 8));
    am.add_with_flags_addr(onion(), PeerFlags::empty());
    assert!(
        am.is_empty(),
        "no SOCKS and no SAM means those rows are not peers"
    );
    am.set_onion_reachable(true);
    am.add_addr(onion());
    assert!(am.entries().iter().any(|e| e.addr == onion()));
    am.set_i2p_reachable(true);
    assert!(am.add_learned_addr(i2p(), 8));
}

#[test]
fn turning_a_net_off_drops_rows_already_in_the_book() {
    let mut am = AddrMan::new();
    am.set_onion_reachable(true);
    am.set_i2p_reachable(true);
    am.set_cjdns_reachable(true);
    am.add_addr(onion());
    am.add_addr(i2p());
    am.add_addr(cjdns());
    am.add(addr(1));
    assert_eq!(am.len(), 4);
    am.set_only_net(vec![OnlyNet::Onion]);
    assert_eq!(
        am.len(),
        1,
        "only-net must drop rows this process will not dial"
    );
    assert!(am.entries().iter().all(|e| e.addr == onion()));
    am.set_onion_reachable(false);
    assert!(
        am.is_empty(),
        "clearing SOCKS drops onion rows already stored"
    );
}

#[test]
fn full_clearnet_batch_does_not_reserve_an_unreachable_overlay() {
    let mut am = AddrMan::new();
    for i in 1..=8u8 {
        am.add(addr(i));
    }
    am.add_addr(onion());
    am.add_addr(i2p());
    let got = am.take_dial_candidates_net(8, &HashSet::new(), &[]);
    assert_eq!(got.len(), 8, "{got:?}");
    assert!(
        got.iter().all(|a| matches!(a, NetAddr::Ip(_))),
        "an onion this process cannot dial must not take a clearnet slot: {got:?}"
    );
}
