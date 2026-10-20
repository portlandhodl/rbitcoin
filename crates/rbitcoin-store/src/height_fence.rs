//! Resident create-height fence: O(blocks) image, O(log tip) point query.
//!
//! Class A fks are assigned sequentially, but a reorg leaves **holes**: orphaned
//! rows sit numerically between two still-confirmed runs. `fk ≤ last_tip_fk` is
//! therefore false as a connectedness test. This fence answers “which confirmed
//! height’s `[first_fk, first_fk+count)` contains this fk, if any.”
//!
//! Built only from `confirmed[]` + `header_txs_*` (already L2). No `tx_height`
//! file and no disk binary search.

use crate::chain::{ConfirmedTable, HeaderTxsTable};
use crate::error::StoreError;
use rbitcoin_primitives::{Fk, Height};
use std::sync::Arc;

/// One confirmed height’s contiguous Class A run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FenceRun {
    /// 1-based inclusive create_fk of the first tx in the block.
    pub first_fk: u64,
    pub count: u32,
    pub height: u32,
}

impl FenceRun {
    #[inline]
    pub fn contains(self, id: u64) -> bool {
        id >= self.first_fk && id < self.first_fk.saturating_add(u64::from(self.count))
    }
}

/// Height-indexed, first_fk-sorted confirmed runs.
///
/// `runs` is `Arc` so leftover TipOnly snapshots are O(1). `extend` / `pop`
/// COW via [`Arc::make_mut`].
#[derive(Clone, Debug, Default)]
pub struct HeightFence {
    /// Sorted by `first_fk` (Class A assign is increasing; reorgs append higher).
    pub runs: Arc<Vec<FenceRun>>,
}

impl HeightFence {
    pub fn empty() -> Self {
        Self {
            runs: Arc::new(Vec::new()),
        }
    }

    /// Sort by `first_fk` and drop empty counts.
    pub fn from_runs(mut runs: Vec<FenceRun>) -> Self {
        runs.retain(|r| r.count > 0 && r.first_fk > 0);
        runs.sort_unstable_by_key(|r| r.first_fk);
        Self {
            runs: Arc::new(runs),
        }
    }

