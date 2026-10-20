use rbitcoin_net::eviction::*;

fn cand(
    id: u64,
    connected_at: u64,
    min_ping: Option<f64>,
    last_block: u64,
    last_tx: u64,
) -> InboundEvictCandidate {
    InboundEvictCandidate {
        id,
        connected_at,
        min_ping,
        last_block,
        last_tx,
        netgroup: 1,
        noban: false,
    }
}

#[test]
fn eviction_drops_the_newest_in_the_largest_netgroup() {
    // Low ids are one-peer groups with a better ping, so the block, tx,
    // ping, and netgroup protects consume them. The interesting peers are
    // a size-3 group and one newer peer alone in another group.
    let mut cands = Vec::new();
    for i in 1..=40 {
        cands.push(InboundEvictCandidate {
            id: i,
            connected_at: 1,
            min_ping: Some(0.01),
            last_block: 0,
            last_tx: 0,
            netgroup: 1_000 + i,
            noban: false,
        });
    }
    cands.push(InboundEvictCandidate {
        id: 100,
        connected_at: 10,
        min_ping: Some(1.0),
        last_block: 0,
        last_tx: 0,
        netgroup: 50_000,
        noban: false,
    });
    cands.push(InboundEvictCandidate {
        id: 101,
        connected_at: 50,
        min_ping: Some(1.0),
        last_block: 0,
        last_tx: 0,
        netgroup: 50_000,
        noban: false,
    });
    cands.push(InboundEvictCandidate {
        id: 102,
        connected_at: 200,
        min_ping: Some(1.0),
        last_block: 0,
        last_tx: 0,
        netgroup: 50_000,
        noban: false,
    });
    cands.push(InboundEvictCandidate {
        id: 103,
        connected_at: 500,
        min_ping: Some(1.0),
        last_block: 0,
        last_tx: 0,
        netgroup: 9_000,
        noban: false,
    });
    let victim = select_inbound_eviction(cands).expect("one inbound to evict");
    assert_ne!(victim, 101, "the oldest peer in the largest group stays");
    assert_ne!(victim, 103, "a newer peer in a smaller group stays");
    assert_eq!(victim, 102, "evict the newest peer in the largest netgroup");
}

#[test]
fn eviction_protects_block_tx_ping_and_netgroup() {
    // 4 block + 5 slow + 4 tx + 8 fast = 21; after protects, one slow remains.
    let mut cands = Vec::new();
    for i in 0..4 {
        cands.push(cand(i, 100 + i, Some(0.05), 1000 + i, 0));
    }
    for i in 4..9 {
        cands.push(cand(i, 200 + i, Some(0.5), 0, 0));
    }
    for i in 9..13 {
        cands.push(cand(i, 300 + i, Some(0.05), 0, 1000 + i));
    }
    for i in 13..21 {
        cands.push(cand(i, 400 + i, Some(0.01), 0, 0));
    }
    assert!(
        select_inbound_eviction(cands).is_none(),
        "block, tx, ping, netgroup, and longest-connected protects cover this set"
    );
}

#[test]
fn noban_never_evicted_alone() {
    let cands = vec![InboundEvictCandidate {
        id: 7,
        connected_at: 1,
        min_ping: Some(9.0),
        last_block: 0,
        last_tx: 0,
        netgroup: 1,
        noban: true,
    }];
    assert!(select_inbound_eviction(cands).is_none());
}

#[test]
fn maxconnections_shaped_protect_count() {
    // Fewer than protect budget → nothing to evict.
    let cands: Vec<_> = (0..8).map(|i| cand(i, i, Some(0.1), i, i)).collect();
    assert!(select_inbound_eviction(cands).is_none());
}

#[test]
fn protect_by_netgroup_keeps_one_peer_from_each_largest_group() {
    let mut untouched = vec![cand(1, 1, None, 0, 0)];
    protect_by_netgroup(&mut untouched, 0);
    assert_eq!(untouched.len(), 1, "k = 0 protects nobody");
    let mut empty = Vec::new();
    protect_by_netgroup(&mut empty, 4);
    assert!(empty.is_empty());

    let group = |id, netgroup| InboundEvictCandidate {
        netgroup,
        ..cand(id, id, None, 0, 0)
    };
    // Group 10 has two peers. Group 1 has one. k = 1 protects the lowest
    // id in the larger group and leaves the other two.
    let mut cands = vec![group(1, 10), group(2, 10), group(3, 1)];
    protect_by_netgroup(&mut cands, 1);
    let ids: Vec<_> = cands.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![2, 3]);

    let mut capped = vec![group(1, 10), group(3, 1)];
    protect_by_netgroup(&mut capped, 1);
    assert_eq!(capped.iter().map(|c| c.id).collect::<Vec<_>>(), vec![1]);

    let mut mismatched = vec![group(8, 4), group(9, 5)];
    protect_by_netgroup(&mut mismatched, 1);
    assert_eq!(mismatched.iter().map(|c| c.id).collect::<Vec<_>>(), vec![9]);
}

#[test]
fn eviction_netgroup_ipv4_slash16() {
    let a: std::net::SocketAddr = "1.2.3.4:1".parse().unwrap();
    let b: std::net::SocketAddr = "1.2.9.9:1".parse().unwrap();
    let c: std::net::SocketAddr = "1.3.0.1:1".parse().unwrap();
    assert_eq!(eviction_netgroup(a), eviction_netgroup(b));
    assert_ne!(eviction_netgroup(a), eviction_netgroup(c));
}
