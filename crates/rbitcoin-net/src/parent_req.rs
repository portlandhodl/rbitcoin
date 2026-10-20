//! Missing-parent GETDATA tracker.
//!
//! One mutex owns the hash map and a per-peer time index. A heartbeat walks
//! only that peer's due entries. Caps are announcements, not a new knob.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{GETDATA_TX_INTERVAL_SECS, MAX_PARENTS_PER_PARK};

/// Announcements one peer may add. Past this the map does not grow.
pub const MAX_PARENT_ANN_PER_PEER: usize = 5_000;
/// Process-wide announcements. A few hundred thousand, not tens of millions.
pub const MAX_PARENT_ANN_GLOBAL: usize = 200_000;

/// One parent GETDATA the sweep should send.
pub struct DueParent {
    pub hash: [u8; 32],
    pub wtxid: bool,
}

/// Why `note_inv` did or did not ask this peer for the transaction now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentNote {
    /// This peer holds the in-flight request. Send getdata now.
    RequestNow,
    /// Recorded, or already recorded. Another request for these bytes is
    /// in flight, or this peer's own request is already live or failed.
    Deferred,
    /// This peer's own counter is full.
    PeerCapped,
    /// The process-wide table is full. The announcement is skipped.
    GlobalFull,
}

#[doc(hidden)]
pub struct ParentAnn {
    pub peer: u64,
    pub preferred: bool,
    pub reqtime: u64,
    /// Bucket in `due_by_peer` while this ann is waiting to be selected.
    pub due_at: Option<u64>,
    pub requested_until: Option<u64>,
    pub failed: bool,
    /// This announcement's getdata type. A wtxid inv does not retarget
    /// another peer's txid row.
    pub wtxid: bool,
}

#[doc(hidden)]
pub struct ParentSlot {
    pub anns: Vec<ParentAnn>,
}

/// Hash map plus per-peer due and in-flight indexes. The indexes are one
/// entry per announcement and stop at the same caps (extra RAM, not a scan
/// of every peer on the heartbeat).
pub struct ParentTracker {
    pub by_hash: HashMap<[u8; 32], ParentSlot>,
    pub due_by_peer: HashMap<u64, BTreeMap<u64, Vec<[u8; 32]>>>,
    pub inflight_by_peer: HashMap<u64, BTreeMap<u64, Vec<[u8; 32]>>>,
    pub hashes_by_peer: HashMap<u64, HashSet<[u8; 32]>>,
    pub ann_per_peer: HashMap<u64, usize>,
    pub announcements: usize,
}

impl ParentTracker {
    pub fn new() -> Self {
        Self {
            by_hash: HashMap::new(),
            due_by_peer: HashMap::new(),
            inflight_by_peer: HashMap::new(),
            hashes_by_peer: HashMap::new(),
            ann_per_peer: HashMap::new(),
            announcements: 0,
        }
    }

    #[doc(hidden)]
    pub fn peer_at_cap(&self, peer: u64) -> bool {
        self.ann_per_peer.get(&peer).copied().unwrap_or(0) >= MAX_PARENT_ANN_PER_PEER
    }

    #[doc(hidden)]
    pub fn global_full(&self) -> bool {
        self.announcements >= MAX_PARENT_ANN_GLOBAL
    }

    pub fn note_inv(
        &mut self,
        peer: u64,
        hash: [u8; 32],
        inbound: bool,
        now: u64,
        wtxid: bool,
    ) -> ParentNote {
        if let Some(note) = self.rearm_existing(peer, hash, now, wtxid) {
            return note;
        }
        if self.peer_at_cap(peer) {
            return ParentNote::PeerCapped;
        }
        if self.global_full() {
            return ParentNote::GlobalFull;
        }
        let exp = now.saturating_add(GETDATA_TX_INTERVAL_SECS);
        // Same-kind in-flight stays the one request. This announcer waits
        // until that window ends and keeps its own getdata type.
        let (due_at, requested_until) = match self.kind_inflight_until(&hash, wtxid) {
            Some(until) => (Some(until), None),
            None => (None, Some(exp)),
        };
        let note = if requested_until.is_some() {
            ParentNote::RequestNow
        } else {
            ParentNote::Deferred
        };
        self.add_ann(
            hash,
            ParentAnn {
                peer,
                preferred: !inbound,
                reqtime: now,
                due_at,
                requested_until,
                failed: false,
                wtxid,
            },
        );
        note
    }

    pub fn schedule(&mut self, hash: [u8; 32], peer: u64, preferred: bool, reqtime: u64) {
        if self
            .by_hash
            .get(&hash)
            .is_some_and(|slot| slot.anns.iter().any(|a| a.peer == peer && !a.wtxid))
        {
            return;
        }
        if self.peer_at_cap(peer) || self.global_full() {
            return;
        }
        self.add_ann(
            hash,
            ParentAnn {
                peer,
                preferred,
                reqtime,
                due_at: Some(reqtime),
                requested_until: None,
                failed: false,
                wtxid: false,
            },
        );
    }

