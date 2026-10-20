//! One height-wave and one commit for filter and tweak rows.
//!
//! The confirm batch and the post-IBD window both produce [`IndexRows`].
//! A commit that finds the watermark or `confirmed[h]` moved returns
//! [`IndexCommit::moved`]. The live path treats that as corrupt. The
//! materialize treats it as a re-plan.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::Query;
use bitcoin::bip158::BlockFilter;
use rbitcoin_primitives::{Fk, Height};

use crate::ConfirmError;
use rbitcoin_consensus::script_pool::start_for_each_owned_chunk;
use rbitcoin_consensus::ConsensusError;

/// Per-tx tweak (`None` = ineligible), block order.
pub type HeightTweaks = Vec<Option<[u8; 33]>>;

/// Filter bytes and tweak vecs for a run of heights. Heights travel with the rows.
#[derive(Default)]
pub struct IndexRows {
    pub filters: Vec<(Height, BlockFilter, Fk)>,
    pub tweaks: Vec<(Height, Fk, HeightTweaks)>,
}

pub struct IndexHeightOut {
    pub filter: Option<(Height, BlockFilter, Fk)>,
    pub tweaks: Option<(Height, Fk, HeightTweaks)>,
}

/// Result of [`commit_index_rows`].
pub struct IndexCommit {
    pub put_ns: u64,
    /// Watermark or `confirmed[h]` moved, or the rows leave a hole at `next`.
    pub moved: bool,
}

/// Claim one height per job. `f` writes its [`IndexHeightOut`] into the job.
pub fn finish_wave<T: Sync>(
    jobs: Vec<T>,
    f: fn(&T) -> Result<(), ConsensusError>,
) -> Result<(), ConfirmError> {
    if let Some(wave) = start_for_each_owned_chunk(jobs, f, 1)? {
        wave.finish()?;
    }
    Ok(())
}

pub fn rows_from_slots(slots: &[Arc<Mutex<Option<IndexHeightOut>>>]) -> IndexRows {
    let mut rows = IndexRows::default();
    for slot in slots {
        let Some(done) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            continue;
        };
        if let Some(filter) = done.filter {
            rows.filters.push(filter);
        }
        if let Some(tweaks) = done.tweaks {
            rows.tweaks.push(tweaks);
        }
    }
    rows
}

/// Commit each index from its watermark. Empty rows are a no-op.
pub fn commit_index_rows(query: &Query, rows: IndexRows) -> Result<IndexCommit, ConfirmError> {
    let t0 = Instant::now();
    let moved = commit_filters(query, rows.filters)? || commit_tweaks(query, rows.tweaks)?;
    Ok(IndexCommit {
        put_ns: t0.elapsed().as_nanos() as u64,
        moved,
    })
}

/// `true` when the rows do not start at `next`, or the put returned 0.
#[doc(hidden)]
pub fn commit_filters(
    query: &Query,
    filters: Vec<(Height, BlockFilter, Fk)>,
) -> Result<bool, ConfirmError> {
    let Some(next) = query.filter_index_next() else {
        return Ok(false);
    };
    let Some(start) = filters.iter().position(|(h, _, _)| h.0 >= next) else {
        return Ok(false);
    };
    if filters[start..]
        .iter()
        .enumerate()
        .any(|(i, (h, _, _))| h.0 != next.saturating_add(i as u32))
    {
        return Ok(true);
    }
    let items: Vec<(BlockFilter, Fk)> = filters
        .into_iter()
        .skip(start)
        .map(|(_, filter, fk)| (filter, fk))
        .collect();
    Ok(query.commit_window_filters(next, &items)? == 0)
}

#[doc(hidden)]
pub fn commit_tweaks(
    query: &Query,
    tweaks: Vec<(Height, Fk, HeightTweaks)>,
) -> Result<bool, ConfirmError> {
    let Some(next) = query.tweak_index_next() else {
        return Ok(false);
    };
    let Some(start) = tweaks.iter().position(|(h, _, _)| h.0 >= next) else {
        return Ok(false);
    };
    let slice = &tweaks[start..];
    if slice
        .iter()
        .enumerate()
        .any(|(i, (h, _, _))| h.0 != next.saturating_add(i as u32))
    {
        return Ok(true);
    }
    Ok(query.commit_window_tweaks(slice)? == 0)
}
