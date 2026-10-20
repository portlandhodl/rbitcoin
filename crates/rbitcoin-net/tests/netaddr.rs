use bitcoin::p2p::address::{AddrV2, AddrV2Message};
use rbitcoin_net::error::NetError;
use rbitcoin_net::netaddr::*;
use sha3::{Digest, Sha3_256};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

#[test]
fn netaddr_onion_parse_roundtrip() {
    let s = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333";
    let a: NetAddr = s.parse().expect("valid v3 onion");
    assert_eq!(a.to_string(), s);
    assert_eq!(a, s.parse().unwrap());
    match a {
        NetAddr::Onion { pk, port } => {
            assert_eq!(
                pk,
                [
                    0x79, 0xbc, 0xc6, 0x25, 0x18, 0x4b, 0x05, 0x19, 0x49, 0x75, 0xc2, 0x8b, 0x66,
                    0xb6, 0x6b, 0x04, 0x69, 0xf7, 0xf6, 0x55, 0x6f, 0xb1, 0xac, 0x31, 0x89, 0xa7,
                    0x9b, 0x40, 0xdd, 0xa3, 0x2f, 0x1f,
                ]
            );
            assert_eq!(port, 8333);
        }
        NetAddr::Ip(_) | NetAddr::I2p { .. } | NetAddr::Cjdns { .. } => {
            panic!("expected onion")
        }
    }
    assert!(
        "qg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333"
            .parse::<NetAddr>()
            .is_err()
    );
    assert!(
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscry.onion:8333"
            .parse::<NetAddr>()
            .is_err()
    );
    assert!("short.onion:8333".parse::<NetAddr>().is_err());
    assert!("not-an-addr".parse::<NetAddr>().is_err());
}

#[test]
fn netaddr_ip_parse_roundtrip() {
    let s = "1.2.3.4:8333";
    let a: NetAddr = s.parse().expect("ipv4");
    assert_eq!(
        a,
        NetAddr::Ip(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 8333))
    );
    assert_eq!(a.to_string(), s);
}

#[test]
fn netaddr_i2p_addrv2_roundtrip() {
    let dest = [0x11u8; 32];
    let msg = AddrV2Message {
        time: 1,
        services: bitcoin::p2p::ServiceFlags::NETWORK,
        addr: AddrV2::I2p(dest),
        port: 8333,
    };
    let a = NetAddr::from_addrv2(&msg).expect("i2p addrv2");
    assert_eq!(a, NetAddr::I2p { dest, port: 8333 });
    assert_eq!(a.to_addrv2(), AddrV2::I2p(dest));
    let s = a.to_string();
    assert!(s.ends_with(".b32.i2p:8333"), "{s}");
    assert_eq!(s.parse::<NetAddr>().unwrap(), a);
    assert_eq!(a.network_label(), "i2p");
    assert_eq!(a.port(), 8333);
    assert!(a.socket_addr().is_none());
    let zeros = NetAddr::I2p {
        dest: [0u8; 32],
        port: 1,
    };
    assert_eq!(
        zeros.to_string(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.b32.i2p:1"
    );
    assert!("short.b32.i2p:1".parse::<NetAddr>().is_err());
}

/// Core SAM 3.1 advertises I2P with port 0. Other networks still reject port 0.
#[test]
fn netaddr_i2p_port_zero_is_core_sam() {
    let dest = [0x11u8; 32];
    let msg = AddrV2Message {
        time: 1,
        services: bitcoin::p2p::ServiceFlags::NETWORK,
        addr: AddrV2::I2p(dest),
        port: 0,
    };
    let a = NetAddr::from_addrv2(&msg).expect("core i2p port 0");
    assert_eq!(a, NetAddr::I2p { dest, port: 0 });
    assert_eq!(a.port(), 0);
    let s = a.to_string();
    assert!(s.ends_with(".b32.i2p:0"), "{s}");
    assert_eq!(s.parse::<NetAddr>().unwrap(), a);
    let onion = AddrV2Message {
        time: 1,
        services: bitcoin::p2p::ServiceFlags::NETWORK,
        addr: AddrV2::TorV3([0x22; 32]),
        port: 0,
    };
    assert!(NetAddr::from_addrv2(&onion).is_none());
}

#[test]
fn netaddr_cjdns_addrv2_roundtrip() {
    let ip = Ipv6Addr::new(0xfc00, 1, 2, 3, 4, 5, 6, 7);
    let msg = AddrV2Message {
        time: 1,
        services: bitcoin::p2p::ServiceFlags::NETWORK,
        addr: AddrV2::Cjdns(ip),
        port: 8333,
    };
    let a = NetAddr::from_addrv2(&msg).expect("cjdns addrv2");
    assert_eq!(a, NetAddr::Cjdns { ip, port: 8333 });
    assert_eq!(a.to_addrv2(), AddrV2::Cjdns(ip));
    assert_eq!(a.network_label(), "cjdns");
    assert_eq!(a.port(), 8333);
    assert_eq!(a.socket_addr(), Some(SocketAddr::from((ip, 8333))));
    let parsed: NetAddr = a.to_string().parse().unwrap();
    assert_eq!(parsed, a);
    assert!(OnlyNet::Cjdns.matches_addr(a));
    assert!(!OnlyNet::Ipv6.matches_addr(a));
}

#[test]
fn cjdns_rejects_global_unicast() {
    let ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
    let msg = AddrV2Message {
        time: 1,
        services: bitcoin::p2p::ServiceFlags::NETWORK,
        addr: AddrV2::Cjdns(ip),
        port: 8333,
    };
    assert!(NetAddr::from_addrv2(&msg).is_none());
}
