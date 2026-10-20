//! Transaction orphanage for txs missing in-mempool / chain parents.
//!
//! Weight budget: 404_000 WU reserved per peer × 25-peer budget → ~10.1M WU
//! unique orphans; unique-count cap 3000. Per-tx max 404_000 WU.
//!
//! Eviction: FIFO by insert order when over weight or count.

use bitcoin::{Transaction, Txid, Wtxid};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// Reserved orphan weight per peer.
pub const ORPHAN_RESERVED_WEIGHT_PER_PEER: u64 = 404_000;
/// Peer multiplier for a fixed global weight budget.
pub const ORPHAN_PEER_BUDGET: u64 = 25;
/// Global unique orphan weight cap (404k × 25 ≈ 10.1M WU).
pub const DEFAULT_ORPHAN_MAX_WEIGHT: u64 = ORPHAN_RESERVED_WEIGHT_PER_PEER * ORPHAN_PEER_BUDGET;
/// Secondary unique-count cap.
pub const DEFAULT_ORPHAN_MAX_COUNT: usize = 3_000;
/// Refuse orphans heavier than a standard tx.
pub const MAX_ORPHAN_TX_WEIGHT: u64 = 404_000;
/// Drop an orphan this long after it was parked. Not a knob.
pub const ORPHAN_EXPIRE_MS: u64 = 20 * 60 * 1000;

#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct OrphanEntry {
    pub tx: Transaction,
    pub wtxid: Wtxid,
    pub weight: u64,
    /// Missing parent txids (prevout.txid not in mempool/chain at insert).
    pub missing: BTreeSet<Txid>,
    /// P2P peer ids that announced this orphan.
    pub announcers: BTreeSet<u64>,
    pub arrived_ms: u64,
}

/// One parked orphan (txid + announcer peer ids).
#[derive(Debug, Clone)]
pub struct OrphanSnapshot {
    pub tx: Transaction,
    pub announcers: Vec<u64>,
}

/// Side pool of not-yet-acceptable txs waiting on parent(s).
#[derive(Debug, Default)]
pub struct Orphanage {
    pub by_txid: HashMap<Txid, OrphanEntry>,
    pub by_wtxid: HashMap<Wtxid, Txid>,
    /// parent txid → orphan children waiting on it.
    pub by_parent: HashMap<Txid, HashSet<Txid>>,
    pub fifo: VecDeque<Txid>,
    pub total_weight: u64,
    /// Announced weight per peer. Eviction reads this instead of scanning the set.
    pub peer_weight: HashMap<u64, u64>,
    pub max_weight: u64,
    pub max_count: usize,
}

