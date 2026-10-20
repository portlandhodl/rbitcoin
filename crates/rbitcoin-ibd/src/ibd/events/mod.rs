//! Peer / body event drain and apply (IBD main loop).

use super::assign::clear_hash_inflight;
use super::assign_plan::{
    remove_from_ordered, should_enqueue_header, want_headers_beyond_soft_cap,
};
use super::dial::{
    disconnect_peer, note_dead_without_block_bytes, note_misbehavior_dead, release_peer_block_work,
};
use super::exit::{
    header_lag_behind_peers, should_advance_locator_after_known_batch,
    should_log_empty_headers_lag, should_rerequest_headers_on_empty_lag,
    should_reseed_work_path_on_empty_lag,
};
use super::peer_io::{note_block_progress, note_block_rx, PeerCmd, PeerEvent};
use super::state::IbdWorkState;
use super::status::LoopStats;
use super::{CONTIG_DENSIFY_AHEAD, MAX_ORDERED_HEADERS, MAX_PEER_POOL, ORDERED_HEADERS_SOFT_CAP};
use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_log::{info, trace, warn};
use rbitcoin_net::chain::ChainHub;
use rbitcoin_net::codec::MAX_HEADERS_RESULTS;
use rbitcoin_net::error::NetError;
use rbitcoin_net::seeds::AddrMan;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Persist the headers classification kept. A short acceptance is the prefix
/// binary search, so the locked commit does not call the store again.
pub fn prepare_header_store(
    hub: &ChainHub,
    headers: &[bitcoin::block::Header],
) -> (Vec<rbitcoin_primitives::Fk>, Option<bitcoin::Work>) {
    let fks = hub.ensure_headers_batch(headers).unwrap_or_default();
    let work = hub.chain_work().ok();
    if fks.len() == headers.len() {
        return (fks, work);
    }
    let accepted = ensure_accepted_prefix(hub, headers);
    (accepted.into_iter().map(|(_, fk)| fk).collect(), work)
}

/// Immediately stop getdata and disconnect every peer (SIGINT / IBD exit).
pub fn disconnect_all_peers(st: &mut IbdWorkState) {
    let n = st.slots.len();
    if n == 0 {
        return;
    }
    for s in &st.slots {
        let _ = s.cmd_tx.send(PeerCmd::Shutdown);
        s.task.abort();
    }
    st.inflight.clear();
    for s in &mut st.slots {
        s.track_clear();
        s.alive = false;
    }
    st.slots.clear();
    info!("ibd: disconnected {n} peer(s)");
}

/// First 80 bytes of a consensus-serialized block → header (no full block decode).
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn decode_block_header_prefix(payload: &[u8]) -> Option<bitcoin::block::Header> {
    use bitcoin::consensus::Decodable;
    if payload.len() < 80 {
        return None;
    }
    let mut cur = std::io::Cursor::new(&payload[..80]);
    bitcoin::block::Header::consensus_decode(&mut cur).ok()
}

/// Header/control events per turn (anti-livelock under multi-peer header spam).
#[doc(hidden)]
pub const CTRL_DRAIN_EVENT_BUDGET: u64 = 512;
#[doc(hidden)]
pub const CTRL_DRAIN_TIME_BUDGET: Duration = Duration::from_millis(5);
/// Body path (framed/decoded blocks): process as much as possible so delivered
/// bytes are not stranded behind headers. Soft wall so cancel/assign still run.
#[doc(hidden)]
pub const BODY_DRAIN_TIME_BUDGET: Duration = Duration::from_millis(40);

#[allow(clippy::too_many_arguments)] // call-site args stay unbundled
/// Non-blocking drain of archive results + peer events.
///
/// **Priority:** body (`BlockFramed`/…) → headers.
/// Delivered block bytes must not wait on header floods (single-FIFO waste).
/// Headers remain budgeted so apply cannot livelock.
///
/// Archive-job dual-track is gone: sole Class A path is body queue → confirm.
pub fn drain_ready_peer_and_body_events(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    body_rx: &mut mpsc::UnboundedReceiver<PeerEvent>,
    ctrl_rx: &mut mpsc::UnboundedReceiver<PeerEvent>,
    archive_write_next: &AtomicU32,
    loop_stats: &LoopStats,
    peer_book: &mut AddrMan,
    local_addr: SocketAddr,
    confirm_feed: Option<&super::confirm::ConfirmFeed>,
) -> Result<bool, NetError> {
    let t0 = Instant::now();
    let mut events = 0u64;

    let body_t0 = Instant::now();
    loop {
        if body_t0.elapsed() >= BODY_DRAIN_TIME_BUDGET {
            break;
        }
        match body_rx.try_recv() {
            Ok(ev) => {
                events += 1;
                apply_peer_event(
                    st,
                    hub,
                    ev,
                    archive_write_next,
                    peer_book,
                    local_addr,
                    confirm_feed,
                );
            }
            Err(_) => break,
        }
    }

    let ctrl_t0 = Instant::now();
    let mut ctrl_n = 0u64;
    while ctrl_n < CTRL_DRAIN_EVENT_BUDGET && ctrl_t0.elapsed() < CTRL_DRAIN_TIME_BUDGET {
        if st.header_cmd.as_ref().is_some_and(|tx| tx.capacity() == 0) {
            break;
        }
        match ctrl_rx.try_recv() {
            Ok(ev) => {
                events += 1;
                ctrl_n += 1;
                apply_peer_event(
                    st,
                    hub,
                    ev,
                    archive_write_next,
                    peer_book,
                    local_addr,
                    confirm_feed,
                );
            }
            Err(_) => break,
        }
    }

    loop_stats
        .drain_ns
        .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    loop_stats.drain_events.fetch_add(events, Ordering::Relaxed);
    Ok(true)
}

#[doc(hidden)]
pub fn batch_header_height(
    st: &IbdWorkState,
    hub: &ChainHub,
    prev: BlockHash,
    batch_prev: Option<(BlockHash, u32)>,
) -> Option<u32> {
    parent_height(&st.hash_height, hub, prev)
        .or_else(|| batch_prev.and_then(|(ph, pht)| (ph == prev).then_some(pht.saturating_add(1))))
}

