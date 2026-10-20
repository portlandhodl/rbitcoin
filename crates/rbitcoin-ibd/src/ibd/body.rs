//! Process-local Class A body presence cache for IBD assign/status.

use bitcoin::BlockHash;
use rbitcoin_net::chain::ChainHub;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// O(1) occupancy of each [`BodyPresence`] set (for `ibd: sizes` logs).
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct BodyPresenceSizes {
    pub known: usize,
    pub pending: usize,
    pub missing: usize,
    pub rejected: usize,
}

/// Process-local Class A body presence cache.
///
/// Assign/status used to call `is_archived` (store) for every ordered hash every
/// loop — that contended with the archive writer and delayed getdata top-up
/// (spiky receive). Probe each hash at most once until archive marks it ready.
///
/// `pending` covers the wire→confirm gap: we clear getdata inflight on body
/// receipt, but must not re-request until confirm advances tip (or re-get after
/// stale expire).
///
/// `rejected` covers Class C permanent failures: never re-offer or re-download.
pub struct BodyPresence {
    pub known: HashSet<BlockHash>,
    /// Received on the wire / in body queue, not yet tip-confirmed.
    /// Membership **is** the pending set; value is when marked (stale expire).
    pub pending_since: HashMap<BlockHash, Instant>,
    /// Store-probed and not archived yet (safe to request getdata).
    pub missing: HashSet<BlockHash>,
    /// Confirm rejected this hash; do not re-offer or re-download.
    pub rejected: HashSet<BlockHash>,
}

impl BodyPresence {
    pub fn new() -> Self {
        Self {
            known: HashSet::new(),
            pending_since: HashMap::new(),
            missing: HashSet::new(),
            rejected: HashSet::new(),
        }
    }

    #[inline]
    #[doc(hidden)]
    pub fn is_pending_hash(&self, h: &BlockHash) -> bool {
        self.pending_since.contains_key(h)
    }

    pub fn mark_pending(&mut self, h: BlockHash) {
        if self.rejected.contains(&h) {
            return;
        }
        self.missing.remove(&h);
        self.pending_since.entry(h).or_insert_with(Instant::now);
    }

    pub fn mark_archived(&mut self, h: BlockHash) {
        if self.rejected.contains(&h) {
            return;
        }
        self.missing.remove(&h);
        self.pending_since.remove(&h);
        self.known.insert(h);
    }

    pub fn mark_missing(&mut self, h: BlockHash) {
        if self.rejected.contains(&h) {
            return;
        }
        self.known.remove(&h);
        self.pending_since.remove(&h);
        self.missing.insert(h);
    }

    /// Drop optimistic Class A cache only — next [`Self::ready`] re-probes the store.
    pub fn demote_known(&mut self, h: BlockHash) {
        self.known.remove(&h);
    }

    /// Permanent confirm failure: never re-offer this hash to the confirm engine
    /// and never re-getdata it.
    pub fn mark_rejected(&mut self, h: BlockHash) {
        self.known.remove(&h);
        self.pending_since.remove(&h);
        self.missing.remove(&h);
        self.rejected.insert(h);
    }

    /// Pending hashes older than `max_age` matching `pred` → mark missing (re-get).
    pub fn expire_stale_pending_if(
        &mut self,
        max_age: Duration,
        mut pred: impl FnMut(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let now = Instant::now();
        let stale: Vec<BlockHash> = self
            .pending_since
            .iter()
            .filter(|(h, t)| pred(h) && now.duration_since(**t) >= max_age)
            .map(|(h, _)| *h)
            .collect();
        for h in &stale {
            self.mark_missing(*h);
        }
        stale
    }

    pub fn is_rejected(&self, h: &BlockHash) -> bool {
        self.rejected.contains(h)
    }

    /// True if we already know Class A is present (no store probe).
    pub fn is_known_archived(&self, h: &BlockHash) -> bool {
        self.known.contains(h)
    }

    pub fn known_len(&self) -> usize {
        self.known.len()
    }

    pub fn size_snapshot(&self) -> BodyPresenceSizes {
        BodyPresenceSizes {
            known: self.known.len(),
            pending: self.pending_since.len(),
            missing: self.missing.len(),
            rejected: self.rejected.len(),
        }
    }

    pub fn is_pending(&self, h: &BlockHash) -> bool {
        self.is_pending_hash(h)
    }

    pub fn is_missing(&self, h: &BlockHash) -> bool {
        self.missing.contains(h)
    }

    /// Hot path: local sets first. Do **not** call `has_block` before checking
    /// `missing` — assign walks tens of thousands of known-missing far hashes.
    ///
    /// Does **not** treat `known` (Class A cache) as skip — that set is densify
    /// bookkeeping; tip-hole re-get after resume still needs peer wire into BQ.
    pub fn skip_download(&mut self, hub: &ChainHub, h: &BlockHash) -> bool {
        if self.rejected.contains(h) || self.is_pending_hash(h) {
            return true;
        }
        if self.is_missing(h) {
            return false;
        }
        if hub.has_block(h) {
            self.known.insert(*h);
            return true;
        }
        false
    }

    /// Drop cache entries that are no longer on the live work path.
    /// Rejected entries are never pruned.
    pub fn hygiene_retain(&mut self, mut keep: impl FnMut(&BlockHash) -> bool) {
        self.known.retain(|h| keep(h));
        self.missing.retain(|h| keep(h));
        self.pending_since.retain(|h, _| keep(h));
    }
}
