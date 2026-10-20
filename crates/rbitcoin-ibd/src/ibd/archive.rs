//! Body-queue rehydrate into confirm after restart.
//!
//! Dual-track archive-job Class A pipeline was removed — confirm is sole Class A.
//! After restart the RAM body queue is empty; Class A bodies on the tip path are
//! reconstructed into the queue so confirm can claim without re-getdata.

use bitcoin::consensus::Encodable;
use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_net::chain::ChainHub;
use rbitcoin_primitives::Fk;

use std::time::Instant;

/// Parent from the stored header, once. The orchestration loop does not do this.
#[doc(hidden)]
pub fn header_parent(hub: &ChainHub, header_fk: Fk) -> Option<BlockHash> {
    if header_fk.is_null() {
        return None;
    }
    let rec = hub.query.get_header(header_fk).ok()?;
    if rec.prev_fk.is_null() {
        return Some(BlockHash::from_byte_array([0u8; 32]));
    }
    let parent = hub.query.get_header(rec.prev_fk).ok()?;
    Some(BlockHash::from_byte_array(parent.hash))
}

/// True when `hash` extends the work path or the confirmed tip. Records the
/// parent either way. A competing leftover stays off the slot and the feed.
#[doc(hidden)]
pub fn leftover_chains(
    hub: &ChainHub,
    st: &mut super::state::IbdWorkState,
    height: u32,
    header_fk: Fk,
    hash: BlockHash,
) -> bool {
    let Some(prev) = header_parent(hub, header_fk) else {
        return false;
    };
    st.note_path_prev(hash, prev);
    if height == 0 {
        return prev.to_byte_array() == [0u8; 32];
    }
    if let Some(parent) = st.height_to_hash.get(&height.saturating_sub(1)) {
        return *parent == prev;
    }
    // Startup seeds the tip into `height_to_hash` before rehydrate, so a
    // missing parent slot is not the tip's child.
    false
}

pub fn rehydrate_block_queue_into_confirm(
    hub: &ChainHub,
    st: &mut super::state::IbdWorkState,
    confirm_feed: &super::confirm::ConfirmFeed,
) -> Result<usize, String> {
    use rbitcoin_log::{info, warn};

    let tip_opt = hub.tip_height();
    let path_lo = match tip_opt {
        None => 0u32,
        Some(t) => t.saturating_add(1),
    };

    // Index only. After restart the RAM queue is empty (by design — sole durable
    // write is Class A; redownload instead of double disk write). Same-process
    // residual still notes feed readiness.
    let queued = hub.query.block_queue_list_meta();
    if queued.is_empty() {
        return Ok(0);
    }
    let mut n = 0usize;
    let mut bytes = 0u64;
    let mut h_min = u32::MAX;
    let mut h_max = 0u32;
    let mut dropped_done = 0usize;
    let mut empty_skip = 0usize;
    let mut unknown_h = 0usize;
    let mut kept_above_tip_flag = 0usize;
    for qb in queued {
        let hash = BlockHash::from_byte_array(qb.hash);
        // Only drop residue at/below confirmed tip (write may have missed dequeue).
        // Heights above tip always keep wire — even if has_block/known looks set
        // (stale RAM or Class A ahead of tip must not erase confirm payload).
        // No tip yet → keep every height (including 0).
        let at_or_below_tip = matches!(
            tip_opt,
            Some(tip) if qb.height != u32::MAX && qb.height <= tip
        );
        if at_or_below_tip {
            let _ = hub.query.block_queue_dequeue_height(qb.height);
            dropped_done = dropped_done.saturating_add(1);
            continue;
        }
        if hub.has_block(&hash) || st.body.is_known_archived(&hash) {
            kept_above_tip_flag = kept_above_tip_flag.saturating_add(1);
        }
        if qb.payload_len == 0 {
            empty_skip = empty_skip.saturating_add(1);
            let _ = hub.query.block_queue_dequeue_height(qb.height);
            continue;
        }
        let wire_bytes = qb.payload_len;
        st.body.mark_pending(hash);
        let header_fk = Fk(qb.header_fk);
        if !header_fk.is_null() {
            st.header_fks.insert(hash, header_fk);
        }
        if qb.height != u32::MAX {
            if leftover_chains(hub, st, qb.height, header_fk, hash) {
                st.record_height(hash, qb.height);
                confirm_feed.note(qb.height, hash);
                bytes = bytes.saturating_add(wire_bytes);
                h_min = h_min.min(qb.height);
                h_max = h_max.max(qb.height);
                n = n.saturating_add(1);
            }
        } else {
            st.body.mark_missing(hash);
            unknown_h = unknown_h.saturating_add(1);
        }
    }

    // Tip+1..bq_min gap: wire was dequeued/never filled while tip lagged → densify.
    // Skip only **claim-ready** heights (confirmed / pending / BQ). Class A alone
    // (resume seed `mark_archived`) is not claimable — still mark_missing so
    // tip-hole race / densify re-getdata into the body queue.
    let mut gap_marked = 0u32;
    if n > 0 && h_min > path_lo {
        for ht in path_lo..h_min {
            let Some(&hash) = st.height_to_hash.get(&ht) else {
                continue;
            };
            if hub.has_block(&hash)
                || st.body.is_pending(&hash)
                || hub.query.block_queue_has_height(ht)
            {
                continue;
            }
            st.body.mark_missing(hash);
            gap_marked = gap_marked.saturating_add(1);
        }
        if gap_marked > 0 {
            warn!(
                "ibd: body queue gap tip+1={path_lo}..{} (bq starts {h_min}) — \
                 marked {gap_marked} missing for densify re-getdata",
                h_min.saturating_sub(1)
            );
        }
    }

    if n > 0 || dropped_done > 0 || empty_skip > 0 || unknown_h > 0 || gap_marked > 0 {
        let mib = bytes / (1024 * 1024);
        if n > 0 {
            info!(
                "ibd: rehydrate body queue → feed ready n={n} h={h_min}..{h_max} \
                 {mib}MiB (dropped_le_tip={dropped_done} empty={empty_skip} unknown_h={unknown_h} \
                 gap_marked={gap_marked} kept_above_tip_flag={kept_above_tip_flag})"
            );
        } else {
            info!(
                "ibd: rehydrate body queue: no ready entries \
                 (dropped_le_tip={dropped_done} empty={empty_skip} unknown_h={unknown_h} \
                 gap_marked={gap_marked})"
            );
        }
        if empty_skip > 0 {
            warn!("ibd: rehydrate dropped {empty_skip} empty body-queue rec(s)");
        }
    }
    Ok(n)
}