#[doc(hidden)]
pub fn note_header_path(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    hash: BlockHash,
    height: u32,
    prev: BlockHash,
) {
    let tip = hub.tip_height().zip(hub.tip_hash());
    if st.try_set_path_slot(hash, height, prev, tip) {
        st.max_peer_height = st.max_peer_height.max(height);
        st.max_ordered_height = st.max_ordered_height.max(height);
        return;
    }
    if let Some(&cur) = st.height_to_hash.get(&height) {
        if cur != hash {
            st.reorg.register_explore(std::iter::once(hash), Some(hash));
        }
    }
}

#[doc(hidden)]
pub fn try_enqueue_ordered_header(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    hash: BlockHash,
    prev: BlockHash,
) -> bool {
    if hub.has_block(&hash) {
        st.known_headers.insert(hash);
        return false;
    }
    if st.body.is_rejected(&hash) || st.reorg.invalid.contains(hash.to_byte_array()) {
        return false;
    }
    let prev_ok = st.known_headers.contains(&prev)
        || hub.has_block(&prev)
        || prev.to_byte_array() == [0u8; 32]
        || hub.tip_hash() == Some(prev);
    if !prev_ok && hub.tip_height().is_some() && !st.known_headers.is_empty() {
        return false;
    }
    st.known_headers.insert(hash);
    let Some(ht) = st.hash_height.get(&hash).copied() else {
        return false;
    };
    if !st.is_on_path(&hash, ht) || st.ordered.len() >= MAX_ORDERED_HEADERS {
        return false;
    }
    if super::path::next_path_height(st, hub).is_some_and(|next| ht > next) {
        return false;
    }
    // The queue is a window over headers already written. Stop at the soft cap.
    if st.header_walk.has_checkpoints() && st.ordered.len() >= ORDERED_HEADERS_SOFT_CAP {
        return false;
    }
    if !should_enqueue_header(
        st.ordered_set.contains(&hash),
        st.inflight.contains_key(&hash),
        st.body.is_pending(&hash),
        st.body.is_rejected(&hash),
        hub.has_block(&hash),
        Some(ht),
        hub.tip_height(),
    ) {
        return false;
    }
    if st.ordered_set.insert(hash) {
        st.ordered.push_back(hash);
        return true;
    }
    false
}

/// After the full batch fails, probe one header before searching.
///
/// A rejected first header means no longer prefix can be stored.
#[doc(hidden)]
pub fn accepted_prefix_after_failure<T>(
    len: usize,
    mut probe: impl FnMut(usize) -> Result<Vec<T>, NetError>,
) -> (usize, Vec<T>) {
    let Ok(mut lo_fks) = probe(1) else {
        return (0, Vec::new());
    };
    let mut lo = 1usize;
    let mut hi = len;
    for _ in 0..len {
        let width = hi - lo;
        if width <= 1 {
            break;
        }
        let mid = lo + width / 2;
        match probe(mid) {
            Ok(fks) => {
                lo = mid;
                lo_fks = fks;
            }
            Err(_) => hi = mid,
        }
    }
    (lo, lo_fks)
}

/// Longest prefix [`ChainHub::ensure_headers_batch`] accepts.
///
/// A rejected tail is not stored and must not update path or explore state.
/// The success path is one batch. A failing batch probes the first header
/// and stops when that header is rejected. Otherwise the rest of the prefix
/// is binary-searched.
#[doc(hidden)]
pub fn ensure_accepted_prefix(
    hub: &ChainHub,
    headers: &[bitcoin::block::Header],
) -> Vec<(bitcoin::block::Header, rbitcoin_primitives::Fk)> {
    if headers.is_empty() {
        return Vec::new();
    }
    if let Ok(fks) = hub.ensure_headers_batch(headers) {
        return headers.iter().copied().zip(fks).collect();
    }
    if headers.len() == 1 {
        return Vec::new();
    }
    let (lo, lo_fks) = accepted_prefix_after_failure(headers.len(), |mid| {
        hub.ensure_headers_batch(&headers[..mid])
    });
    headers[..lo].iter().copied().zip(lo_fks).collect()
}

/// `fks` comes from [`prepare_header_store`] after classification.
#[doc(hidden)]
pub fn accepted_headers(
    headers: &[bitcoin::block::Header],
    fks: &[rbitcoin_primitives::Fk],
) -> Vec<(bitcoin::block::Header, rbitcoin_primitives::Fk)> {
    let n = fks.len().min(headers.len());
    headers
        .iter()
        .copied()
        .zip(fks.iter().copied())
        .take(n)
        .collect()
}

#[doc(hidden)]
pub fn on_headers_batch(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    peer: usize,
    headers: Vec<bitcoin::block::Header>,
    fks: &[rbitcoin_primitives::Fk],
    work: Option<bitcoin::Work>,
) -> usize {
    let accepted = accepted_headers(&headers, fks);
    let mut added = 0usize;
    let mut batch_prev: Option<(BlockHash, u32)> = None;
    let mut stored = Vec::with_capacity(accepted.len());
    for (hdr, fk) in accepted {
        let hash = hdr.block_hash();
        let prev = hdr.prev_blockhash;
        st.header_fks.insert(hash, fk);
        if let Some(h) = batch_header_height(st, hub, prev, batch_prev) {
            note_header_path(st, hub, hash, h, prev);
            if !st.is_on_path(&hash, h) && super::header_walk::note_off_path(st, hub, peer, hash) {
                disconnect_peer(st, peer);
            }
            if st.is_on_path(&hash, h) {
                let base = if hub.tip_hash() == Some(prev) {
                    work
                } else {
                    None
                };
                hub.query.note_milestone_header(
                    h,
                    hash.to_byte_array(),
                    prev.to_byte_array(),
                    hdr.work(),
                    base,
                );
            }
            batch_prev = Some((hash, h));
        }
        if try_enqueue_ordered_header(st, hub, hash, prev) {
            added += 1;
        }
        stored.push(hdr);
    }
    super::header_walk::note_stored_candidate(st, hub, &stored);
    added
}