    /// `Some` when this peer already had this kind of announcement for `hash`.
    #[doc(hidden)]
    pub fn rearm_existing(
        &mut self,
        peer: u64,
        hash: [u8; 32],
        now: u64,
        wtxid: bool,
    ) -> Option<ParentNote> {
        let slot = self.by_hash.get_mut(&hash)?;
        let pos = slot
            .anns
            .iter()
            .position(|a| a.peer == peer && a.wtxid == wtxid)?;
        let other_inflight = slot
            .anns
            .iter()
            .any(|a| a.wtxid == wtxid && a.peer != peer && a.requested_until.is_some());
        let due_at = {
            if slot.anns[pos].requested_until.is_some() || slot.anns[pos].failed || other_inflight {
                return Some(ParentNote::Deferred);
            }
            let exp = now.saturating_add(GETDATA_TX_INTERVAL_SECS);
            let due_at = slot.anns[pos].due_at;
            slot.anns[pos].requested_until = Some(exp);
            slot.anns[pos].due_at = None;
            (due_at, exp)
        };
        if let Some(t) = due_at.0 {
            unindex(&mut self.due_by_peer, peer, t, &hash);
        }
        index_at(&mut self.inflight_by_peer, peer, due_at.1, hash);
        Some(ParentNote::RequestNow)
    }

    #[doc(hidden)]
    pub fn add_ann(&mut self, hash: [u8; 32], ann: ParentAnn) {
        let peer = ann.peer;
        let due_at = ann.due_at;
        let exp = ann.requested_until;
        let slot = self
            .by_hash
            .entry(hash)
            .or_insert_with(|| ParentSlot { anns: Vec::new() });
        slot.anns.push(ann);
        *self.ann_per_peer.entry(peer).or_default() += 1;
        self.announcements += 1;
        self.hashes_by_peer.entry(peer).or_default().insert(hash);
        if let Some(exp) = exp {
            index_at(&mut self.inflight_by_peer, peer, exp, hash);
        } else if let Some(t) = due_at {
            index_at(&mut self.due_by_peer, peer, t, hash);
        }
    }

    /// The peer answered notfound for a hash we asked it for. That request
    /// is over, and a waiter of the same kind is due at its announcement.
    pub fn note_not_found(&mut self, peer: u64, hash: [u8; 32]) {
        let inflight: Vec<(u64, bool)> = self
            .by_hash
            .get(&hash)
            .map(|slot| {
                slot.anns
                    .iter()
                    .filter(|a| a.peer == peer)
                    .filter_map(|a| a.requested_until.map(|exp| (exp, a.wtxid)))
                    .collect()
            })
            .unwrap_or_default();
        for (exp, wtxid) in inflight {
            self.fail_inflight(&hash, peer, exp);
            self.release_waiters(&hash, wtxid);
        }
    }

    pub fn forget_peer(&mut self, peer: u64) {
        self.due_by_peer.remove(&peer);
        self.inflight_by_peer.remove(&peer);
        let Some(hashes) = self.hashes_by_peer.remove(&peer) else {
            return;
        };
        for hash in hashes {
            self.remove_peer_ann(&hash, peer);
        }
    }

    pub fn announcer_peers(&self, hashes: [[u8; 32]; 2]) -> Vec<u64> {
        let mut peers = Vec::new();
        for hash in hashes {
            let Some(slot) = self.by_hash.get(&hash) else {
                continue;
            };
            for a in &slot.anns {
                if !a.failed && !peers.contains(&a.peer) {
                    peers.push(a.peer);
                }
            }
        }
        peers
    }

    pub fn resolve(&mut self, hashes: [[u8; 32]; 2], admitted: bool) {
        for hash in hashes {
            if admitted {
                self.remove_hash(&hash);
                continue;
            }
            let inflight = self.inflight_peers(&hash);
            for (peer, exp) in inflight {
                self.fail_inflight(&hash, peer, exp);
            }
            if self.slot_all_failed(&hash) {
                self.remove_hash(&hash);
            }
        }
    }

    pub fn take_due(
        &mut self,
        peer: u64,
        now: u64,
        mut already_have: impl FnMut(&[u8; 32], bool) -> bool,
    ) -> Vec<DueParent> {
        self.expire_peer_inflight(peer, now);
        let due_hashes = self.due_hashes(peer, now);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for hash in due_hashes {
            if out.len() >= MAX_PARENTS_PER_PARK {
                break;
            }
            if !seen.insert(hash) {
                continue;
            }
            self.expire_hash_inflight(&hash, now);
            if !self.by_hash.contains_key(&hash) {
                continue;
            }
            if self.slot_all_failed(&hash) {
                self.remove_hash(&hash);
                continue;
            }
            let Some(wtxid) = self.select(peer, now, &hash) else {
                continue;
            };
            if already_have(&hash, wtxid) {
                self.drop_kind(&hash, wtxid);
                continue;
            }
            out.push(DueParent { hash, wtxid });
        }
        out
    }