/// Reconstruct Class A bodies from tip+1 into the RAM body queue (restart path).
///
/// Body queue is RAM-only (no durable wire). Resume seed marks Class A as known
/// for densify bookkeeping, but confirm intake is **BQ wire only**. Prefer
/// Class A reconstruct → `block_queue_offer` over peer re-getdata for the tip
/// confirm batch (`max`, typically [`super::TIP_HOLE_MAX`]).
///
/// Walks **contiguous** heights from tip+1: stops at the first height without
/// Class A (or reconstruct/offer failure → mark_missing so assign re-gets).
/// Already-queued / pending / confirmed heights are skipped without breaking
/// the contiguous walk when still claim-ready.
pub fn rehydrate_class_a_into_body_queue(
    hub: &ChainHub,
    st: &mut super::state::IbdWorkState,
    confirm_feed: &super::confirm::ConfirmFeed,
    max: usize,
) -> Result<usize, String> {
    use rbitcoin_log::info;

    if max == 0 {
        return Ok(0);
    }
    let path_lo = match hub.tip_height() {
        None => 0u32,
        Some(t) => t.saturating_add(1),
    };
    let t0 = Instant::now();
    let mut n = 0usize;
    let mut bytes = 0u64;
    let mut h_min = u32::MAX;
    let mut h_max = 0u32;
    let mut failed = 0u32;

    for ht in path_lo.. {
        if n >= max {
            break;
        }
        match rehydrate_one_height(hub, st, confirm_feed, ht)? {
            RehydrateOne::Offered { bytes: b } => {
                bytes = bytes.saturating_add(b);
                h_min = h_min.min(ht);
                h_max = h_max.max(ht);
                n = n.saturating_add(1);
            }
            RehydrateOne::Queued => {
                h_min = h_min.min(ht);
                h_max = h_max.max(ht);
                n = n.saturating_add(1);
            }
            RehydrateOne::Stop { failed: f } => {
                if f {
                    failed = failed.saturating_add(1);
                }
                break;
            }
        }
    }

    if n > 0 || failed > 0 {
        let mib = bytes / (1024 * 1024);
        info!(
            "ibd: Class A rehydrate → body queue n={n} h={h_min}..{h_max} \
             {mib}MiB fail={failed} (store reconstruct {:?})",
            t0.elapsed()
        );
    }
    Ok(n)
}

