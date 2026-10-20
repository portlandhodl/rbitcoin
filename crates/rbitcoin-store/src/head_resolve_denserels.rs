//! Plan Shape A head resolve: **txids in → denserels out** (or fk+range short-circuit).
//!
//! Probe+identity (uring when available):
//! 1. **Open** — every unsealed OA (insert tail + in-flight seal), one batch
//! 2. **Sealed** — newest segment first. Fuse + one MPHF (or sealed OA), then
//!    identity, then retire fence-connected keys before the next segment.
//!
//! Each identity: at most two page-grouped `txid.body` shots (first four cands,
//! then the rest if still unfinished) → newest-first walk (`body==want`,
//! fence-connected if a fence is on). Unconnected identity does **not** skip
//! later segments or shot B. TipOnly strips unconnected winners at the end.
//! **One** `create.loc` batch runs **after** the sealed walk, on FdOnly /
//! standalone bulk — not on the held probe ring.
//!
//! [`resolve_fk_and_range_batch`] is the **stamp short-circuit**: stops after
//! loc, returns `(fk, body_range)` so prep denserels-loads by offset.
//!
//! **IO shape:** probe may use one TLS [`UringSession`]; sidefile ID is
//! page-grouped bulk pread (one read per OS page of `txid.body`). Nested TLS
//! uring remains a hard error.
//!
//! Backend: global `RBITCOIN_IO` (`uring` \| `pool` \| `pread`).

use crate::error::StoreError;
use crate::height_fence::HeightFence;
use crate::io_backend::{self, ReadIoBackend};
use crate::segmented_head::HeadProbeWave;
use crate::tx_table::TxTable;
use crate::txid_body::TxidBody;
use crate::uring_session::{self, UringSession};
use rbitcoin_primitives::Fk;
use std::time::Instant;

/// Stamp short-circuit: **txids → (fk, body_range)** via one TLS uring machine.
///
/// Probe (head pages) + identity on the held ring, then **one** loc batch after
/// TLS drops. Prep denserels loads by offset (skip re-idx).
pub fn resolve_fk_and_range_batch(
    table: &TxTable,
    txids: &[[u8; 32]],
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    resolve_fk_and_range_batch_opts(table, txids, None, false)
}

/// Like [`resolve_fk_and_range_batch`], but prefer a **connected** Class A row
/// (height fence hit). An unconnected hit does **not** skip older segments.
///
/// `tip_only`: result is connected-or-None (confirm). Otherwise connected else
/// newest unconnected (RPC).
pub fn resolve_fk_and_range_batch_with_tip(
    table: &TxTable,
    heights: &HeightFence,
    txids: &[[u8; 32]],
    tip_only: bool,
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    resolve_fk_and_range_batch_opts(table, txids, Some(heights), tip_only)
}

#[doc(hidden)]
pub fn note_first_leftover_miss(
    tip_only: bool,
    picked: &[Option<Fk>],
    n_cands: &[usize],
    had_id: &[bool],
) {
    crate::head_resolve_stats::clear_leftover_miss();
    if !tip_only {
        return;
    }
    for i in 0..picked.len() {
        if picked[i].is_some() {
            continue;
        }
        let on = crate::head_resolve_pick::classify_leftover_miss(n_cands[i], had_id[i]);
        crate::head_resolve_stats::note_leftover_miss(on, n_cands[i] as u64);
        return;
    }
}

#[doc(hidden)]
pub fn hex_bytes(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len().saturating_mul(2));
    for &x in b {
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 0x0f) as usize] as char);
    }
    s
}

#[doc(hidden)]
pub fn format_leftover_probe_diag(d: &crate::head_resolve_stats::LeftoverProbeDiag) -> String {
    let age = d
        .sealed_age
        .map(|a| a.to_string())
        .unwrap_or_else(|| "-".into());
    let mut s = format!(
        "leftover probe diag txid={} mix={} page_base={} bits={} file_id={} first_fk={} age={} \
         hit_empty={} depth_end={} empty_local={} occ={} hop2eq={} ncand={}",
        hex_bytes(&d.txid),
        hex_bytes(&d.mixed_prefix),
        d.page_base,
        d.bits,
        d.file_id,
        d.first_fk,
        age,
        u8::from(d.hit_empty),
        d.depth_end,
        d.empty_local,
        d.page_occupied,
        u8::from(d.hop_equal_second),
        d.cands.len(),
    );
    for c in &d.cands {
        s.push_str(&format!(
            " | d={} loc={} rel={} abs={} body={} match={}",
            c.depth,
            c.local,
            c.rel,
            c.abs_fk,
            hex_bytes(&c.body_prefix),
            u8::from(c.body_match),
        ));
    }
    s
}