#[doc(hidden)]
pub fn on_empty_headers(st: &mut IbdWorkState, hub: &ChainHub) -> bool {
    st.empty_header_streak = st.empty_header_streak.saturating_add(1);
    let tip_h = hub.tip_height().unwrap_or(0);
    let lag = header_lag_behind_peers(st, tip_h);
    let path_idle = st.ordered.is_empty() && st.inflight.is_empty();
    let peers_n = st.slots.iter().filter(|s| s.alive).count() as u32;
    if st.empty_header_streak >= peers_n.max(2) && path_idle {
        st.headers_done = true;
    } else if lag > 2 {
        if should_log_empty_headers_lag(st.empty_header_streak) {
            let known = st
                .max_ready_height
                .max(st.hash_height.values().copied().max().unwrap_or(0));
            if st.ordered_set.is_empty() {
                warn!(
                    "ibd: empty headers but lag={lag} behind max_peer_height={} (known≈{known}, tip={tip_h}) — keep header sync",
                    st.max_peer_height,
                );
            } else {
                trace!(
                    "ibd: empty headers but lag={lag} behind max_peer_height={} (known≈{known}, tip={tip_h}) — keep header sync",
                    st.max_peer_height,
                );
            }
        }
        st.headers_done = false;
        if should_rerequest_headers_on_empty_lag(st.empty_header_streak) {
            if should_reseed_work_path_on_empty_lag(
                st.empty_header_streak,
                st.ordered_set.is_empty(),
            ) {
                return true;
            }
            let _ = super::header_walk::send_getheaders(st, hub);
        }
    } else if st.empty_header_streak < 8 && st.ordered_set.len() < ORDERED_HEADERS_SOFT_CAP {
        let _ = super::header_walk::send_getheaders(st, hub);
    } else if st.empty_header_streak >= 8 && lag <= 2 {
        st.headers_done = true;
    }
    false
}

#[doc(hidden)]
pub fn on_known_headers_batch(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    _peer: usize,
    batch_len: usize,
) {
    let live = st.ordered_set.len();
    let need_ready_headroom = want_headers_beyond_soft_cap(
        live,
        st.body.known_len(),
        st.max_ordered_height.saturating_sub(st.max_ready_height),
        4096,
    );
    let lag = header_lag_behind_peers(st, hub.tip_height().unwrap_or(0));
    if live < MAX_ORDERED_HEADERS
        && (live < ORDERED_HEADERS_SOFT_CAP || need_ready_headroom)
        && should_advance_locator_after_known_batch(
            live,
            lag,
            batch_len >= MAX_HEADERS_RESULTS,
            need_ready_headroom,
        )
    {
        let _ = super::header_walk::send_getheaders(st, hub);
    }
}

pub fn apply_peer_event(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    ev: PeerEvent,
    archive_write_next: &AtomicU32,
    peer_book: &mut AddrMan,
    local_addr: SocketAddr,
    confirm_feed: Option<&super::confirm::ConfirmFeed>,
) {
    match ev {
        PeerEvent::Headers { peer, headers } => apply_headers_event(st, hub, peer, headers),
        PeerEvent::BlockFramed {
            peer,
            hash,
            payload,
            header_fk,
        } => apply_block_framed(
            st,
            hub,
            archive_write_next,
            confirm_feed,
            FramedBody {
                peer,
                hash,
                payload,
                header_fk,
            },
        ),
        PeerEvent::BlockDecodeFailed { peer, hash } => apply_block_decode_failed(st, peer, hash),
        PeerEvent::NotFound { peer, hashes } => apply_notfound(st, peer, hashes),
        PeerEvent::BlocksInv { peer, hashes } => {
            if !hashes.is_empty() {
                super::header_walk::note_block_inv(st, peer);
            }
        }
        PeerEvent::Addrs { peer, addrs } => {
            let allow = take_addr_tokens(st, peer, addrs.len());
            inject_learned_addrs(peer_book, &addrs[..allow], local_addr, peer);
        }
        PeerEvent::Dead { peer, reason } => apply_peer_dead(st, peer_book, peer, reason),
    }
}

pub fn apply_headers_event(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    peer: usize,
    headers: Vec<bitcoin::block::Header>,
) {
    if st.header_cmd.is_some() {
        super::state::enqueue_header_cmd(st, super::state::HeaderCmd::Batch { peer, headers });
        return;
    }
    let owned = std::mem::replace(st, IbdWorkState::new(Vec::new(), None, None));
    let shared = std::sync::Mutex::new(owned);
    let bus = super::peer_io::empty_header_fk_bus();
    run_header_batch(&shared, hub, peer, headers, &bus);
    *st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
}

#[doc(hidden)]
pub enum HeaderPlan {
    Done,
    Resume,
    Store {
        headers: Vec<bitcoin::block::Header>,
        known: Vec<rbitcoin_primitives::Fk>,
        inflight: super::header_walk::InFlight,
    },
}

#[doc(hidden)]
pub struct BatchMark {
    pub tip_before: Option<BlockHash>,
    pub work_before: [u8; 32],
    pub queued_before: usize,
    pub outer_len: usize,
}

#[doc(hidden)]
pub fn finish_mark(st: &mut IbdWorkState, hub: &ChainHub, peer: usize, mark: &BatchMark) {
    if mark.outer_len > 0 {
        super::header_walk::settle_walk_peer(
            st,
            hub,
            peer,
            mark.tip_before,
            mark.queued_before,
            mark.outer_len,
        );
    }
    super::header_walk::note_reservation(st, hub, peer, mark.tip_before, mark.work_before);
}