#[doc(hidden)]
pub enum RehydrateOne {
    Offered { bytes: u64 },
    Queued,
    Stop { failed: bool },
}

#[doc(hidden)]
pub fn rehydrate_one_height(
    hub: &ChainHub,
    st: &mut super::state::IbdWorkState,
    confirm_feed: &super::confirm::ConfirmFeed,
    ht: u32,
) -> Result<RehydrateOne, String> {
    use rbitcoin_log::warn;
    let Some(&hash) = st.height_to_hash.get(&ht) else {
        return Ok(RehydrateOne::Stop { failed: false });
    };
    let path_lo = match hub.tip_height() {
        None => 0u32,
        Some(t) => t.saturating_add(1),
    };
    if hub.has_block(&hash) {
        if ht == path_lo && hub.tip_height().is_some() {
            warn!(
                "ibd: tip+1={ht} {hash} is in confirmed-set while tip={} — \
                 confirmed[] likely maps an earlier height to tip+1; \
                 restart after tip revalidate / open repair (or fresh datadir)",
                hub.tip_height().unwrap_or(0)
            );
        }
        return Ok(RehydrateOne::Stop { failed: false });
    }
    if st.body.is_rejected(&hash) {
        return Ok(RehydrateOne::Stop { failed: false });
    }
    if hub.query.block_queue_has_height(ht) {
        st.body.mark_pending(hash);
        confirm_feed.note(ht, hash);
        return Ok(RehydrateOne::Queued);
    }
    if st.body.is_pending(&hash) {
        st.body.mark_missing(hash);
    }
    let has_class_a = st.body.is_known_archived(&hash)
        || hub
            .query
            .is_block_archived(&hash.to_byte_array())
            .unwrap_or(false);
    if !has_class_a {
        return Ok(RehydrateOne::Stop { failed: false });
    }
    rehydrate_offer_class_a(hub, st, confirm_feed, ht, hash)
}

#[doc(hidden)]
pub fn rehydrate_offer_class_a(
    hub: &ChainHub,
    st: &mut super::state::IbdWorkState,
    confirm_feed: &super::confirm::ConfirmFeed,
    ht: u32,
    hash: BlockHash,
) -> Result<RehydrateOne, String> {
    use rbitcoin_log::warn;
    let block = match hub.query.reconstruct_archived_block(&hash.to_byte_array()) {
        Ok(Some(b)) => b,
        Ok(None) => {
            warn!(
                "ibd: Class A rehydrate h={ht}: header_txs missing after archive flag — re-getdata"
            );
            st.body.mark_missing(hash);
            return Ok(RehydrateOne::Stop { failed: true });
        }
        Err(e) => {
            warn!("ibd: Class A rehydrate reconstruct h={ht} {hash}: {e} — re-getdata");
            st.body.mark_missing(hash);
            return Ok(RehydrateOne::Stop { failed: true });
        }
    };
    let merkle_ok = block
        .compute_merkle_root()
        .is_some_and(|mr| mr == block.header.merkle_root);
    if !merkle_ok || block.block_hash() != hash {
        warn!(
            "ibd: Class A rehydrate h={ht} {hash}: reconstructed body fails header check \
             (merkle/hash) — clear Class A and re-getdata"
        );
        let _ = hub.query.clear_archived_body(hash.as_byte_array());
        st.body.demote_known(hash);
        st.body.mark_missing(hash);
        return Ok(RehydrateOne::Stop { failed: true });
    }
    let mut payload = Vec::new();
    if block.consensus_encode(&mut payload).is_err() {
        warn!("ibd: Class A rehydrate encode h={ht} {hash} failed — re-getdata");
        st.body.mark_missing(hash);
        return Ok(RehydrateOne::Stop { failed: true });
    }
    let header_fk = st.header_fks.get(&hash).copied().unwrap_or(Fk::NULL);
    if let Err(e) = hub
        .query
        .block_queue_offer(ht, hash.to_byte_array(), header_fk.0, &payload)
    {
        warn!("ibd: Class A rehydrate offer h={ht}: {e} — re-getdata");
        st.body.mark_missing(hash);
        return Ok(RehydrateOne::Stop { failed: true });
    }
    st.body.mark_pending(hash);
    confirm_feed.note(ht, hash);
    st.max_ready_height = st.max_ready_height.max(ht);
    Ok(RehydrateOne::Offered {
        bytes: payload.len() as u64,
    })
}
