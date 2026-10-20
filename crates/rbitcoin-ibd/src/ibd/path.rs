//! Ordered work-path seeding and header locator tips.

use super::state::IbdWorkState;
use super::MAX_ORDERED_HEADERS;
use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_log::{info, warn};
use rbitcoin_net::chain::ChainHub;
use std::time::Instant;

pub fn load_resume_entries(
    hub: &ChainHub,
    exclude: &[[u8; 32]],
) -> Vec<rbitcoin_query::ResumeWorkEntry> {
    let Some(tip_hash) = hub.tip_hash() else {
        return Vec::new();
    };
    let tip_h = hub.tip_height().unwrap_or(0);
    info!("ibd: resume seed walk start tip={tip_h} max_ordered={MAX_ORDERED_HEADERS}");
    match hub.query.resume_work_path_after_tip_excluding(
        tip_hash.to_byte_array(),
        tip_h,
        MAX_ORDERED_HEADERS,
        exclude,
    ) {
        Ok(p) => p,
        Err(e) => {
            warn!("ibd: resume seed from store failed: {e}");
            Vec::new()
        }
    }
}

pub fn seed_work_path_from_store(st: &mut IbdWorkState, hub: &ChainHub) {
    let exclude: Vec<[u8; 32]> = st.reorg.invalid.iter().collect();
    let path = load_resume_entries(hub, &exclude);
    install_resumed_path(st, hub, &path);
}

pub fn install_resumed_path(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    path: &[rbitcoin_query::ResumeWorkEntry],
) {
    install_resume(st, hub, path, true);
}

/// Plant the resume path and leave the most-work rewind to the caller, which
/// runs it after dropping the coordination lock.
pub fn stage_resumed_path(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    path: &[rbitcoin_query::ResumeWorkEntry],
) {
    install_resume(st, hub, path, false);
}

#[doc(hidden)]
pub fn install_resume(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    path: &[rbitcoin_query::ResumeWorkEntry],
    rewind_now: bool,
) {
    let Some(tip_hash) = hub.tip_hash() else {
        return;
    };
    let tip_h = hub.tip_height().unwrap_or(0);
    let t0 = Instant::now();
    if path.is_empty() {
        return;
    }
    info!(
        "ibd: resume seed graph n={} (store walk {:?})",
        path.len(),
        t0.elapsed()
    );
    let mut explore_is_sibling_fork = false;
    let mut explore_need = Vec::new();
    for e in path {
        let hash = BlockHash::from_byte_array(e.hash);
        if st.reorg.invalid.contains(e.hash) {
            break;
        }
        st.known_headers.insert(hash);
        st.hash_height.insert(hash, e.height);
        st.header_fks.insert(hash, e.header_fk);
        if e.height <= tip_h && hash != tip_hash {
            explore_is_sibling_fork = true;
            if !e.has_body {
                explore_need.push(hash);
            }
        } else if explore_is_sibling_fork && !e.has_body && explore_need.len() < 4 {
            explore_need.push(hash);
        }
    }
    if explore_is_sibling_fork {
        let explore_tip = path
            .iter()
            .rev()
            .find(|e| !st.reorg.invalid.contains(e.hash))
            .map(|e| BlockHash::from_byte_array(e.hash));
        st.reorg.register_explore(explore_need, explore_tip);
        if rewind_now {
            match super::reorg::maybe_rewind_to_best_work(st, hub) {
                Ok(true) => {
                    info!(
                        "ibd: resume seed rewound to most-work header path (store walk {:?})",
                        t0.elapsed()
                    );
                    return;
                }
                Ok(false) => {
                    info!(
                    "ibd: resume seed greater-work sibling fork explore_need={} explore_tip={:?}",
                    st.reorg.need_getdata().len(),
                    explore_tip
                );
                }
                Err(e) => {
                    warn!("ibd: resume seed most-work rewind failed: {e}");
                }
            }
        }
    }
    let mut with_body = 0u32;
    let mut ready_prefix_to = tip_h;
    let mut ready_prefix = true;
    let tip = hub.tip_height().zip(hub.tip_hash());
    for e in path {
        if st.reorg.invalid.contains(e.hash) {
            break;
        }
        let hash = BlockHash::from_byte_array(e.hash);
        let prev = BlockHash::from_byte_array(e.prev_hash);
        let on_path = st.try_set_path_slot(hash, e.height, prev, tip);
        st.max_ordered_height = st.max_ordered_height.max(e.height);
        if on_path && st.ordered_set.insert(hash) {
            st.ordered.push_back(hash);
        }
        if e.has_body {
            st.body.mark_archived(hash);
            with_body = with_body.saturating_add(1);
            if ready_prefix {
                ready_prefix_to = e.height;
            }
        } else {
            ready_prefix = false;
        }
    }
    st.max_ready_height = st.max_ready_height.max(ready_prefix_to);
    st.headers_done = false;
    info!(
        "ibd: resume seed ordered={} class_a_bodies={} ready_to={} (store walk {:?})",
        st.ordered.len(),
        with_body,
        ready_prefix_to,
        t0.elapsed()
    );
    plant_valid_tip_child(st, hub);
    // The anchor was restored before these rows were queued. Note the linked
    // prefix so script checks turn back off.
    super::header_walk::renote_stored_path(st, hub);
}