/// Classify under the lock, persist only the new suffix outside it, then
/// commit the in-memory path. This is the only header-batch apply.
pub fn run_header_batch(
    shared: &std::sync::Mutex<IbdWorkState>,
    hub: &ChainHub,
    peer: usize,
    headers: Vec<bitcoin::block::Header>,
    bus: &super::peer_io::HeaderFkBus,
) {
    let (mark, plan) = {
        let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
        plan_header_batch(&mut st, hub, peer, headers)
    };
    match plan {
        HeaderPlan::Done => {}
        HeaderPlan::Resume => {
            let exclude: Vec<[u8; 32]> = {
                let st = shared.lock().unwrap_or_else(|err| err.into_inner());
                st.reorg.invalid.iter().collect()
            };
            let path = super::path::load_resume_entries(hub, &exclude);
            let pairs: Vec<(BlockHash, rbitcoin_primitives::Fk)> = path
                .iter()
                .map(|e| (BlockHash::from_byte_array(e.hash), e.header_fk))
                .collect();
            {
                let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                super::path::stage_resumed_path(&mut st, hub, &path);
            }
            super::peer_io::publish_header_fks(bus, &pairs);
            let rewound = match super::reorg::rewind_off_coord_lock(shared, hub, Some(bus)) {
                Ok(rewound) => rewound,
                Err(e) => {
                    rbitcoin_log::warn!("ibd: header-thread rewind failed: {e}");
                    false
                }
            };
            let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
            if rewound {
                let _ = try_complete_awaiting_reorg(&mut st, hub);
            }
            let _ = super::header_walk::send_getheaders(&mut st, hub);
            finish_mark(&mut st, hub, peer, &mark);
        }
        HeaderPlan::Store {
            headers,
            known,
            mut inflight,
        } => {
            let (fks, work) = fks_for_kept(&known, hub, &headers[known.len()..]);
            let n = fks.len().min(headers.len());
            let pairs: Vec<(BlockHash, rbitcoin_primitives::Fk)> = headers
                .iter()
                .take(n)
                .map(|h| h.block_hash())
                .zip(fks.iter().copied())
                .collect();
            let ask = {
                let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                let added = on_headers_batch(&mut st, hub, peer, headers, &fks, work);
                let ask = complete_stored_headers(&mut st, hub, &mut inflight, added, n);
                finish_mark(&mut st, hub, peer, &mark);
                ask
            };
            super::peer_io::publish_header_fks(bus, &pairs);
            let rewound =
                super::reorg::rewind_off_coord_lock(shared, hub, Some(bus)).unwrap_or(false);
            if rewound {
                let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                let _ = try_complete_awaiting_reorg(&mut st, hub);
            }
            if ask {
                let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                let _ = super::header_walk::send_getheaders(&mut st, hub);
            }
        }
    }
}

/// Fks for a kept batch. Headers already in the work map are not written again.
#[doc(hidden)]
pub fn fks_for_kept(
    known: &[rbitcoin_primitives::Fk],
    hub: &ChainHub,
    rest: &[bitcoin::block::Header],
) -> (Vec<rbitcoin_primitives::Fk>, Option<bitcoin::Work>) {
    if rest.is_empty() {
        return (known.to_vec(), None);
    }
    let (new_fks, work) = prepare_header_store(hub, rest);
    let mut fks = known.to_vec();
    fks.extend(new_fks);
    (fks, work)
}

#[doc(hidden)]
pub fn plan_header_batch(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    peer: usize,
    headers: Vec<bitcoin::block::Header>,
) -> (BatchMark, HeaderPlan) {
    let mark = BatchMark {
        tip_before: super::header_walk::candidate_tip(st),
        work_before: super::header_walk::candidate_work(st),
        queued_before: st.ordered.len(),
        outer_len: headers.len(),
    };
    if mark.outer_len == 0 {
        super::header_walk::settle_walk_peer(
            st,
            hub,
            peer,
            mark.tip_before,
            mark.queued_before,
            mark.outer_len,
        );
    }
    let plan = plan_header_reply(st, hub, peer, headers);
    if matches!(plan, HeaderPlan::Done) {
        finish_mark(st, hub, peer, &mark);
    }
    (mark, plan)
}

#[doc(hidden)]
pub fn plan_header_reply(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    peer: usize,
    headers: Vec<bitcoin::block::Header>,
) -> HeaderPlan {
    let ask = super::header_walk::take_header_ask(st, peer);
    let mut inflight = super::header_walk::InFlight::capture(st);
    if headers.is_empty() {
        inflight.hold(st);
        if super::header_walk::note_empty(st, hub, peer) {
            return HeaderPlan::Done;
        }
        return if on_empty_headers(st, hub) {
            HeaderPlan::Resume
        } else {
            HeaderPlan::Done
        };
    }
    let class = super::header_walk::classify(st, hub, ask, &headers);
    let mut headers = headers;
    let keep = match class {
        super::header_walk::HeaderClass::WalkAbsorb => {
            let before = super::header_walk::candidate_tip(st);
            if super::header_walk::absorb_lookahead(st, hub, peer, ask, &headers)
                && super::header_walk::candidate_tip(st) == before
            {
                return HeaderPlan::Done;
            }
            headers.len()
        }
        super::header_walk::HeaderClass::WalkStore => headers.len(),
        super::header_walk::HeaderClass::Stray => {
            let before = super::header_walk::candidate_tip(st);
            if super::header_walk::suppress_competing_chain(st, hub, peer, &headers)
                || super::header_walk::candidate_tip(st) == before
            {
                return HeaderPlan::Done;
            }
            headers.len()
        }
        super::header_walk::HeaderClass::Refill => {
            if super::header_walk::reject_refill_miss(st, hub, &headers) {
                return HeaderPlan::Done;
            }
            super::header_walk::proven_header_prefix(st, hub, &headers)
        }
        super::header_walk::HeaderClass::Fork => {
            if super::header_walk::suppress_competing_chain(st, hub, peer, &headers) {
                return HeaderPlan::Done;
            }
            if super::header_walk::reject_refill_miss(st, hub, &headers) {
                return HeaderPlan::Done;
            }
            super::header_walk::proven_header_prefix(st, hub, &headers)
        }
    };
    if keep == 0 {
        return HeaderPlan::Done;
    }
    headers.truncate(keep);
    let known = headers
        .iter()
        .map_while(|h| st.header_fks.get(&h.block_hash()).copied())
        .collect();
    HeaderPlan::Store {
        headers,
        known,
        inflight,
    }
}