/// Hop + cand dump for a leftover miss only (not lookup / BQ-ahead TipOnly).
pub fn diagnose_and_note_leftover_probe(table: &TxTable, txid: &[u8; 32]) {
    match diagnose_txid_probe(table, txid) {
        Ok(d) => {
            rbitcoin_log::warn!("store: {}", format_leftover_probe_diag(&d));
            crate::head_resolve_stats::note_leftover_probe_diag(d);
        }
        Err(e) => {
            rbitcoin_log::warn!("store: leftover probe diag failed: {e}");
        }
    }
}

#[doc(hidden)]
pub fn diagnose_txid_probe(
    table: &TxTable,
    txid: &[u8; 32],
) -> Result<crate::head_resolve_stats::LeftoverProbeDiag, StoreError> {
    use crate::address_head::{h1_in_page, h2_in_page, page_base_for_txid, PAGE_SLOTS};
    use crate::head_resolve_stats::{LeftoverProbeCand, LeftoverProbeDiag};

    let mixed = table.secret.mix_txid(txid);
    let bits = table.head.bits();
    let page_base = page_base_for_txid(&mixed, bits);
    let first_fks = table.head.first_fks_snapshot();
    let (file_id, first_fk, hop) = table.head.leftover_open_hop(&mixed)?;
    let abs_cands = table.head.probe_candidates(&mixed)?;
    let side = table.txid_sidefile();
    let mask = if bits <= crate::address_head::PAGE_SLOT_BITS {
        (1u64 << bits) - 1
    } else {
        PAGE_SLOTS - 1
    };
    let h1 = h1_in_page(&mixed, bits);
    let h2 = h2_in_page(&mixed, bits);
    let mut rel_meta = std::collections::HashMap::new();
    for &(d, rel) in hop.scan.cands.iter() {
        let local = h1.wrapping_add(u64::from(d).wrapping_mul(h2)) & mask;
        rel_meta.insert(rel, (d, local));
    }
    let mut cands = Vec::with_capacity(abs_cands.len());
    for fk in abs_cands {
        let Some(id) = fk.get() else {
            continue;
        };
        let rel = if id >= first_fk { id - first_fk + 1 } else { 0 };
        let (depth, local) = rel_meta.get(&rel).copied().unwrap_or((u32::MAX, 0));
        let body = side.get_read_at(fk).unwrap_or_default();
        let mut body_prefix = [0u8; 8];
        body_prefix.copy_from_slice(&body[..8]);
        cands.push(LeftoverProbeCand {
            depth,
            local,
            rel,
            abs_fk: id,
            body_prefix,
            body_match: body == *txid,
        });
    }
    let sealed_age = crate::head_resolve_stats::sealed_age_for_fk(&first_fks, first_fk);
    let mut mixed_prefix = [0u8; 8];
    mixed_prefix.copy_from_slice(&mixed[..8]);
    Ok(LeftoverProbeDiag {
        txid: *txid,
        mixed_prefix,
        page_base,
        bits,
        file_id,
        first_fk,
        sealed_age,
        hit_empty: hop.scan.hit_empty,
        depth_end: hop.scan.depth_end,
        empty_local: hop.scan.empty_local,
        page_occupied: hop.occupied,
        hop_equal_second: hop.hop_equal_second,
        cands,
    })
}

#[doc(hidden)]
pub fn resolve_fk_and_range_batch_opts(
    table: &TxTable,
    txids: &[[u8; 32]],
    heights: Option<&HeightFence>,
    tip_only: bool,
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    if txids.is_empty() {
        crate::head_resolve_stats::clear_leftover_miss();
        return Ok(Vec::new());
    }
    match io_backend::read_io_backend() {
        ReadIoBackend::Uring => map_uring_resolve(
            resolve_fk_and_range_uring(table, txids, heights, tip_only),
            || resolve_fk_and_range_pread(table, txids, heights, tip_only),
        ),
        ReadIoBackend::Pread => resolve_fk_and_range_pread(table, txids, heights, tip_only),
    }
}