    #[doc(hidden)]
    pub fn due_hashes(&self, peer: u64, now: u64) -> Vec<[u8; 32]> {
        let Some(tree) = self.due_by_peer.get(&peer) else {
            return Vec::new();
        };
        tree.range(..=now)
            .flat_map(|(_, v)| v.iter().copied())
            .collect()
    }

    /// When this kind is already in flight, the time that request ends.
    #[doc(hidden)]
    pub fn kind_inflight_until(&self, hash: &[u8; 32], wtxid: bool) -> Option<u64> {
        self.by_hash.get(hash).and_then(|slot| {
            slot.anns
                .iter()
                .filter(|a| a.wtxid == wtxid)
                .filter_map(|a| a.requested_until)
                .min()
        })
    }

    #[doc(hidden)]
    pub fn select(&mut self, peer: u64, now: u64, hash: &[u8; 32]) -> Option<bool> {
        let exp = now.saturating_add(GETDATA_TX_INTERVAL_SECS);
        let (due_at, wtxid) = {
            let slot = self.by_hash.get_mut(hash)?;
            let has_pref = slot
                .anns
                .iter()
                .any(|a| a.preferred && !a.failed && a.reqtime <= now);
            let pos = slot.anns.iter().position(|a| {
                !a.failed && a.reqtime <= now && a.peer == peer && (!has_pref || a.preferred)
            })?;
            let wtxid = slot.anns[pos].wtxid;
            // Any in-flight request for these bytes blocks a second getdata.
            // A non-segwit wtxid inv uses the txid, so the orphan parent is
            // already in flight. The kind we would send stays this ann's own.
            if slot.anns.iter().any(|a| a.requested_until.is_some()) {
                return None;
            }
            let ann = &mut slot.anns[pos];
            let due_at = ann.due_at.unwrap_or(ann.reqtime);
            ann.requested_until = Some(exp);
            ann.due_at = None;
            (due_at, wtxid)
        };
        unindex(&mut self.due_by_peer, peer, due_at, hash);
        index_at(&mut self.inflight_by_peer, peer, exp, *hash);
        Some(wtxid)
    }