#[doc(hidden)]
pub fn complete_stored_headers(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    inflight: &mut super::header_walk::InFlight,
    added: usize,
    batch_len: usize,
) -> bool {
    inflight.hold(st);
    if added > 0 {
        st.empty_header_streak = 0;
        st.headers_done = false;
        let live = st.ordered_set.len();
        let need_ready_headroom = want_headers_beyond_soft_cap(
            live,
            st.body.known_len(),
            st.max_ordered_height.saturating_sub(st.max_ready_height),
            4096,
        );
        return batch_len >= MAX_HEADERS_RESULTS
            && live < MAX_ORDERED_HEADERS
            && (live < ORDERED_HEADERS_SOFT_CAP || need_ready_headroom);
    }
    // A stored plan has at least one header. An empty batch never reaches here.
    on_known_headers_batch(st, hub, 0, batch_len);
    false
}

pub struct FramedBody {
    pub peer: usize,
    pub hash: BlockHash,
    pub payload: Vec<u8>,
    pub header_fk: Option<rbitcoin_primitives::Fk>,
}

#[doc(hidden)]
pub fn apply_block_framed(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    archive_write_next: &AtomicU32,
    confirm_feed: Option<&super::confirm::ConfirmFeed>,
    body: FramedBody,
) {
    let FramedBody {
        peer,
        hash,
        payload,
        header_fk,
    } = body;
    let wire_bytes = payload.len();
    // Unsolicited wire is not a body we asked for. Drop it before any copy
    // and before it can move this peer's progress clock.
    let requested = st.inflight.contains_key(&hash);
    if !requested {
        return;
    }
    note_block_rx(&mut st.slots, peer, wire_bytes);
    super::assign::note_block_len(st, wire_bytes);
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    if st.body.is_rejected(&hash) || hub.has_block(&hash) {
        return;
    }
    let header_fk = if let Some(fk) = header_fk {
        st.header_fks.insert(hash, fk);
        fk
    } else if let Some(&fk) = st.header_fks.get(&hash) {
        fk
    } else {
        st.body.mark_missing(hash);
        return;
    };
    let tip_h = hub.tip_height().unwrap_or(0);
    let Some(height) = st.hash_height.get(&hash).copied() else {
        st.body.mark_missing(hash);
        return;
    };
    let write_next = archive_write_next.load(Ordering::Relaxed);
    let tip_hi = tip_h.saturating_add(CONTIG_DENSIFY_AHEAD);
    let densify_hi = write_next.saturating_add(CONTIG_DENSIFY_AHEAD);
    if height > tip_hi && height > densify_hi {
        st.body.mark_missing(hash);
        return;
    }
    let tip_hash = hub.tip_hash();
    if height <= tip_h && tip_hash != Some(hash) {
        if let Ok(block) = bitcoin::consensus::deserialize::<bitcoin::Block>(&payload) {
            st.reorg.hold_body(block);
            st.body.mark_pending(hash);
            if try_complete_awaiting_reorg(st, hub) {
                return;
            }
        }
    }
    if super::progress::claim_ready(hub, &mut st.body, height, &hash) {
        return;
    }
    let raw = hash.to_byte_array();
    if hub.query.block_queue_has_hash(&raw) {
        return;
    }
    match hub.query.block_queue_offer_from(
        height,
        raw,
        header_fk.0,
        &payload,
        u64::try_from(peer).ok(),
    ) {
        Ok(_offer) => {
            let _ = try_complete_awaiting_reorg(st, hub);
        }
        Err(e) => {
            if store_reject_drops_peer(&e) {
                warn!("ibd: peer[{peer}] block {hash} h={height}: {e}; dropping peer");
                st.body.mark_missing(hash);
                st.reopen_for_densify(&[hash]);
                disconnect_peer(st, peer);
                return;
            }
            rbitcoin_log::warn!("ibd: body queue offer failed ({e}) h={height}");
            st.body.mark_missing(hash);
            return;
        }
    }
    st.body.mark_pending(hash);
    if let Some(feed) = confirm_feed {
        feed.note(height, hash);
    }
}

/// Undecodable wire is the sender's block. Any other store refusal stays a
/// re-get: the peer may still have an honest copy.
#[doc(hidden)]
pub fn store_reject_drops_peer(err: &rbitcoin_store::StoreError) -> bool {
    match err {
        rbitcoin_store::StoreError::Rejected(why) => {
            *why == rbitcoin_query::Query::UNDECODABLE_WIRE_MSG
        }
        _ => false,
    }
}

#[doc(hidden)]
pub fn apply_block_decode_failed(st: &mut IbdWorkState, peer: usize, hash: BlockHash) {
    note_block_progress(&mut st.slots, peer);
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    if st.body.is_pending(&hash) {
        st.body.mark_missing(hash);
    }
}

#[doc(hidden)]
pub fn apply_notfound(st: &mut IbdWorkState, peer: usize, hashes: Vec<BlockHash>) {
    note_block_progress(&mut st.slots, peer);
    let mut freed = Vec::new();
    if let Some(s) = st.slots.iter_mut().find(|s| s.id == peer) {
        for h in &hashes {
            s.track_remove(h);
            let empty = st
                .inflight
                .get_mut(h)
                .map(|e| e.remove_peer(peer))
                .unwrap_or(false);
            if empty {
                st.inflight.remove(h);
                if st.body.is_pending(h) {
                    st.body.mark_missing(*h);
                }
                freed.push(*h);
            }
        }
    }
    st.reopen_for_densify(&freed);
}

