//! Combined archive-prep + confirm-load stage (single parent-body path).
//!
//! Production confirm load calls [`load_creates_once`] for Class A create decode
//! and pin_new denserels. Always idx→body with `range=None` (no process pin FIFO).
//! Pipeline pins live on the plan (`batch_pin`, `BatchParents`); ancient
//! parents use cold Class A by stamped range.

use rbitcoin_primitives::Fk;
use rbitcoin_store::{
    decode_packed_tx_outs_with_spender_rels_secret, decode_packed_tx_with_spender_rels_secret,
    IdxBodyJob, IdxBodyMode, Store, StoreError, StoreSecret,
};

#[allow(clippy::type_complexity)] // packed (fk, range) / span row is the on-disk shape
/// One create loaded for the combined path.
#[derive(Debug, Clone)]
pub struct CombinedCreate {
    pub fk: Fk,
    pub body_range: (u64, u64),
    pub n_out: u32,
    pub raw: Vec<u8>,
    /// When `mode == Full`, one full decode lives here so callers do not re-decode.
    pub decoded_full: Option<(
        rbitcoin_store::TxRecord,
        Vec<rbitcoin_store::InputRecord>,
        Vec<rbitcoin_store::OutputRecord>,
        Vec<u32>,
    )>,
    /// When `mode == OutsDenserels`, decoded meta/outs/denserels (avoid re-decode on pin).
    pub decoded_outs: Option<(
        rbitcoin_store::TxRecord,
        Vec<rbitcoin_store::OutputRecord>,
        Vec<u32>,
    )>,
}

/// Load creates by fk via loc→body, decode once.
///
/// Ranges are always
/// resolved from `create.loc` (`range=None` on jobs). Callers fill schema-13 zero
/// body `TxRecord.txid` from plan RAM maps when needed — this path never seeds
/// a process pin map and does not fill txid from `txid.body` for that purpose.
///
/// **Shipped entry used by** wire pin and SH.
pub fn load_creates_once(
    store: &Store,
    fks: &[Fk],
    mode: IdxBodyMode,
) -> Result<Vec<CombinedCreate>, rbitcoin_store::StoreError> {
    if fks.is_empty() {
        return Ok(Vec::new());
    }
    let mut jobs: Vec<IdxBodyJob> = fks
        .iter()
        .map(|fk| IdxBodyJob::new(fk.get().unwrap_or(0), None))
        .collect();
    store.idx_body_pipeline(&mut jobs, mode)?;
    let mut seqsigwit_jobs: Vec<IdxBodyJob> = if mode == IdxBodyMode::Full {
        fks.iter()
            .map(|fk| IdxBodyJob::new(fk.get().unwrap_or(0), None))
            .collect()
    } else {
        Vec::new()
    };
    if mode == IdxBodyMode::Full {
        store.idx_seqsigwit_pipeline(&mut seqsigwit_jobs, IdxBodyMode::Full)?;
    }
    let secret: &StoreSecret = store.txs.store_secret();
    let mut out = Vec::with_capacity(jobs.len());
    for (i, (fk, job)) in fks.iter().zip(jobs).enumerate() {
        if !job.ok {
            continue;
        }
        let Some(range) = job.range else {
            continue;
        };
        let mut decoded_full = None;
        let mut decoded_outs = None;
        match mode {
            IdxBodyMode::Full => {
                if let Ok((mut tx, _empty_ins, outs, rels)) =
                    decode_packed_tx_with_spender_rels_secret(&job.body, job.n_out, Some(secret))
                {
                    let Some(ij) = seqsigwit_jobs.get(i) else {
                        return Err(StoreError::Corrupt(
                            "invariant: Full create missing seqsigwit job",
                        ));
                    };
                    if !ij.ok {
                        return Err(StoreError::Corrupt(
                            "invariant: Full create seqsigwit body missing after load",
                        ));
                    }
                    let mut ins = rbitcoin_store::decode_seqsigwit_secret(
                        &ij.body,
                        tx.input_count,
                        Some(secret),
                    )
                    .map_err(|_| {
                        StoreError::Corrupt("invariant: packed create seqsigwit decode failed")
                    })?;
                    store.stamp_input_prevouts(*fk, &mut ins)?;
                    if tx.input_count == 0 {
                        tx.input_count = ins.len() as u32;
                    }
                    decoded_full = Some((tx, ins, outs, rels));
                } else {
                    return Err(StoreError::Corrupt(
                        "invariant: packed create Full decode failed after body load",
                    ));
                }
            }
            IdxBodyMode::Outs => {
                if let Ok((tx, outs, rels)) = decode_packed_tx_outs_with_spender_rels_secret(
                    &job.body,
                    job.n_out,
                    Some(secret),
                ) {
                    // Leave txid zero; caller fills from plan
                    // `external_parents` / batch maps only.
                    decoded_outs = Some((tx, outs, rels));
                } else {
                    return Err(StoreError::Corrupt(
                        "invariant: packed create denserels decode failed after body load",
                    ));
                }
            }
        }
        out.push(CombinedCreate {
            fk: *fk,
            body_range: range,
            n_out: job.n_out,
            raw: job.body,
            decoded_full,
            decoded_outs,
        });
    }
    Ok(out)
}
