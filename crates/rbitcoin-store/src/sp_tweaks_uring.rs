//! Completion-session load for BIP-352 tweak backfill.
//!
//! Reads: idx ranges **before** taking the TLS ring (no nested
//! `with_thread_local`), then S1 `txout` → S2 `seqsigwit` / S3 parent `txout` only
//! for P2TR creates.
//!
//! Writes are **not** this machine: consecutive heights go through
//! [`SpTweaksTable::put_blocks`] (one body pwrite + one idx pwrite).

use crate::error::StoreError;
use crate::io_handle::IoHandle;
use crate::tx_table::{
    decode_packed_tx_outs_with_spender_rels_secret, decode_seqsigwit_secret, InputRecord,
    OutputRecord, TxRecord, TxTable,
};
use crate::uring_session::{self, UringSession};
use crate::U64Map;
use rbitcoin_primitives::Fk;
use std::path::Path;

/// One create after the load wave.
pub struct LoadedTweakTx {
    pub fk: Fk,
    pub rec: TxRecord,
    pub outs: Vec<OutputRecord>,
    pub need_seqsigwit: bool,
    pub inputs: Option<Vec<InputRecord>>,
}

/// Wave result: per-fk bodies + parent outs for P2TR spends not in the wave.
pub struct TweakWave {
    pub txs: Vec<LoadedTweakTx>,
    pub parents: U64Map<([u8; 32], Vec<OutputRecord>)>,
}

#[doc(hidden)]
pub fn is_p2tr(spk: &[u8]) -> bool {
    spk.len() == 34 && spk[0] == 0x51 && spk[1] == 0x20
}

#[doc(hidden)]
pub struct PreadJob {
    pub off: u64,
    pub buf: Vec<u8>,
}

#[doc(hidden)]
pub fn jobs_from_ranges(ranges: &[Option<(u64, u64)>], cap: Option<usize>) -> Vec<PreadJob> {
    ranges
        .iter()
        .map(|r| match r {
            Some((off, len)) if *len > 0 => {
                let n = match cap {
                    Some(c) => (*len as usize).min(c).max(1),
                    None => (*len as usize).max(1),
                };
                PreadJob {
                    off: *off,
                    buf: vec![0u8; n],
                }
            }
            _ => PreadJob {
                off: 0,
                buf: Vec::new(),
            },
        })
        .collect()
}