/// How many of `want` addresses from `peer` may enter the dial book. The
/// bucket counts from the peer's connect on the IBD mono clock. A peer with
/// the `addr` permission spends tokens but is never limited, as in tip-follow.
#[doc(hidden)]
pub fn take_addr_tokens(st: &mut IbdWorkState, peer: usize, want: usize) -> usize {
    let now = super::peer_io::ibd_mono_ms();
    let slot = st.slots.iter().find(|s| s.id == peer);
    let start = slot.map_or(now, |s| s.connected_ms);
    // IBD dials outbound only, so the peer address fills the bind slot.
    let exempt = slot.zip(st.perms.as_ref()).is_some_and(|(s, p)| {
        p.permission_flags(s.addr, false, s.addr)
            .has(rbitcoin_net::net_permissions::NetPermissionFlags::ADDR)
    });
    let took = st
        .addr_buckets
        .entry(peer)
        .or_insert_with(|| rbitcoin_net::peer::AddrTokenBucket::new(start))
        .take(want, now);
    if exempt {
        want
    } else {
        took
    }
}

#[doc(hidden)]
pub fn apply_peer_dead(
    st: &mut IbdWorkState,
    peer_book: &mut AddrMan,
    peer: usize,
    reason: String,
) {
    warn!("ibd: peer[{peer}] dead: {reason}");
    st.addr_buckets.remove(&peer);
    super::header_walk::forget_walk_peer(st, peer);
    if let Some(s) = st.slots.iter().find(|s| s.id == peer) {
        if reason == "peer misbehavior threshold" {
            note_misbehavior_dead(peer_book, &mut st.addr_cooldown, s.net, Instant::now());
        } else {
            note_dead_without_block_bytes(
                peer_book,
                &mut st.addr_cooldown,
                s.net,
                s.first_data_ms,
                Instant::now(),
            );
        }
        let lat = s.first_data_ms.saturating_sub(s.connected_ms);
        let cooled = s
            .net
            .socket_addr()
            .is_some_and(|sock| st.addr_cooldown.contains_key(&sock));
        peer_book.apply_ibd_dead_speed_addr(s.net, lat, s.rate.bps(), cooled);
    }
    let freed = release_peer_block_work(&mut st.slots, &mut st.inflight, &mut st.body, peer);
    st.reopen_for_densify(&freed);
}

/// Grow the IBD dial book from peer-advertised addresses (getaddr responses).
pub fn inject_learned_addrs(
    book: &mut AddrMan,
    addrs: &[rbitcoin_net::NetAddr],
    local_addr: SocketAddr,
    from_peer: usize,
) {
    if addrs.is_empty() {
        return;
    }
    let mut added = 0usize;
    for &a in addrs {
        if a.port() == 0 || rbitcoin_net::netaddr::netaddr_unroutable_gossip(a) {
            continue;
        }
        match a {
            rbitcoin_net::NetAddr::Ip(s) if s == local_addr => continue,
            _ => {}
        }
        if book.add_learned_addr(a, MAX_PEER_POOL) {
            added += 1;
        }
    }
    if added > 0 {
        rbitcoin_log::debug!(
            "ibd: peer[{from_peer}] taught {added} addr(s); book={}",
            book.len()
        );
    }
}

/// Permanent confirm failure: drop from the work path and never re-offer.
///
/// Without this, `offer_confirm_ready` re-noted ghost/re-queued hashes and the
/// confirm engine spun on the same BadPrev / missing-prevout tip+1 (signet log:
/// same hash every ~30s with tip frozen).
pub fn update_confirm_lag(lag: &AtomicU32, tip: Option<u32>, max_ready: u32) {
    let t = tip.unwrap_or(0);
    lag.store(max_ready.saturating_sub(t), Ordering::Relaxed);
}

pub fn apply_confirm_events(
    st: &mut IbdWorkState,
    hub: &ChainHub,
    rx: &std::sync::mpsc::Receiver<super::confirm::ConfirmEvent>,
    archive_write_next: &AtomicU32,
    max_ready_shared: &AtomicU32,
    last_progress: &mut Instant,
    feed: Option<&super::confirm::ConfirmFeed>,
) {
    while let Ok(ev) = rx.try_recv() {
        match ev {
            super::confirm::ConfirmEvent::Accepted { hash } => {
                *last_progress = Instant::now();
                st.confirm_stuck_since = None;
                remove_from_ordered(&mut st.ordered, &mut st.ordered_set, hash);
                st.body.mark_archived(hash);
                let tip = hub.tip_height().unwrap_or(0);
                archive_write_next.store(tip.saturating_add(1), Ordering::Relaxed);
                st.max_ready_height = st.max_ready_height.max(tip);
                max_ready_shared.store(st.max_ready_height, Ordering::Relaxed);
                if let Some(f) = feed {
                    f.release_isolate_if_tip(tip);
                }
            }
            super::confirm::ConfirmEvent::Reject {
                height,
                hash,
                class,
                err,
                batch_len,
                sender,
                parent,
            } => {
                apply_confirm_reject(
                    st,
                    height,
                    hash,
                    class,
                    &err,
                    Some(hub.query.as_ref()),
                    Some(hub),
                    batch_len,
                    feed,
                    sender,
                    parent,
                );
            }
        }
    }
    if st.confirm_quiesce {
        if let Some(f) = feed {
            f.clear();
        }
        st.confirm_quiesce = false;
    }
}

#[allow(clippy::too_many_arguments)] // call-site args stay unbundled
pub fn apply_confirm_reject(
    st: &mut IbdWorkState,
    height: u32,
    hash: BlockHash,
    class: super::confirm::ConfirmRejectClass,
    err: &str,
    query: Option<&rbitcoin_query::Query>,
    hub: Option<&rbitcoin_net::chain::ChainHub>,
    batch_len: usize,
    feed: Option<&super::confirm::ConfirmFeed>,
    sender: Option<usize>,
    parent: super::confirm::RejectParent,
) {
    use super::confirm::ConfirmRejectClass;
    let sent = class;
    let class = if class == ConfirmRejectClass::ConsensusInvalid {
        if let Some(h) = hub {
            class.trust_consensus(parent, h.tip_hash())
        } else {
            class
        }
    } else {
        class
    };
    let class = class.isolate_if_batched(batch_len);
    if class == ConfirmRejectClass::Cascade && batch_len > 1 {
        if let Some(f) = feed {
            let until = height.saturating_add(batch_len as u32).saturating_sub(1);
            f.request_single_block(until);
        }
    }
    if class.is_soft() {
        apply_soft_wire_reject(st, height, hash, err, query, hub, sender);
        return;
    }
    match class {
        ConfirmRejectClass::Cancelled => {
            warn!("ibd: confirm reject cancelled @{height} {hash}: {err}");
        }
        ConfirmRejectClass::SoftWire => {}
        ConfirmRejectClass::Cascade => {
            apply_cascade_reject(st, height, hash, err, hub);
            if sent == ConfirmRejectClass::ConsensusInvalid && batch_len == 1 {
                reget_dropped_verdict_body(st, hash);
            }
        }
        ConfirmRejectClass::EngineFault => {
            apply_engine_fault_reject(st, height, hash, err, query);
        }
        ConfirmRejectClass::ConsensusInvalid => {
            apply_consensus_invalid_reject(st, height, hash, err, query, hub);
        }
    }
}

