//! Pure ordered-path helpers (no peer IO).

use bitcoin::BlockHash;
use std::collections::{HashSet, VecDeque};

/// Mark `h` done in the ordered set. Prefer O(1) front pop; otherwise leave a
/// stale ordered entry. Ghosts (removed from set but still in the deque) are
/// front-trimmed here so the next front is always a live set member when possible.
///
/// **Ghosts:** middle removes only update `ordered_set`. Entries may remain in
/// the deque until front-trim or [`compact_ordered`]. Scans that walk `ordered`
/// should skip hashes not in `ordered_set` (or use set membership).
pub fn remove_from_ordered(
    ordered: &mut VecDeque<BlockHash>,
    ordered_set: &mut HashSet<BlockHash>,
    h: BlockHash,
) {
    ordered_set.remove(&h);
    if ordered.front() == Some(&h) {
        ordered.pop_front();
    }
    while let Some(&front) = ordered.front() {
        if ordered_set.contains(&front) {
            break;
        }
        ordered.pop_front();
    }
}

/// Rebuild `ordered` to match `ordered_set` (drop middle ghosts).
///
/// Cheap no-op when the deque is not much larger than the live set.
pub fn compact_ordered(ordered: &mut VecDeque<BlockHash>, ordered_set: &HashSet<BlockHash>) {
    let len = ordered.len();
    if len <= 64 {
        return;
    }
    let live = ordered_set.len();
    let ghost_budget = (live / 2).max(64);
    if len <= live.saturating_add(ghost_budget) {
        return;
    }
    let mut next = VecDeque::with_capacity(live);
    for h in ordered.iter().copied() {
        if ordered_set.contains(&h) {
            next.push_back(h);
        }
    }
    *ordered = next;
}

/// Densify slots per peer: none while a tip hole exists, else half of `per_peer`.
///
/// When tip+1 is a fetch hole, new densify would sit in Core's getdata FIFO
/// ahead of (or mixed with) tip-hole requests — mainnet sat hole=1 with
/// conf_blks=0 while bq/feed grew far bodies. Existing inflight densify may
/// finish; assign issues no further far getdata until the prefix is in hand.
pub fn far_slots_per_peer(per_peer: usize, tip_hole: bool) -> usize {
    if tip_hole {
        0
    } else {
        (per_peer / 2).max(1)
    }
}

/// Per-peer densify cap: 0 while a tip hole is open; otherwise half of
/// `per_peer`, or `per_peer` when this peer's EWMA bps is ≥ 2× pack median
/// and the pack is not a tight cluster.
pub fn densify_slots_for_peer(
    per_peer: usize,
    tip_hole: bool,
    peer_bps: Option<u64>,
    pack_median: Option<u64>,
    pack_tight: bool,
) -> usize {
    let base = far_slots_per_peer(per_peer, tip_hole);
    if tip_hole || pack_tight {
        return base;
    }
    let Some(bps) = peer_bps else {
        return base;
    };
    let Some(med) = pack_median else {
        return base;
    };
    if med > 0 && bps >= med.saturating_mul(2) {
        per_peer
    } else {
        base
    }
}

/// Whether to request more headers past the soft cap.
///
/// Only when the ordered path is **mostly claim-ready** (dense body-queue /
/// Class A readiness) and the gap from max_ready to max_ordered is short —
/// never for sparse far-only header floods without bodies.
///
/// `live == 0` is **not** “beyond soft cap.” Empty-path getheaders is a
/// separate cadence (main loop / empty-headers lag). Treating empty as
/// headroom made every already-known 1-header announce re-issue getheaders
/// (mainnet tip: ~1k INFO lines/s).
/// Soft-cap headroom only when headers are wanted and this walk has no
/// checkpoints yet. A checkpointed walk does not ask past the soft cap.
pub fn need_ready_headroom(want: bool, have_checkpoints: bool) -> bool {
    want && !have_checkpoints
}

pub fn want_headers_beyond_soft_cap(
    live: usize,
    known_ready: usize,
    ready_gap: u32,
    gap_need: u32,
) -> bool {
    if live == 0 {
        return false;
    }
    let mostly_ready = known_ready >= live.saturating_mul(3) / 4;
    mostly_ready && ready_gap < gap_need
}

/// Admit a header onto `ordered` (first time or post-drain re-admit).
///
/// Re-admit after tip-drain/hygiene emptied `ordered` is required (mainnet
/// 292k). Do not re-queue work that is already inflight, pending/BQ, rejected,
/// confirmed, or at/below tip — those 1-header announces are tip chatter.
pub fn should_enqueue_header(
    in_ordered: bool,
    inflight: bool,
    pending: bool,
    rejected: bool,
    has_block: bool,
    height: Option<u32>,
    tip_h: Option<u32>,
) -> bool {
    if in_ordered || inflight || pending || rejected || has_block {
        return false;
    }
    !matches!((height, tip_h), (Some(ht), Some(tip)) if ht <= tip)
}