#[doc(hidden)]
pub fn run_preads_serial(
    fd: IoHandle,
    path: &Path,
    jobs: &mut [PreadJob],
) -> Result<(), StoreError> {
    for job in jobs.iter_mut() {
        if job.buf.is_empty() {
            continue;
        }
        let rc = crate::bulk_io::pread_single(fd, job.off, &mut job.buf);
        uring_session::require_full_cqe(rc, job.buf.len(), path)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn run_preads(
    session: &mut UringSession,
    fd: IoHandle,
    path: &Path,
    kind: u8,
    jobs: &mut [PreadJob],
) -> Result<(), StoreError> {
    if jobs.is_empty() {
        return Ok(());
    }
    session.begin_batch()?;
    let epoch = session.epoch();
    let n = jobs.len();
    let mut next = 0usize;
    let mut inflight = 0usize;
    let mut n_done = 0usize;

    loop {
        while inflight < 128 && session.free_sq() > 0 && next < n {
            let i = next;
            next += 1;
            if jobs[i].buf.is_empty() {
                n_done += 1;
                continue;
            }
            let ud = uring_session::pack_ud(kind, epoch, i as u32);
            // SAFETY: `jobs[i].buf` lives until this pread wave drains.
            unsafe { session.push_pread(fd, jobs[i].off, &mut jobs[i].buf, ud) }?;
            inflight += 1;
        }
        if n_done >= n {
            break;
        }
        if inflight == 0 {
            break;
        }
        session.sync_submission();
        let _ = session.submit();
        let cqes = session.harvest_ready()?;
        if cqes.is_empty() {
            session.submit_and_wait_one()?;
            continue;
        }
        for (ud, res) in cqes {
            let (k, ep, slot) = uring_session::unpack_ud(ud);
            if k != kind || ep != epoch {
                return Err(StoreError::Corrupt("sp_tweaks load leftover CQE"));
            }
            let i = slot as usize;
            if i >= n {
                return Err(StoreError::Corrupt("sp_tweaks load bad slot"));
            }
            uring_session::require_full_cqe(res, jobs[i].buf.len(), path)?;
            inflight = inflight.saturating_sub(1);
            n_done += 1;
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn run_stage(
    fd: IoHandle,
    path: &Path,
    kind: u8,
    jobs: &mut [PreadJob],
) -> Result<(), StoreError> {
    if jobs.iter().all(|j| j.buf.is_empty()) {
        return Ok(());
    }
    match uring_session::with_thread_local(uring_session::DEFAULT_ENTRIES, |session| {
        let mut session = session.drain_guard();
        run_preads(&mut session, fd, path, kind, jobs)
    }) {
        Ok(r) => r,
        Err(_) => run_preads_serial(fd, path, jobs),
    }
}

#[doc(hidden)]
pub fn empty_rec(txid: [u8; 32]) -> TxRecord {
    TxRecord {
        txid,
        version: 1,
        locktime: 0,
        input_start_fk: Fk::NULL,
        input_count: 0,
        output_start_fk: Fk::NULL,
        output_count: 0,
    }
}

/// Load outs for `fks`; seqsigwit + extra parents only for P2TR creates.
///
/// Idx resolve ([`crate::var_table::VarTable::record_range_batch`]) runs
/// **outside** the TLS ring. Body preads are the machine.
pub fn load_tweak_wave(table: &TxTable, fks: &[Fk]) -> Result<TweakWave, StoreError> {
    if fks.is_empty() {
        return Ok(TweakWave {
            txs: Vec::new(),
            parents: U64Map::default(),
        });
    }

    let loc = table.create_loc_range_batch(fks)?;
    let txout_ranges: Vec<Option<(u64, u64)>> = loc.iter().map(|p| p.map(|x| x.txout)).collect();
    let n_outs: Vec<u32> = loc
        .iter()
        .map(|p| p.map(|x| x.n_out).unwrap_or(0))
        .collect();
    let mut txs: Vec<LoadedTweakTx> = Vec::with_capacity(fks.len());
    for &fk in fks {
        let txid = table.body_txid(fk).unwrap_or([0u8; 32]);
        txs.push(LoadedTweakTx {
            fk,
            rec: empty_rec(txid),
            outs: Vec::new(),
            need_seqsigwit: false,
            inputs: None,
        });
    }
    let mut txout_jobs = jobs_from_ranges(&txout_ranges, None);
    run_stage(
        table.body.body_read_fd(),
        table.body.body_file_path(),
        uring_session::KIND_SP_TXOUT,
        &mut txout_jobs,
    )?;

    let mut wave_outs: U64Map<Vec<OutputRecord>> = U64Map::default();
    for (i, job) in txout_jobs.iter().enumerate() {
        if job.buf.is_empty() {
            continue;
        }
        let (mut rec, outs, _) = decode_packed_tx_outs_with_spender_rels_secret(
            &job.buf,
            n_outs[i],
            Some(&table.secret),
        )?;
        rec.txid = txs[i].rec.txid;
        table.overlay_stamped_n_in(txs[i].fk, &mut rec)?;
        txs[i].need_seqsigwit = outs.iter().any(|o| is_p2tr(&o.script));
        wave_outs.insert(txs[i].fk.0, outs.clone());
        txs[i].outs = outs;
        txs[i].rec = rec;
    }

    let p2tr_i: Vec<usize> = txs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.need_seqsigwit)
        .map(|(i, _)| i)
        .collect();
    if !p2tr_i.is_empty() {
        let seqsigwit_fks: Vec<Fk> = p2tr_i.iter().map(|&i| txs[i].fk).collect();
        let seqsigwit_ranges = table.seqsigwit_loc.range_batch(&seqsigwit_fks)?;
        let mut seqsigwit_jobs = jobs_from_ranges(&seqsigwit_ranges, None);
        run_stage(
            table.seqsigwit.body_read_fd(),
            table.seqsigwit.body_file_path(),
            uring_session::KIND_SP_SEQSIGWIT,
            &mut seqsigwit_jobs,
        )?;
        for (j, &ti) in p2tr_i.iter().enumerate() {
            if seqsigwit_jobs[j].buf.is_empty() {
                continue;
            }
            let mut ins = decode_seqsigwit_secret(
                &seqsigwit_jobs[j].buf,
                txs[ti].rec.input_count,
                Some(&table.secret),
            )?;
            table.stamp_seqsigwit_prevouts(txs[ti].fk, &mut ins)?;
            txs[ti].inputs = Some(ins);
        }
    }

    let mut missing: Vec<Fk> = Vec::new();
    for t in &txs {
        let Some(ins) = t.inputs.as_ref() else {
            continue;
        };
        for inp in ins {
            if inp.is_coinbase() {
                continue;
            }
            if wave_outs.contains_key(&inp.create_fk().0) {
                continue;
            }
            missing.push(inp.create_fk());
        }
    }
    missing.sort_by_key(|f| f.0);
    missing.dedup();

    let mut parents: U64Map<([u8; 32], Vec<OutputRecord>)> = U64Map::default();
    if !missing.is_empty() {
        let loc = table.create_loc_range_batch(&missing)?;
        let pr: Vec<Option<(u64, u64)>> = loc.iter().map(|p| p.map(|x| x.txout)).collect();
        let pn: Vec<u32> = loc
            .iter()
            .map(|p| p.map(|x| x.n_out).unwrap_or(0))
            .collect();
        let mut jobs = jobs_from_ranges(&pr, None);
        run_stage(
            table.body.body_read_fd(),
            table.body.body_file_path(),
            uring_session::KIND_SP_PARENT,
            &mut jobs,
        )?;
        for (i, fk) in missing.iter().enumerate() {
            if jobs[i].buf.is_empty() {
                continue;
            }
            let (mut rec, outs, _) = decode_packed_tx_outs_with_spender_rels_secret(
                &jobs[i].buf,
                pn[i],
                Some(&table.secret),
            )?;
            rec.txid = table.body_txid(*fk).unwrap_or(rec.txid);
            parents.insert(fk.0, (rec.txid, outs));
        }
    }

    Ok(TweakWave { txs, parents })
}
