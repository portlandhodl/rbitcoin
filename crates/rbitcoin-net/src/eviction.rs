//! Inbound peer eviction (Core `SelectNodeToEvict` / `AttemptToEvictConnection`).
//!
//! When inbound slots are full, accept a new peer only after disconnecting one
//! unprotected inbound. Protection mirrors Core: netgroup, recent blocks, recent
//! txs, lowest min-ping.

/// One inbound session considered for eviction.
#[derive(Clone, Debug)]
pub struct InboundEvictCandidate {
    pub id: u64,
    pub connected_at: u64,
    pub min_ping: Option<f64>,
    pub last_block: u64,
    pub last_tx: u64,
    pub netgroup: u64,
    pub noban: bool,
}

#[doc(hidden)]
pub const PROTECT_NETGROUP: usize = 4;
#[doc(hidden)]
pub const PROTECT_BLOCKS: usize = 4;
#[doc(hidden)]
pub const PROTECT_TXS: usize = 4;
#[doc(hidden)]
pub const PROTECT_MINPING: usize = 8;
/// Longest-connected inbound peers kept when slots are full.
#[doc(hidden)]
pub const PROTECT_LONGEST: usize = 8;

/// Pick one inbound id to disconnect, or `None` if every candidate is protected.
pub fn select_inbound_eviction(mut cands: Vec<InboundEvictCandidate>) -> Option<u64> {
    cands.retain(|c| !c.noban);
    if cands.is_empty() {
        return None;
    }

    protect_by_netgroup(&mut cands, PROTECT_NETGROUP);
    if cands.is_empty() {
        return None;
    }

    cands.sort_by(|a, b| {
        b.last_block
            .cmp(&a.last_block)
            .then_with(|| a.id.cmp(&b.id))
    });
    remove_first_k(&mut cands, PROTECT_BLOCKS);
    if cands.is_empty() {
        return None;
    }

    cands.sort_by(|a, b| b.last_tx.cmp(&a.last_tx).then_with(|| a.id.cmp(&b.id)));
    remove_first_k(&mut cands, PROTECT_TXS);
    if cands.is_empty() {
        return None;
    }

    cands.sort_by(|a, b| {
        ping_key(a.min_ping)
            .partial_cmp(&ping_key(b.min_ping))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    remove_first_k(&mut cands, PROTECT_MINPING);
    if cands.is_empty() {
        return None;
    }

    // A share of the longest-connected peers stays. Evicting them is how a
    // new inbound replaces the peers that have been useful the longest.
    cands.sort_by(|a, b| {
        a.connected_at
            .cmp(&b.connected_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    remove_first_k(&mut cands, PROTECT_LONGEST);
    if cands.is_empty() {
        return None;
    }

    // Newest peer in the largest netgroup. Group ids are the integers stored
    // at accept; this compares those integers and does not read asmap.
    let mut counts = std::collections::HashMap::<u64, usize>::new();
    for c in &cands {
        *counts.entry(c.netgroup).or_insert(0) += 1;
    }
    let (group, _) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .expect("at least one candidate");
    cands
        .iter()
        .filter(|c| c.netgroup == group)
        .max_by(|a, b| {
            a.connected_at
                .cmp(&b.connected_at)
                .then_with(|| a.id.cmp(&b.id))
        })
        .map(|c| c.id)
}

#[doc(hidden)]
pub fn ping_key(min_ping: Option<f64>) -> f64 {
    min_ping.unwrap_or(f64::MAX)
}

#[doc(hidden)]
pub fn remove_first_k(cands: &mut Vec<InboundEvictCandidate>, k: usize) {
    let n = k.min(cands.len());
    cands.drain(0..n);
}

/// Protect up to `k` peers from the largest keyed netgroups (Core netgroup protect).
#[doc(hidden)]
pub fn protect_by_netgroup(cands: &mut Vec<InboundEvictCandidate>, k: usize) {
    use std::collections::HashMap;
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for c in cands.iter() {
        *counts.entry(c.netgroup).or_insert(0) += 1;
    }
    let mut groups: Vec<(u64, usize)> = counts.into_iter().collect();
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut protect_ids = Vec::new();
    for (group, _) in groups {
        if protect_ids.len() >= k {
            break;
        }
        if let Some(c) = cands
            .iter()
            .filter(|c| c.netgroup == group)
            .min_by_key(|c| c.id)
        {
            protect_ids.push(c.id);
        }
    }
    cands.retain(|c| !protect_ids.contains(&c.id));
}

/// Stable netgroup key for eviction (IPv4 `/16`, IPv6 `/32`; no asmap).
pub fn eviction_netgroup(addr: std::net::SocketAddr) -> u64 {
    crate::netgroup::netgroup(addr, None)
}
