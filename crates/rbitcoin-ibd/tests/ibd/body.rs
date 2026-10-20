use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_ibd::ibd::body::BodyPresence;
use rbitcoin_net::chain::ChainHub;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

fn h(n: u8) -> BlockHash {
    let mut b = [0u8; 32];
    b[0] = n;
    BlockHash::from_byte_array(b)
}

#[test]
fn presence_lifecycle_surface() {
    let mut body = BodyPresence::new();
    body.mark_pending(h(1));
    assert!(body.is_pending(&h(1)));
    assert!(!body.is_known_archived(&h(1)));

    body.mark_archived(h(2));
    assert!(body.is_known_archived(&h(2)));
    assert!(!body.is_pending(&h(2)));

    body.mark_missing(h(3));
    assert!(!body.is_pending(&h(3)));
    assert!(!body.is_known_archived(&h(3)));

    assert_eq!(body.size_snapshot().pending, 1);
    assert_eq!(body.known_len(), 1);

    let hash = h(4);
    body.mark_pending(hash);
    body.mark_archived(hash);
    assert!(body.is_known_archived(&hash));
    body.mark_missing(hash);
    assert!(!body.is_known_archived(&hash));
    body.mark_archived(hash);
    assert!(body.is_known_archived(&hash));

    body.demote_known(h(2));
    assert!(!body.is_known_archived(&h(2)));

    body.mark_archived(h(10));
    body.mark_missing(h(11));
    body.mark_rejected(h(12));
    body.hygiene_retain(|x| *x == h(10) || *x == h(12));
    assert!(body.is_known_archived(&h(10)));
    assert!(body.is_rejected(&h(12)));
    let snap = body.size_snapshot();
    assert_eq!(snap.known, 1);
    assert_eq!(snap.rejected, 1);
}

#[test]
fn rejected_sticky_through_mark_ops() {
    let mut body = BodyPresence::new();
    let rej = h(5);
    body.mark_archived(rej);
    body.mark_rejected(rej);
    assert!(body.is_rejected(&rej));
    body.mark_archived(rej);
    assert!(body.is_rejected(&rej));
    assert!(!body.is_known_archived(&rej));

    let r = h(20);
    body.mark_rejected(r);
    body.mark_pending(r);
    body.mark_missing(r);
    body.mark_archived(r);
    assert!(body.is_rejected(&r));
    assert!(!body.is_pending(&r));
    assert!(!body.is_known_archived(&r));
}

#[test]
fn expire_stale_pending_if_surface() {
    let mut body = BodyPresence::new();
    body.mark_pending(h(1));
    body.mark_pending(h(2));
    let expired = body.expire_stale_pending_if(Duration::ZERO, |_| true);
    assert_eq!(expired.len(), 2);

    body.mark_pending(h(3));
    body.mark_pending(h(4));
    let only3 = body.expire_stale_pending_if(Duration::ZERO, |x| *x == h(3));
    assert_eq!(only3, vec![h(3)]);
    assert!(body.is_pending(&h(4)));

    body.mark_archived(h(10));
    body.mark_rejected(h(11));
    let snap = body.size_snapshot();
    assert_eq!(snap.known, 1);
    assert_eq!(snap.pending, 1);
    assert_eq!(snap.rejected, 1);
}