    #[doc(hidden)]
    pub fn expire_peer_inflight(&mut self, peer: u64, now: u64) {
        let expired = self
            .inflight_by_peer
            .get(&peer)
            .map(|tree| {
                tree.range(..=now)
                    .flat_map(|(t, v)| v.iter().map(|h| (*t, *h)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (exp, hash) in expired {
            self.fail_inflight(&hash, peer, exp);
            if self.slot_all_failed(&hash) {
                self.remove_hash(&hash);
            }
        }
    }

    #[doc(hidden)]
    pub fn expire_hash_inflight(&mut self, hash: &[u8; 32], now: u64) {
        let expired = self.inflight_peers(hash);
        for (peer, exp) in expired {
            if exp <= now {
                self.fail_inflight(hash, peer, exp);
            }
        }
        if self.slot_all_failed(hash) {
            self.remove_hash(hash);
        }
    }

    #[doc(hidden)]
    pub fn inflight_peers(&self, hash: &[u8; 32]) -> Vec<(u64, u64)> {
        self.by_hash
            .get(hash)
            .map(|slot| {
                slot.anns
                    .iter()
                    .filter_map(|a| a.requested_until.map(|exp| (a.peer, exp)))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[doc(hidden)]
    pub fn fail_inflight(&mut self, hash: &[u8; 32], peer: u64, exp: u64) {
        // Expire the ann whose window is `exp`. A same-peer txid parent is a
        // different row and must stay eligible once this window ends.
        if let Some(slot) = self.by_hash.get_mut(hash) {
            if let Some(ann) = slot
                .anns
                .iter_mut()
                .find(|a| a.peer == peer && a.requested_until == Some(exp))
            {
                ann.requested_until = None;
                ann.failed = true;
            }
        }
        unindex(&mut self.inflight_by_peer, peer, exp, hash);
    }

    #[doc(hidden)]
    pub fn slot_all_failed(&self, hash: &[u8; 32]) -> bool {
        self.by_hash
            .get(hash)
            .is_some_and(|slot| !slot.anns.is_empty() && slot.anns.iter().all(|a| a.failed))
    }

    #[doc(hidden)]
    pub fn remove_hash(&mut self, hash: &[u8; 32]) {
        let Some(slot) = self.by_hash.remove(hash) else {
            return;
        };
        for ann in slot.anns {
            self.note_removed_ann(&ann, hash);
        }
    }

    #[doc(hidden)]
    pub fn drop_kind(&mut self, hash: &[u8; 32], wtxid: bool) {
        let peers: Vec<u64> = self
            .by_hash
            .get(hash)
            .map(|slot| {
                slot.anns
                    .iter()
                    .filter(|a| a.wtxid == wtxid)
                    .map(|a| a.peer)
                    .collect()
            })
            .unwrap_or_default();
        for peer in peers {
            self.remove_peer_ann_kind(hash, peer, Some(wtxid));
        }
    }

    #[doc(hidden)]
    pub fn remove_peer_ann(&mut self, hash: &[u8; 32], peer: u64) {
        self.remove_peer_ann_kind(hash, peer, None);
    }

    #[doc(hidden)]
    pub fn remove_peer_ann_kind(&mut self, hash: &[u8; 32], peer: u64, kind: Option<bool>) {
        loop {
            let Some(ann) = self.by_hash.get_mut(hash).and_then(|slot| {
                let pos = slot
                    .anns
                    .iter()
                    .position(|a| a.peer == peer && kind.is_none_or(|k| a.wtxid == k))?;
                Some(slot.anns.swap_remove(pos))
            }) else {
                return;
            };
            if self
                .by_hash
                .get(hash)
                .is_some_and(|slot| slot.anns.is_empty())
            {
                self.by_hash.remove(hash);
            }
            self.note_removed_ann(&ann, hash);
            if kind.is_some() {
                return;
            }
        }
    }

    #[doc(hidden)]
    pub fn note_removed_ann(&mut self, ann: &ParentAnn, hash: &[u8; 32]) {
        self.announcements = self.announcements.saturating_sub(1);
        if let Some(c) = self.ann_per_peer.get_mut(&ann.peer) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                self.ann_per_peer.remove(&ann.peer);
            }
        }
        if let Some(set) = self.hashes_by_peer.get_mut(&ann.peer) {
            let still = self
                .by_hash
                .get(hash)
                .is_some_and(|slot| slot.anns.iter().any(|a| a.peer == ann.peer));
            if !still {
                set.remove(hash);
                if set.is_empty() {
                    self.hashes_by_peer.remove(&ann.peer);
                }
            }
        }
        if let Some(exp) = ann.requested_until {
            unindex(&mut self.inflight_by_peer, ann.peer, exp, hash);
        }
        if let Some(t) = ann.due_at {
            unindex(&mut self.due_by_peer, ann.peer, t, hash);
        }
        if ann.requested_until.is_some() {
            self.release_waiters(hash, ann.wtxid);
        }
    }

    /// The in-flight request is gone. Waiters of this kind were due when
    /// that window ended. They are due at their own announcement time.
    #[doc(hidden)]
    pub fn release_waiters(&mut self, hash: &[u8; 32], wtxid: bool) {
        if self.kind_inflight_until(hash, wtxid).is_some() {
            return;
        }
        let waiters: Vec<(u64, u64, u64)> = self
            .by_hash
            .get(hash)
            .map(|slot| {
                slot.anns
                    .iter()
                    .filter(|a| a.wtxid == wtxid && !a.failed)
                    .filter_map(|a| a.due_at.map(|due| (a.peer, due, a.reqtime)))
                    .collect()
            })
            .unwrap_or_default();
        for (peer, due_at, reqtime) in waiters {
            if due_at <= reqtime {
                continue;
            }
            unindex(&mut self.due_by_peer, peer, due_at, hash);
            if let Some(ann) = self.by_hash.get_mut(hash).and_then(|slot| {
                slot.anns
                    .iter_mut()
                    .find(|a| a.peer == peer && a.wtxid == wtxid && a.due_at == Some(due_at))
            }) {
                ann.due_at = Some(reqtime);
            }
            index_at(&mut self.due_by_peer, peer, reqtime, *hash);
        }
    }
}

#[doc(hidden)]
pub fn index_at(
    tree: &mut HashMap<u64, BTreeMap<u64, Vec<[u8; 32]>>>,
    peer: u64,
    time: u64,
    hash: [u8; 32],
) {
    tree.entry(peer)
        .or_default()
        .entry(time)
        .or_default()
        .push(hash);
}

#[doc(hidden)]
pub fn unindex(
    tree: &mut HashMap<u64, BTreeMap<u64, Vec<[u8; 32]>>>,
    peer: u64,
    time: u64,
    hash: &[u8; 32],
) {
    let Some(by_time) = tree.get_mut(&peer) else {
        return;
    };
    let Some(v) = by_time.get_mut(&time) else {
        return;
    };
    if let Some(i) = v.iter().position(|h| h == hash) {
        v.swap_remove(i);
    }
    if v.is_empty() {
        by_time.remove(&time);
    }
    if by_time.is_empty() {
        tree.remove(&peer);
    }
}