impl Orphanage {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_ORPHAN_MAX_WEIGHT, DEFAULT_ORPHAN_MAX_COUNT)
    }

    pub fn with_limits(max_weight: u64, max_count: usize) -> Self {
        Self {
            by_txid: HashMap::new(),
            by_wtxid: HashMap::new(),
            by_parent: HashMap::new(),
            fifo: VecDeque::new(),
            total_weight: 0,
            peer_weight: HashMap::new(),
            max_weight: max_weight.max(MAX_ORPHAN_TX_WEIGHT),
            max_count: max_count.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.by_txid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_txid.is_empty()
    }

    pub fn total_weight(&self) -> u64 {
        self.total_weight
    }

    pub fn contains(&self, txid: &Txid) -> bool {
        self.by_txid.contains_key(txid)
    }

    pub fn contains_wtxid(&self, wtxid: &Wtxid) -> bool {
        self.by_wtxid.contains_key(wtxid)
    }

    pub fn wtxid_of(&self, txid: &Txid) -> Option<Wtxid> {
        self.by_txid.get(txid).map(|e| e.wtxid)
    }

    pub fn tx_by_wtxid(&self, wtxid: &Wtxid) -> Option<&Transaction> {
        let txid = self.by_wtxid.get(wtxid)?;
        self.by_txid.get(txid).map(|e| &e.tx)
    }

    pub fn missing_of(&self, txid: &Txid) -> Option<&BTreeSet<Txid>> {
        self.by_txid.get(txid).map(|e| &e.missing)
    }

    pub fn txs(&self) -> impl Iterator<Item = &Transaction> {
        self.by_txid.values().map(|e| &e.tx)
    }

    /// Insert (or add `from` as announcer if already parked).
    pub fn insert_from(
        &mut self,
        tx: Transaction,
        missing: BTreeSet<Txid>,
        from: Option<u64>,
    ) -> bool {
        self.insert_from_at(tx, missing, from, unix_ms())
    }

    /// [`Self::insert_from`] at an explicit unix millisecond clock.
    pub fn insert_from_at(
        &mut self,
        tx: Transaction,
        missing: BTreeSet<Txid>,
        from: Option<u64>,
        now_ms: u64,
    ) -> bool {
        if missing.is_empty() {
            return false;
        }
        self.expire(now_ms);
        let txid = tx.compute_txid();
        let wtxid = tx.compute_wtxid();
        if let Some(e) = self.by_txid.get(&txid) {
            if e.wtxid == wtxid {
                if let Some(peer) = from {
                    self.add_announcer(&txid, peer);
                }
                return false;
            }
            self.remove_txid(&txid);
        }
        let weight = tx.weight().to_wu();
        if weight > MAX_ORPHAN_TX_WEIGHT {
            return false;
        }
        if let Some(peer) = from {
            while self.peer_orphan_weight(peer).saturating_add(weight)
                > ORPHAN_RESERVED_WEIGHT_PER_PEER
            {
                if !self.evict_one_from_peer(peer) {
                    return false;
                }
            }
        }
        while !self.by_txid.is_empty()
            && (self.by_txid.len() >= self.max_count
                || self.total_weight.saturating_add(weight) > self.max_weight)
        {
            if !self.evict_oldest() {
                break;
            }
        }
        if self.by_txid.len() >= self.max_count {
            return false;
        }
        // Room left is the cap minus what is already parked. A tx that lands
        // on the cap still fits; one past it does not.
        if self.max_weight.saturating_sub(self.total_weight) < weight {
            return false;
        }
        for p in &missing {
            self.by_parent.entry(*p).or_default().insert(txid);
        }
        let mut announcers = BTreeSet::new();
        if let Some(peer) = from {
            announcers.insert(peer);
        }
        self.by_wtxid.insert(wtxid, txid);
        self.by_txid.insert(
            txid,
            OrphanEntry {
                tx,
                wtxid,
                weight,
                missing,
                announcers,
                arrived_ms: now_ms,
            },
        );
        self.fifo.push_back(txid);
        self.total_weight = self.total_weight.saturating_add(weight);
        if let Some(peer) = from {
            self.add_peer_weight(peer, weight);
        }
        true
    }

    #[doc(hidden)]
    pub fn expire(&mut self, now_ms: u64) {
        let stale: Vec<Txid> = self
            .by_txid
            .iter()
            .filter(|(_, e)| now_ms.saturating_sub(e.arrived_ms) >= ORPHAN_EXPIRE_MS)
            .map(|(txid, _)| *txid)
            .collect();
        for txid in stale {
            self.remove_txid(&txid);
        }
    }

    #[doc(hidden)]
    pub fn add_peer_weight(&mut self, peer: u64, weight: u64) {
        let w = self.peer_weight.entry(peer).or_default();
        *w = w.saturating_add(weight);
    }

    #[doc(hidden)]
    pub fn sub_peer_weight(&mut self, peer: u64, weight: u64) {
        let next = self
            .peer_weight
            .get(&peer)
            .copied()
            .unwrap_or(0)
            .saturating_sub(weight);
        if next == 0 {
            self.peer_weight.remove(&peer);
        } else {
            self.peer_weight.insert(peer, next);
        }
    }

    #[doc(hidden)]
    pub fn unaccount(&mut self, e: &OrphanEntry) {
        self.total_weight = self.total_weight.saturating_sub(e.weight);
        for peer in &e.announcers {
            self.sub_peer_weight(*peer, e.weight);
        }
    }

    #[doc(hidden)]
    pub fn peer_orphan_weight(&self, peer: u64) -> u64 {
        self.peer_weight.get(&peer).copied().unwrap_or(0)
    }

    #[doc(hidden)]
    pub fn announcers_within_reserve(&self, txid: &Txid) -> bool {
        let Some(e) = self.by_txid.get(txid) else {
            return false;
        };
        if e.announcers.is_empty() {
            return false;
        }
        e.announcers
            .iter()
            .any(|p| self.peer_orphan_weight(*p) <= ORPHAN_RESERVED_WEIGHT_PER_PEER)
    }

    /// Drop `peer` from the oldest orphan it announced. The tx stays when
    /// another peer still announces it; only that peer's weight is released.
    #[doc(hidden)]
    pub fn evict_one_from_peer(&mut self, peer: u64) -> bool {
        let victim = self
            .fifo
            .iter()
            .find(|txid| {
                self.by_txid
                    .get(*txid)
                    .is_some_and(|e| e.announcers.contains(&peer))
            })
            .copied();
        let Some(txid) = victim else {
            return false;
        };
        let weight = {
            let Some(e) = self.by_txid.get_mut(&txid) else {
                return false;
            };
            if !e.announcers.remove(&peer) {
                return false;
            }
            e.weight
        };
        self.sub_peer_weight(peer, weight);
        let last = self
            .by_txid
            .get(&txid)
            .is_some_and(|e| e.announcers.is_empty());
        if last {
            self.remove_txid(&txid);
        }
        self.fifo.retain(|t| self.by_txid.contains_key(t));
        true
    }

    #[doc(hidden)]
    pub fn evict_oldest(&mut self) -> bool {
        if self.evict_oldest_filtered(true) {
            return true;
        }
        // The reserve is a preference. A set that is entirely protected still
        // makes room for one newer orphan.
        self.evict_oldest_filtered(false)
    }

    #[doc(hidden)]
    pub fn evict_oldest_filtered(&mut self, honor_reserve: bool) -> bool {
        let mut skipped = VecDeque::new();
        while let Some(txid) = self.fifo.pop_front() {
            if !self.by_txid.contains_key(&txid) {
                continue;
            }
            if honor_reserve && self.announcers_within_reserve(&txid) {
                skipped.push_back(txid);
                continue;
            }
            self.remove_txid(&txid);
            skipped.append(&mut self.fifo);
            self.fifo = skipped;
            return true;
        }
        self.fifo = skipped;
        false
    }

    #[doc(hidden)]
    pub fn remove_txid(&mut self, txid: &Txid) -> Option<Transaction> {
        let e = self.by_txid.remove(txid)?;
        self.by_wtxid.remove(&e.wtxid);
        self.unaccount(&e);
        for p in &e.missing {
            if let Some(set) = self.by_parent.get_mut(p) {
                set.remove(txid);
                if set.is_empty() {
                    self.by_parent.remove(p);
                }
            }
        }
        // fifo may still hold txid if removed mid-list; leave stale ids (skipped on pop)
        Some(e.tx)
    }

    /// Remove one parked tx and return it. `None` when it is not parked.
    pub fn take_txid(&mut self, txid: &Txid) -> Option<Transaction> {
        self.remove_txid(txid)
    }

    /// Take all orphans that listed `parent` as missing (for re-accept).
    pub fn take_children_of(&mut self, parent: &Txid) -> Vec<Transaction> {
        let Some(children) = self.by_parent.remove(parent) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(children.len());
        for cid in children {
            if let Some(e) = self.by_txid.remove(&cid) {
                self.by_wtxid.remove(&e.wtxid);
                self.unaccount(&e);
                for p in &e.missing {
                    if p == parent {
                        continue;
                    }
                    if let Some(set) = self.by_parent.get_mut(p) {
                        set.remove(&cid);
                        if set.is_empty() {
                            self.by_parent.remove(p);
                        }
                    }
                }
                out.push(e.tx);
            }
        }
        out
    }

    pub fn announcers_of(&self, txid: &Txid) -> Vec<u64> {
        self.by_txid
            .get(txid)
            .map(|e| e.announcers.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn has_announcer(&self, peer: u64) -> bool {
        self.by_txid.values().any(|e| e.announcers.contains(&peer))
    }

    pub fn add_announcer(&mut self, txid: &Txid, peer: u64) -> bool {
        let weight = {
            let Some(e) = self.by_txid.get_mut(txid) else {
                return false;
            };
            if !e.announcers.insert(peer) {
                return false;
            }
            e.weight
        };
        self.add_peer_weight(peer, weight);
        true
    }

    pub fn add_announcer_wtxid(&mut self, wtxid: &Wtxid, peer: u64) -> bool {
        let Some(txid) = self.by_wtxid.get(wtxid).copied() else {
            return false;
        };
        self.add_announcer(&txid, peer)
    }

    /// Drop `peer` as announcer; erase orphans with no remaining announcers.
    pub fn erase_for_peer(&mut self, peer: u64) {
        let hit: Vec<(Txid, u64, bool)> = self
            .by_txid
            .iter()
            .filter_map(|(txid, e)| {
                if !e.announcers.contains(&peer) {
                    return None;
                }
                Some((*txid, e.weight, e.announcers.len() == 1))
            })
            .collect();
        let mut drop = Vec::new();
        for (txid, weight, last) in hit {
            if let Some(e) = self.by_txid.get_mut(&txid) {
                e.announcers.remove(&peer);
            }
            self.sub_peer_weight(peer, weight);
            if last {
                drop.push(txid);
            }
        }
        for t in drop {
            self.remove_txid(&t);
        }
        self.fifo.retain(|t| self.by_txid.contains_key(t));
    }

    pub fn snapshot(&self) -> Vec<OrphanSnapshot> {
        let mut out = Vec::with_capacity(self.fifo.len());
        let mut seen = HashSet::new();
        for txid in &self.fifo {
            if !seen.insert(*txid) {
                continue;
            }
            if let Some(e) = self.by_txid.get(txid) {
                out.push(OrphanSnapshot {
                    tx: e.tx.clone(),
                    announcers: e.announcers.iter().copied().collect(),
                });
            }
        }
        out
    }

    /// Drop orphans that are themselves included in a confirmed block.
    ///
    /// Children of confirmed parents are **not** erased here — callers should
    /// [`take_children_of`] and re-accept (Core `AddChildrenToWorkSet` path).
    /// Spending a parent in the block does not invalidate the orphan; the parent
    /// create is now a chain UTXO.
    pub fn erase_for_block(&mut self, block_txids: &[Txid]) {
        let block: HashSet<Txid> = block_txids.iter().copied().collect();
        let drop: Vec<Txid> = self
            .by_txid
            .keys()
            .filter(|t| block.contains(*t))
            .copied()
            .collect();
        for t in drop {
            self.remove_txid(&t);
        }
        self.fifo.retain(|t| self.by_txid.contains_key(t));
    }
}

#[doc(hidden)]
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