#[doc(hidden)]
pub fn apply_soft_wire_reject(
    st: &mut IbdWorkState,
    height: u32,
    hash: BlockHash,
    err: &str,
    query: Option<&rbitcoin_query::Query>,
    hub: Option<&rbitcoin_net::chain::ChainHub>,
    sender: Option<usize>,
) {
    let bad_prev = super::reorg::is_bad_prev_err(err);
    // A BadPrev whose parent is the current tip is a missing header slot,
    // not a competing fork. Keep the hash so densify asks for it again.
    let retry_tip_child = bad_prev && prev_is_hub_tip(st, hub, hash);
    if bad_prev {
        st.headers_done = false;
        if let Some(h) = hub {
            st.reorg
                .register_explore(std::iter::empty::<bitcoin::BlockHash>(), Some(hash));
            let rewound = super::reorg::maybe_rewind_to_best_work(st, h).unwrap_or(false);
            if rewound {
                return;
            }
            if !retry_tip_child && st.height_to_hash.get(&height) == Some(&hash) {
                st.height_to_hash.remove(&height);
                remove_from_ordered(&mut st.ordered, &mut st.ordered_set, hash);
            }
        } else if st.height_to_hash.get(&height) == Some(&hash) {
            st.height_to_hash.remove(&height);
            remove_from_ordered(&mut st.ordered, &mut st.ordered_set, hash);
        }
    }
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    if let Some(q) = query {
        let _ = q.block_queue_dequeue_height(height);
        if rbitcoin_net::chain::reject_is_mutated(err) {
            match q.clear_archived_body(hash.as_byte_array()) {
                Ok(true) => {
                    warn!("ibd: cleared corrupt Class A body for {hash} @{height} (mutated body)")
                }
                Ok(false) => {}
                Err(e) => warn!("ibd: clear Class A body {hash} @{height}: {e}"),
            }
        }
    }
    if !bad_prev || retry_tip_child {
        // `isolate_if_batched` leaves SoftWire only for a one-block wave, so
        // `hash` is the block that failed and `sender` sent that copy. The
        // live pending sender may be a replacement's.
        if !retry_tip_child && rbitcoin_net::chain::reject_is_mutated(err) {
            if let Some(peer) = sender {
                punish_mutated_sender(st, peer, hash);
            }
        }
        // A batched soft reject was isolated as a cascade. It landed here on
        // its own block, so it does not count toward the cascade halt. This
        // also clears a count for another hash: the tip+1 retry got a
        // verdict, so the pipeline is not stuck repeating that cascade.
        st.cascade_at = None;
        st.body.mark_missing(hash);
        st.reopen_for_densify(&[hash]);
        st.body.demote_known(hash);
        st.reopen_for_densify(&[hash]);
        if retry_tip_child {
            warn!(
                "ibd: confirm reject BadPrev @{height} {hash}: {err} (parent is tip, re-getdata)"
            );
        } else {
            warn!("ibd: confirm reject soft @{height} {hash}: {err} (re-getdata, not blacklisted)");
        }
    } else {
        warn!(
            "ibd: confirm reject BadPrev @{height} {hash}: {err} (slot evicted, not re-get same hash)"
        );
    }
}

/// True when `hash` chains directly off the hub tip.
#[doc(hidden)]
pub fn prev_is_hub_tip(
    st: &IbdWorkState,
    hub: Option<&rbitcoin_net::chain::ChainHub>,
    hash: BlockHash,
) -> bool {
    let Some(h) = hub else {
        return false;
    };
    let Some(tip) = h.tip_hash() else {
        return false;
    };
    st.path_prev
        .get(&hash)
        .copied()
        .or_else(|| h.prev_of(&hash))
        == Some(tip)
}

/// Core `MaybePunishNodeForBlock` (`BLOCK_MUTATED`): drop the sender and
/// cool down its address so the re-get goes to another peer. Redial falls
/// back to a cooling address when no other is dialable. A noban peer stays.
#[doc(hidden)]
pub fn punish_mutated_sender(st: &mut IbdWorkState, peer: usize, hash: BlockHash) {
    let Some(addr) = st
        .slots
        .iter()
        .find(|s| s.id == peer && s.alive)
        .map(|s| s.addr)
    else {
        return;
    };
    // IBD dials outbound only. A bind address only matters for inbound
    // (`-whitebind`), so the peer address fills that slot.
    let noban = st.perms.as_ref().is_some_and(|p| {
        p.is_noban()
            || p.permission_flags(addr, false, addr)
                .has(rbitcoin_net::net_permissions::NetPermissionFlags::NOBAN)
    });
    if noban {
        warn!("ibd: peer[{peer}] sent mutated block {hash}; not punishing noban peer");
        return;
    }
    warn!("ibd: peer[{peer}] sent mutated block {hash}; dropping peer");
    disconnect_peer(st, peer);
}