/// If tip+1 is missing or invalid, plant a known valid sibling/child of tip.
pub fn plant_valid_tip_child(st: &mut IbdWorkState, hub: &ChainHub) {
    let Some((child, expect, tip_hash)) = plant_find_valid_child(st, hub) else {
        return;
    };
    plant_extend_from_child(st, hub, child, expect, tip_hash);
}

#[doc(hidden)]
pub fn plant_find_valid_child(
    st: &IbdWorkState,
    hub: &ChainHub,
) -> Option<(BlockHash, u32, BlockHash)> {
    let tip_hash = hub.tip_hash()?;
    let tip_h = hub.tip_height().unwrap_or(0);
    let expect = if hub.tip_height().is_none() {
        0u32
    } else {
        tip_h.saturating_add(1)
    };
    if let Some(&cur) = st.height_to_hash.get(&expect) {
        if !st.reorg.invalid.contains(cur.to_byte_array()) && !st.body.is_rejected(&cur) {
            return None;
        }
    }
    for (&h, &ht) in &st.hash_height {
        if ht != expect {
            continue;
        }
        if st.reorg.invalid.contains(h.to_byte_array()) || st.body.is_rejected(&h) {
            continue;
        }
        let Some(prev) = st.path_prev.get(&h).copied().or_else(|| hub.prev_of(&h)) else {
            continue;
        };
        if prev != tip_hash {
            continue;
        }
        return Some((h, expect, tip_hash));
    }
    None
}

#[doc(hidden)]
pub fn plant_extend_from_child(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    child: BlockHash,
    expect: u32,
    tip_hash: BlockHash,
) {
    let mut prev = tip_hash;
    let mut cur = child;
    let mut ht = expect;
    for _ in 0..MAX_ORDERED_HEADERS {
        if !plant_commit_header(st, hub, cur, ht, prev) {
            break;
        }
        prev = cur;
        ht = ht.saturating_add(1);
        match plant_next_connected(st, hub, cur, ht) {
            Some(n) => cur = n,
            None => break,
        }
    }
}

#[doc(hidden)]
pub fn plant_commit_header(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    cur: BlockHash,
    ht: u32,
    prev: BlockHash,
) -> bool {
    if st.reorg.invalid.contains(cur.to_byte_array()) || st.body.is_rejected(&cur) {
        return false;
    }
    let tip = hub.tip_height().zip(hub.tip_hash());
    let on_path = st.try_set_path_slot(cur, ht, prev, tip);
    st.max_ordered_height = st.max_ordered_height.max(ht);
    if on_path && st.ordered_set.insert(cur) {
        st.ordered.push_back(cur);
    }
    st.known_headers.insert(cur);
    true
}

#[doc(hidden)]
pub fn plant_hash_at_height(st: &IbdWorkState, ht: u32) -> Option<BlockHash> {
    st.height_to_hash.get(&ht).copied().or_else(|| {
        st.hash_height
            .iter()
            .find(|(_, &hht)| hht == ht)
            .map(|(h, _)| *h)
    })
}