/// Fallback to pread only when the ring cannot be opened. Harvest invariants
/// (`Corrupt` / `Io` from a live machine) must not be swallowed.
#[doc(hidden)]
pub fn map_uring_resolve<T>(
    uring: Result<T, StoreError>,
    pread: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    match uring {
        Ok(v) => Ok(v),
        Err(e) if is_uring_unavailable(&e) => pread(),
        Err(e) => Err(e),
    }
}

#[doc(hidden)]
pub fn is_uring_unavailable(err: &StoreError) -> bool {
    match err {
        StoreError::Unavailable | StoreError::Corrupt("io_uring is Linux-only") => true,
        StoreError::Io { path, .. } if path.as_os_str() == "io_uring" => true,
        _ => false,
    }
}

#[doc(hidden)]
pub fn add_wave_cands(n_cands: &mut [usize], cands: &[Vec<Fk>]) -> u64 {
    let mut n = 0u64;
    for (i, c) in cands.iter().enumerate() {
        n_cands[i] = n_cands[i].saturating_add(c.len());
        n = n.saturating_add(c.len() as u64);
    }
    n
}

#[doc(hidden)]
pub struct IdentityHits {
    pub picked: Vec<Option<Fk>>,
}

#[doc(hidden)]
pub fn resolve_identity_core(
    table: &TxTable,
    txids: &[[u8; 32]],
    heights: Option<&HeightFence>,
    tip_only: bool,
    session: Option<&mut UringSession>,
) -> Result<IdentityHits, StoreError> {
    crate::head_resolve_stats::add_keys(txids.len() as u64);

    let mixed: Vec<[u8; 32]> = txids.iter().map(|t| table.secret.mix_txid(t)).collect();
    let side = table.txid_sidefile();
    let first_fks = table.head.first_fks_snapshot();
    let mut local_age = [0u64; crate::head_resolve_stats::AGE_CAP];
    let mut picked: Vec<Option<Fk>> = vec![None; txids.len()];
    let mut connected = vec![false; txids.len()];
    let mut n_cands = vec![0usize; txids.len()];
    let mut had_id = vec![false; txids.len()];
    let mut body_lookups = 0u64;
    let mut miss_peeks = 0u64;
    let mut id_ns = 0u64;
    let mut probe_ns = 0u64;
    let mut cands_total = 0u64;
    let mut ctx = crate::IoCtx::from_opt(session);

    let t_probe = Instant::now();
    let open =
        table
            .head
            .probe_candidates_batch_wave(&mixed, HeadProbeWave::Open, None, &mut ctx)?;
    probe_ns = probe_ns.saturating_add(t_probe.elapsed().as_nanos() as u64);
    cands_total = cands_total.saturating_add(add_wave_cands(&mut n_cands, &open));
    id_idx_wave(
        txids,
        &open,
        side,
        &mut picked,
        &mut connected,
        heights,
        &mut body_lookups,
        &mut miss_peeks,
        &mut id_ns,
        &first_fks,
        &mut local_age,
        &mut ctx,
        &mut had_id,
    )?;

    if any_unfinished(&picked, &connected, heights) {
        let n_segs = table.head.segment_count();
        let mut active = unfinished_mask(&picked, &connected, heights);
        for si in (0..n_segs).rev() {
            if !active.iter().any(|on| *on) {
                break;
            }
            let t_probe = Instant::now();
            let cands = table
                .head
                .probe_sealed_segment(&mixed, si, Some(&active), &mut ctx)?;
            probe_ns = probe_ns.saturating_add(t_probe.elapsed().as_nanos() as u64);
            if cands.iter().all(|c| c.is_empty()) {
                continue;
            }
            cands_total = cands_total.saturating_add(add_wave_cands(&mut n_cands, &cands));
            id_idx_wave(
                txids,
                &cands,
                side,
                &mut picked,
                &mut connected,
                heights,
                &mut body_lookups,
                &mut miss_peeks,
                &mut id_ns,
                &first_fks,
                &mut local_age,
                &mut ctx,
                &mut had_id,
            )?;
            for (i, on) in active.iter_mut().enumerate() {
                *on = !key_finished(i, &picked, &connected, heights);
            }
        }
    }

    if tip_only && heights.is_some() {
        for (i, w) in picked.iter_mut().enumerate() {
            if !connected[i] {
                *w = None;
            }
        }
    }
    note_first_leftover_miss(tip_only, &picked, &n_cands, &had_id);

    crate::head_resolve_stats::add_probe(probe_ns);
    crate::head_resolve_stats::add_cands(cands_total);
    crate::head_resolve_stats::add_body(id_ns);
    crate::head_resolve_stats::add_body_lookups(body_lookups);
    crate::head_resolve_stats::add_miss_peeks(miss_peeks);
    crate::head_resolve_stats::add_hit_ages(&local_age);

    Ok(IdentityHits { picked })
}