#[doc(hidden)]
pub fn apply_cascade_reject(
    st: &mut IbdWorkState,
    height: u32,
    hash: BlockHash,
    err: &str,
    hub: Option<&rbitcoin_net::chain::ChainHub>,
) {
    // Leave the body queue: the plan was stale, and the confirm thread that
    // rejected offered any body it held back.
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    const CASCADE_HALT_AFTER: u8 = 3;
    let tip = hub
        .and_then(|h| h.tip_hash())
        .map(|t| t.to_byte_array())
        .unwrap_or([0u8; 32]);
    let n = match st.cascade_at {
        Some((h, t, c)) if h == hash && t == tip => c.saturating_add(1),
        _ => 1u8,
    };
    st.cascade_at = Some((hash, tip, n));
    if n >= CASCADE_HALT_AFTER {
        st.halt = Some(format!(
            "cascade repeated {n}× @{height} {hash} (tip unchanged): {err}"
        ));
        warn!(
            "ibd: confirm reject cascade halt @{height} {hash}: {err} ({n} at same tip, not blacklisted)"
        );
        return;
    }
    note_confirm_stuck(st);
    warn!("ibd: confirm reject cascade @{height} {hash}: {err} (requeue, not blacklisted)");
}

/// Load, scripts, and write each drop the body of a lone block they judge
/// invalid. When apply no longer trusts that verdict (the block's parent is
/// not the tip now), no stage holds the body: ask for it again.
#[doc(hidden)]
pub fn reget_dropped_verdict_body(st: &mut IbdWorkState, hash: BlockHash) {
    st.body.mark_missing(hash);
    st.reopen_for_densify(&[hash]);
}

#[doc(hidden)]
pub fn apply_engine_fault_reject(
    st: &mut IbdWorkState,
    height: u32,
    hash: BlockHash,
    err: &str,
    query: Option<&rbitcoin_query::Query>,
) {
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    if st.engine_fault_seen.contains(&hash) {
        if let Some(q) = query {
            let _ = q.block_queue_dequeue_height(height);
        }
        st.halt = Some(format!("engine fault repeated @{height} {hash}: {err}"));
        warn!(
            "ibd: engine fault halt @{height} {hash}: {err} (second occurrence, not blacklisted)"
        );
        return;
    }
    st.engine_fault_seen.insert(hash);
    note_confirm_stuck(st);
    warn!(
        "ibd: confirm reject engine-fault @{height} {hash}: {err} (requeue once, not blacklisted)"
    );
}

#[doc(hidden)]
pub fn apply_consensus_invalid_reject(
    st: &mut IbdWorkState,
    height: u32,
    hash: BlockHash,
    err: &str,
    query: Option<&rbitcoin_query::Query>,
    hub: Option<&rbitcoin_net::chain::ChainHub>,
) {
    if let Some(q) = query {
        let _ = q.block_queue_dequeue_height(height);
    }
    st.body.mark_rejected(hash);
    st.reorg.invalid.mark(hash.to_byte_array());
    if st.height_to_hash.get(&height) == Some(&hash) {
        st.height_to_hash.remove(&height);
    }
    remove_from_ordered(&mut st.ordered, &mut st.ordered_set, hash);
    clear_hash_inflight(&mut st.slots, &mut st.inflight, hash);
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 8 || n.is_multiple_of(50) {
        warn!("ibd: confirm reject applied {hash} @{height}: {err} (consensus-invalid, count={n})");
    }
    if let Some(h) = hub {
        for t in super::reorg::competing_valid_header_tips(st, h) {
            st.reorg
                .register_explore(std::iter::empty::<bitcoin::BlockHash>(), Some(t));
        }
        let rewound = super::reorg::maybe_rewind_to_best_work(st, h).unwrap_or(false);
        if !rewound {
            super::path::seed_work_path_from_store(st, h);
        }
        super::path::plant_valid_tip_child(st, h);
        let expect = if h.tip_height().is_none() {
            0u32
        } else {
            h.tip_height().unwrap_or(0).saturating_add(1)
        };
        let next_ok = st.height_to_hash.get(&expect).is_some_and(|nh| {
            !st.reorg.invalid.contains(nh.to_byte_array()) && !st.body.is_rejected(nh)
        });
        if rewound || next_ok {
            st.confirm_stuck_since = None;
            return;
        }
    }
    note_confirm_stuck(st);
}

#[doc(hidden)]
pub fn note_confirm_stuck(st: &mut IbdWorkState) {
    if st.confirm_stuck_since.is_none() {
        st.confirm_stuck_since = Some(Instant::now());
    }
}

/// Proactive most-work apply: header-work rewind only (no gathered `accept_branch`).
#[doc(hidden)]
pub fn try_apply_exploration(st: &mut IbdWorkState, hub: &rbitcoin_net::chain::ChainHub) -> bool {
    if st.reorg.explore_tips().is_empty() {
        return false;
    }
    match super::reorg::maybe_rewind_to_best_work(st, hub) {
        Ok(true) => {
            rbitcoin_log::info!(
                "ibd: most-work header rewind after exploration (no accept_branch)"
            );
            true
        }
        Ok(false) => {
            st.reorg.clear_explore();
            false
        }
        Err(e) => {
            warn!("ibd: exploration rewind failed: {e}");
            false
        }
    }
}

/// After a side-branch body is held (or BQ has mids), try to finish an awaiting reorg
/// by header-work rewind (never `accept_branch` of gathered bodies).
pub fn try_complete_awaiting_reorg(
    st: &mut IbdWorkState,
    hub: &rbitcoin_net::chain::ChainHub,
) -> bool {
    try_apply_exploration(st, hub)
}

pub fn parent_height(
    hash_height: &HashMap<BlockHash, u32>,
    hub: &ChainHub,
    prev: BlockHash,
) -> Option<u32> {
    if prev.to_byte_array() == [0u8; 32] {
        return Some(0);
    }
    if let Some(&ph) = hash_height.get(&prev) {
        return Some(ph.saturating_add(1));
    }
    if hub.tip_hash() == Some(prev) {
        return Some(hub.tip_height().unwrap_or(0).saturating_add(1));
    }
    // Confirmed ancestor (tip−1 / deeper): competing headers often attach to a
    // non-tip parent that is not yet in the RAM height map. height_of_hash is
    // best-chain only — orphan prevs stay None (peer batch_prev can fill).
    if let Ok(Some(h)) = hub.query.height_of_hash(&prev.to_byte_array()) {
        return Some(h.0.saturating_add(1));
    }
    None
}