#[doc(hidden)]
pub fn plant_next_connected(
    st: &IbdWorkState,
    hub: &ChainHub,
    parent: BlockHash,
    ht: u32,
) -> Option<BlockHash> {
    let n = plant_hash_at_height(st, ht)?;
    let p = st.path_prev.get(&n).copied().or_else(|| hub.prev_of(&n))?;
    (p == parent).then_some(n)
}

/// Highest hashes on the download path (newest first) for a queue refill.
///
/// The walk tip is a separate locator. An empty deque refills from the
/// confirmed tip, which the header locator appends when this list is empty.
/// Includes proactive exploration tips (greater-work sibling path) so empty
/// getheaders lag can re-root onto the heavier store chain.
pub fn work_path_tips(st: &IbdWorkState) -> Vec<BlockHash> {
    let mut tips = Vec::with_capacity(8);
    // ordered is tip→far; the back is the highest known header on the path.
    let live =
        |h: &BlockHash| !st.reorg.invalid.contains(h.to_byte_array()) && !st.body.is_rejected(h);
    for h in st.ordered.iter().rev().take(4) {
        if st.ordered_set.contains(h) && live(h) {
            tips.push(*h);
        }
    }
    for h in st.reorg.explore_tips() {
        if !live(h) {
            continue;
        }
        if !tips.contains(h) {
            tips.push(*h);
        }
        if tips.len() >= 8 {
            break;
        }
    }
    // Ghosts in the deque are not a path. A hash known only by height still
    // names the next window. An empty deque does not: those entries are not
    // the connected work path.
    if tips.is_empty() && !st.ordered.is_empty() {
        if let Some((&h, _)) = st
            .hash_height
            .iter()
            .filter(|(h, _)| live(h))
            .max_by_key(|(_, &ht)| ht)
        {
            tips.push(h);
        }
    }
    tips
}

/// Height of the next header after the queue tail, or one above the confirmed
/// tip when the queue is empty.
pub fn next_path_height(st: &IbdWorkState, hub: &ChainHub) -> Option<u32> {
    let tail = st
        .ordered
        .iter()
        .rev()
        .find(|hash| st.ordered_set.contains(*hash));
    if let Some(tail) = tail {
        return st
            .hash_height
            .get(tail)
            .map(|height| height.saturating_add(1));
    }
    Some(hub.tip_height().unwrap_or(0).saturating_add(1))
}

/// The header after the candidate is indexed and can still join the queue.
///
/// An ineligible occupant (confirmed, rejected, or invalid) does not count:
/// treating it as stored would suppress every later ask.
pub fn walk_successor_indexed(st: &IbdWorkState, hub: &ChainHub) -> bool {
    let height = if let Some(height) = st.header_walk.next_height() {
        height
    } else if let Some(height) = next_path_height(st, hub) {
        height
    } else {
        return false;
    };
    let Some(&hash) = st.height_to_hash.get(&height) else {
        return false;
    };
    !st.reorg.invalid.contains(hash.to_byte_array())
        && !st.body.is_rejected(&hash)
        && !hub.has_block(&hash)
}

/// Append stored work-path headers while the queue is under the soft cap.
/// Stops at the first height that is not indexed. Returns how many were added.
pub fn append_stored_headers(st: &mut IbdWorkState, hub: &ChainHub) -> usize {
    let mut added = 0usize;
    while st.ordered.len() < super::ORDERED_HEADERS_SOFT_CAP {
        let Some(height) = next_path_height(st, hub) else {
            break;
        };
        let Some(&hash) = st.height_to_hash.get(&height) else {
            break;
        };
        if st.reorg.invalid.contains(hash.to_byte_array())
            || st.body.is_rejected(&hash)
            || hub.has_block(&hash)
        {
            break;
        }
        let n = push_reseed(st, &[hash]);
        if n == 0 {
            break;
        }
        added += n;
    }
    added
}

pub fn push_reseed(st: &mut IbdWorkState, hashes: &[BlockHash]) -> usize {
    let mut rebuilt = 0usize;
    for &h in hashes {
        if st.ordered.len() >= super::MAX_ORDERED_HEADERS {
            break;
        }
        if st.ordered_set.insert(h) {
            st.ordered.push_back(h);
            rebuilt += 1;
        }
    }
    rebuilt
}