#[doc(hidden)]
pub fn resolve_fk_and_range_pread(
    table: &TxTable,
    txids: &[[u8; 32]],
    heights: Option<&HeightFence>,
    tip_only: bool,
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    let ident = resolve_identity_core(table, txids, heights, tip_only, None)?;
    attach_loc_to_identity(table, txids, ident)
}

#[doc(hidden)]
pub fn attach_loc_to_identity(
    table: &TxTable,
    txids: &[[u8; 32]],
    ident: IdentityHits,
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    let IdentityHits { picked } = ident;
    let t_idx = Instant::now();
    let mut need: Vec<Fk> = Vec::new();
    let mut slots: Vec<usize> = Vec::new();
    for (i, fk) in picked.iter().enumerate() {
        if let Some(fk) = *fk {
            need.push(fk);
            slots.push(i);
        }
    }
    let ranges = table.create_loc.range_batch(&need)?;
    crate::head_resolve_stats::add_idx(t_idx.elapsed().as_nanos() as u64);
    let mut winner: Vec<Option<(Fk, crate::create_loc::CreateLocPair)>> = vec![None; picked.len()];
    for (slot, (fk, range)) in slots.into_iter().zip(need.into_iter().zip(ranges)) {
        match range {
            Some(pair) => winner[slot] = Some((fk, pair)),
            None => {
                crate::uring_session::note_uring_invariant(
                    crate::uring_session::UringInvariant::IdxRangeMissing,
                );
                return Err(StoreError::Corrupt(
                    "invariant: loc range missing after identity",
                ));
            }
        }
    }
    Ok(txids
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, winner[i]))
        .collect())
}

/// Connected if a height fence is set, else any winner.
#[doc(hidden)]
pub fn key_finished(
    ki: usize,
    picked: &[Option<Fk>],
    connected: &[bool],
    heights: Option<&HeightFence>,
) -> bool {
    if heights.is_some() {
        connected[ki]
    } else {
        picked[ki].is_some()
    }
}

#[doc(hidden)]
pub fn any_unfinished(
    picked: &[Option<Fk>],
    connected: &[bool],
    heights: Option<&HeightFence>,
) -> bool {
    (0..picked.len()).any(|i| !key_finished(i, picked, connected, heights))
}

#[doc(hidden)]
pub fn unfinished_mask(
    picked: &[Option<Fk>],
    connected: &[bool],
    heights: Option<&HeightFence>,
) -> Vec<bool> {
    (0..picked.len())
        .map(|i| !key_finished(i, picked, connected, heights))
        .collect()
}