    /// True when `other` shares this fence's run allocation (cheap snapshot).
    pub fn shares_runs(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.runs, &other.runs)
    }

    /// Rebuild from confirmed tip + per-header Class A ranges.
    pub fn from_confirmed(
        confirmed: &ConfirmedTable,
        header_txs: &HeaderTxsTable,
    ) -> Result<Self, StoreError> {
        let Some(tip) = confirmed.tip_height() else {
            return Ok(Self::empty());
        };
        let mut runs = Vec::with_capacity(tip.0 as usize + 1);
        for h in 0..=tip.0 {
            let Some(hfk) = confirmed.get(Height(h))? else {
                continue;
            };
            let Some((first, n)) = header_txs.get_range(hfk)? else {
                continue;
            };
            let Some(id) = first.get() else {
                continue;
            };
            if n == 0 {
                continue;
            }
            runs.push(FenceRun {
                first_fk: id,
                count: n,
                height: h,
            });
        }
        Ok(Self::from_runs(runs))
    }

    pub fn len(&self) -> usize {
        self.runs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Confirm tip+1: append this height’s run (first_fk must be new).
    pub fn extend(&mut self, height: u32, first_fk: Fk, count: u32) {
        let Some(id) = first_fk.get() else {
            return;
        };
        if count == 0 {
            return;
        }
        let runs = Arc::make_mut(&mut self.runs);
        runs.push(FenceRun {
            first_fk: id,
            count,
            height,
        });
        if runs.len() >= 2 {
            let n = runs.len();
            if runs[n - 1].first_fk < runs[n - 2].first_fk {
                runs.sort_unstable_by_key(|r| r.first_fk);
            }
        }
    }

    /// Disconnect tip: drop the run for `height` (must be the tip height).
    pub fn pop_height(&mut self, height: u32) {
        if let Some(i) = self.runs.iter().position(|r| r.height == height) {
            Arc::make_mut(&mut self.runs).remove(i);
        }
    }

    /// Connected create height, or `None` if `fk` sits in a hole / unconfirmed.
    #[inline]
    pub fn height_of(&self, fk: Fk) -> Option<u32> {
        let id = fk.get()?;
        if self.runs.is_empty() {
            return None;
        }
        let i = self.runs.partition_point(|r| r.first_fk <= id);
        if i == 0 {
            return None;
        }
        let r = self.runs[i - 1];
        if r.contains(id) {
            Some(r.height)
        } else {
            None
        }
    }

    pub fn get_batch(&self, fks: &[Fk]) -> Vec<Option<u32>> {
        fks.iter().map(|&fk| self.height_of(fk)).collect()
    }

    /// Half-open `[lo, hi)` create-fk spans not in any run, covering `1..=last_fk`.
    ///
    /// Adjacent runs emit nothing between them. Empty fence → `[1, last_fk+1)`
    /// when `last_fk > 0`. Used by Class C open repair (complement of the fence).
    pub fn unconnected_ranges(&self, last_fk: u64) -> Vec<(u64, u64)> {
        if last_fk == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut prev_end = 1u64;
        let cap = last_fk.saturating_add(1);
        for r in self.runs.iter() {
            if r.first_fk >= cap {
                break;
            }
            if r.first_fk > prev_end {
                out.push((prev_end, r.first_fk.min(cap)));
            }
            let run_end = r.first_fk.saturating_add(u64::from(r.count));
            if run_end > prev_end {
                prev_end = run_end;
            }
        }
        if prev_end < cap {
            out.push((prev_end, cap));
        }
        out
    }

    /// True when every create fk in inclusive `lo..=hi` sits in a fence run.
    ///
    /// In-flight prune: leftover TipOnly accepts a span only if this is true
    /// (max height on the fence is not enough — holes / past last run end).
    pub fn covers_fk_span(&self, lo: u64, hi: u64) -> bool {
        if lo == 0 || hi < lo || self.runs.is_empty() {
            return false;
        }
        let mut need = lo;
        while need <= hi {
            let i = self.runs.partition_point(|r| r.first_fk <= need);
            if i == 0 {
                return false;
            }
            let r = self.runs[i - 1];
            if !r.contains(need) {
                return false;
            }
            need = r.first_fk.saturating_add(u64::from(r.count));
        }
        true
    }

    /// Highest confirmed height present on any run (`None` if empty).
    ///
    /// Distinct from `confirmed[]` length (`tip_height`): `set_many` can
    /// publish tip before [`Self::extend`]. In-flight prune uses
    /// [`Self::drain_and_fence_hi`], not this alone.
    pub fn max_height(&self) -> Option<u32> {
        self.runs.iter().map(|r| r.height).max()
    }

    /// Height both head drain and this fence have passed (`None` keeps inflight).
    ///
    /// `drain_fk == 0` or a drain fk not on a run (hole, or drain ahead of
    /// the fence) is `None`. A hit is that run's height.
    pub fn drain_and_fence_hi(&self, drain_fk: u64) -> Option<u32> {
        if drain_fk == 0 {
            return None;
        }
        let drain_h = self.height_of(Fk(drain_fk))?;
        Some(drain_h)
    }

    /// Highest create_fk in any connected run (`0` if empty).
    ///
    /// Not `last_tip_fk` as a connectedness test — holes after reorg sit
    /// numerically between runs. Use [`Self::height_of`] per fk; this is
    /// only a prune HWM (min with `tx.head` occupied).
    pub fn max_connected_fk(&self) -> u64 {
        self.runs
            .iter()
            .map(|r| {
                r.first_fk
                    .saturating_add(u64::from(r.count))
                    .saturating_sub(1)
            })
            .max()
            .unwrap_or(0)
    }
}

/// Last 11 confirmed header timestamps ending at the fence tip (BIP113 window).
///
/// Sequential extend is O(1). Non-sequential / pop → caller rebuilds from
/// confirmed headers (reorgs, open). Historical MTP does not use this.
#[derive(Clone, Copy, Debug, Default)]
pub struct MtpRing {
    pub times: [u32; 11],
    /// Valid prefix `0..=11`.
    pub len: u8,
    /// Height of `times[len-1]` when `len > 0`.
    pub tip: u32,
}

impl MtpRing {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Append if `height` is genesis on an empty ring, or `tip+1`.
    pub fn try_push(&mut self, height: u32, time: u32) -> bool {
        if self.len == 0 {
            if height != 0 {
                return false;
            }
            self.times[0] = time;
            self.len = 1;
            self.tip = 0;
            return true;
        }
        if height != self.tip.saturating_add(1) {
            return false;
        }
        if (self.len as usize) < 11 {
            self.times[self.len as usize] = time;
            self.len += 1;
        } else {
            self.times.copy_within(1..11, 0);
            self.times[10] = time;
        }
        self.tip = height;
        true
    }

    pub fn set_window(&mut self, tip: u32, times: &[u32]) {
        let n = times.len().min(11);
        if n == 0 {
            *self = Self::empty();
            return;
        }
        self.times[..n].copy_from_slice(&times[..n]);
        self.len = n as u8;
        self.tip = tip;
    }

    pub fn clear(&mut self) {
        *self = Self::empty();
    }

    /// Window for BIP113 at `height` when this ring ends there.
    pub fn window_at(&self, height: u32) -> Option<(u8, [u32; 11])> {
        if self.len == 0 || height != self.tip {
            return None;
        }
        let expect = (height.saturating_add(1)).min(11) as u8;
        if self.len != expect {
            return None;
        }
        Some((self.len, self.times))
    }
}