#[allow(clippy::too_many_arguments)] // IO/session args stay unbundled
/// Sidefile ID (at most two page-grouped shots) then BIP30 match.
///
/// Shot A is the first four cands of unfinished keys; shot B is the rest only
/// when the key is still unfinished. A fence-connected win skips shot B; an
/// unconnected body match does not. Loc fill is **one** batch after the
/// sealed walk (`attach_loc_to_identity`), never on this held probe ring.
///
/// When `ctx` is held, identity preads ride that **already-held** plan ring.
/// When none, libc pread for ID.
#[doc(hidden)]
pub fn id_idx_wave(
    txids: &[[u8; 32]],
    cands_by_key: &[Vec<Fk>],
    side: &TxidBody,
    picked: &mut [Option<Fk>],
    connected: &mut [bool],
    heights: Option<&HeightFence>,
    body_lookups: &mut u64,
    miss_peeks: &mut u64,
    id_ns: &mut u64,
    first_fks: &[u64],
    local_age: &mut [u64; crate::head_resolve_stats::AGE_CAP],
    ctx: &mut crate::IoCtx<'_>,
    had_id: &mut [bool],
) -> Result<(), StoreError> {
    use crate::head_resolve_pick::{next_id_shot, walk_id_prefix, ID_FILL_CHUNK};
    use crate::int_map::{U64Map, U64Set};

    let n = cands_by_key.len();
    let mut filled = vec![0usize; n];
    let mut skip = vec![false; n];
    let mut started = vec![false; n];
    let mut key_miss = vec![0u64; n];
    for ki in 0..n {
        let done = key_finished(ki, picked, connected, heights);
        skip[ki] = done;
        started[ki] = !done;
    }
    let mut id_map: U64Map<[u8; 32]> = U64Map::default();

    for take in [ID_FILL_CHUNK, usize::MAX] {
        let shot = next_id_shot(cands_by_key, &filled, &skip, take);
        let mut need: Vec<Fk> = Vec::new();
        {
            let mut seen = U64Set::default();
            for fk in shot {
                let Some(id) = fk.get() else {
                    continue;
                };
                if id_map.contains_key(&id) {
                    continue;
                }
                if seen.insert(id) {
                    need.push(fk);
                }
            }
        }
        if !need.is_empty() {
            let t_id = Instant::now();
            let (more, _pages) = side.get_many_page_grouped_ctx(&need, ctx)?;
            *id_ns = id_ns.saturating_add(t_id.elapsed().as_nanos() as u64);
            *body_lookups = body_lookups.saturating_add(more.len() as u64);
            id_map.extend(more);
        }
        for ki in 0..n {
            if skip[ki] {
                continue;
            }
            filled[ki] = filled[ki].saturating_add(take).min(cands_by_key[ki].len());
        }
        for ki in 0..n {
            if skip[ki] {
                continue;
            }
            let cands = &cands_by_key[ki];
            let nfill = filled[ki];
            // A prior segment may already hold an unconnected body. Do not
            // replace it unless this prefix connects, or this is the last
            // cand and nothing is picked yet.
            let fallback = heights.is_some() && nfill >= cands.len() && picked[ki].is_none();
            let walked = walk_id_prefix(cands, nfill, &txids[ki], &id_map, heights, fallback);
            if walked.had_body {
                had_id[ki] = true;
            }
            key_miss[ki] = walked.miss;
            if let Some((fk, rank)) = walked.winner {
                crate::head_resolve_stats::add_hit_rank(rank);
                note_identity_pick(ki, fk, picked, connected, heights, first_fks, local_age);
                skip[ki] = true;
            }
        }
    }

    for ki in 0..n {
        if !started[ki] {
            continue;
        }
        *miss_peeks = miss_peeks.saturating_add(key_miss[ki]);
    }
    Ok(())
}

#[doc(hidden)]
pub fn note_identity_pick(
    ki: usize,
    fk: Fk,
    picked: &mut [Option<Fk>],
    connected: &mut [bool],
    heights: Option<&HeightFence>,
    first_fks: &[u64],
    local_age: &mut [u64; crate::head_resolve_stats::AGE_CAP],
) {
    picked[ki] = Some(fk);
    if heights.is_some_and(|h| h.height_of(fk).is_some()) {
        connected[ki] = true;
    }
    crate::head_resolve_stats::note_local_hit_age(local_age, first_fks, fk.0);
}

#[doc(hidden)]
pub fn resolve_fk_and_range_uring(
    table: &TxTable,
    txids: &[[u8; 32]],
    heights: Option<&HeightFence>,
    tip_only: bool,
) -> Result<Vec<crate::tx_table::TxidFkRange>, StoreError> {
    let ident = uring_session::with_thread_local(uring_session::DEFAULT_ENTRIES, |session| {
        resolve_identity_core(table, txids, heights, tip_only, Some(session))
    })??;
    attach_loc_to_identity(table, txids, ident)
}
