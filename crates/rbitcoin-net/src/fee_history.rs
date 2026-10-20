//! Per-height fee hurdles read from `txstat`, for historical fee estimates.
//!
//! Heights are kept newest-first up to a `txstat.body` byte budget. Each
//! height's hurdle feeds [`AnalogHistory`]; a height without one (a
//! coinbase-only block, or every tx below min relay) is held for the byte
//! and reorg bookkeeping but is not an observation.

use rbitcoin_mempool::{AnalogHistory, CONFIDENCE_NEAR, HISTORICAL_CONFIDENCE_FAR};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

/// Newest heights whose block hash is kept to validate the history file.
pub const RECENT_HASHES: usize = 144;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoricalFeeBlock {
    pub p10_sat_kvb: Option<u64>,
    pub txstat_bytes: u64,
}

#[derive(Debug)]
pub struct FeeHistory {
    pub blocks: BTreeMap<u32, HistoricalFeeBlock>,
    pub txstat_bytes: u64,
    pub budget: u64,
    pub targets: Vec<u32>,
    pub hashes: BTreeMap<u32, [u8; 32]>,
    pub analog: AnalogHistory,
    /// An insert between held heights; rebuild before the next estimate.
    pub analog_stale: bool,
    /// Per-target rate, cleared on any change (one rebuild per new block).
    pub rates: Option<HashMap<u32, Option<u64>>>,
}

impl FeeHistory {
    pub fn new(budget: u64, targets: &[u32]) -> Self {
        Self {
            blocks: BTreeMap::new(),
            txstat_bytes: 0,
            budget,
            targets: targets.to_vec(),
            hashes: BTreeMap::new(),
            analog: AnalogHistory::new(targets),
            analog_stale: false,
            rates: None,
        }
    }

    pub fn get(&self, height: u32) -> Option<HistoricalFeeBlock> {
        self.blocks.get(&height).copied()
    }

    /// A connect at `height`: it replaces that height and every height above.
    pub fn insert(&mut self, height: u32, block: HistoricalFeeBlock, hash: Option<[u8; 32]>) {
        self.truncate_above(height);
        if let Some(old) = self.blocks.remove(&height) {
            self.txstat_bytes = self.txstat_bytes.saturating_sub(old.txstat_bytes);
            if old.p10_sat_kvb.is_some() && !self.analog_stale {
                self.analog.pop_back();
            }
        }
        self.hashes.remove(&height);
        self.blocks.insert(height, block);
        self.txstat_bytes = self.txstat_bytes.saturating_add(block.txstat_bytes);
        if let (Some(rate), false) = (block.p10_sat_kvb, self.analog_stale) {
            self.analog.push_back(rate);
        }
        self.note_hash(height, hash);
        self.evict();
    }

    /// A preload row: kept only when the height is not held.
    pub fn insert_if_absent(
        &mut self,
        height: u32,
        block: HistoricalFeeBlock,
        hash: Option<[u8; 32]>,
    ) {
        if self.blocks.contains_key(&height) {
            return;
        }
        self.rates = None;
        let first = self.blocks.first_key_value().map(|(&h, _)| h);
        let last = self.blocks.last_key_value().map(|(&h, _)| h);
        if let (Some(rate), false) = (block.p10_sat_kvb, self.analog_stale) {
            if let (Some(first), Some(last)) = (first, last) {
                match (height.cmp(&first), height.cmp(&last)) {
                    (_, Ordering::Greater) => self.analog.push_back(rate),
                    (Ordering::Less, _) => self.analog.push_front(rate),
                    _ => self.analog_stale = true,
                }
            } else {
                self.analog.push_back(rate);
            }
        }
        self.blocks.insert(height, block);
        self.txstat_bytes = self.txstat_bytes.saturating_add(block.txstat_bytes);
        self.note_hash(height, hash);
        self.evict();
    }

    /// Drop every height above `height` (a reorg found while loading).
    pub fn truncate_above(&mut self, height: u32) {
        self.rates = None;
        let above = self.blocks.split_off(&height.saturating_add(1));
        for old in above.values() {
            self.txstat_bytes = self.txstat_bytes.saturating_sub(old.txstat_bytes);
            if old.p10_sat_kvb.is_some() && !self.analog_stale {
                self.analog.pop_back();
            }
        }
        self.hashes.split_off(&height.saturating_add(1));
    }

    #[doc(hidden)]
    pub fn note_hash(&mut self, height: u32, hash: Option<[u8; 32]>) {
        if let Some(hash) = hash {
            self.hashes.insert(height, hash);
            while self.hashes.len() > RECENT_HASHES {
                self.hashes.pop_first();
            }
        }
    }

    #[doc(hidden)]
    pub fn evict(&mut self) {
        self.rates = None;
        while self.txstat_bytes > self.budget {
            let Some((height, old)) = self.blocks.pop_first() else {
                self.txstat_bytes = 0;
                break;
            };
            self.txstat_bytes = self.txstat_bytes.saturating_sub(old.txstat_bytes);
            if old.p10_sat_kvb.is_some() && !self.analog_stale {
                self.analog.pop_front();
            }
            self.hashes.remove(&height);
        }
    }

    /// Historical rate per target (1 block at [`CONFIDENCE_NEAR`], farther at
    /// [`HISTORICAL_CONFIDENCE_FAR`]); None while a target is not ready.
    pub fn rates(&mut self) -> HashMap<u32, Option<u64>> {
        if let Some(rates) = &self.rates {
            return rates.clone();
        }
        if self.analog_stale {
            self.analog
                .rebuild(self.blocks.values().filter_map(|b| b.p10_sat_kvb));
            self.analog_stale = false;
        }
        let rates: HashMap<u32, Option<u64>> = self
            .targets
            .iter()
            .map(|&n| {
                let confidence = if n <= 1 {
                    CONFIDENCE_NEAR
                } else {
                    HISTORICAL_CONFIDENCE_FAR
                };
                (n, self.analog.rate_sat_kvb(n, confidence))
            })
            .collect();
        self.rates = Some(rates.clone());
        rates
    }

    /// Held heights, oldest first.
    pub fn entries(&self) -> Vec<(u32, HistoricalFeeBlock)> {
        self.blocks.iter().map(|(&h, &b)| (h, b)).collect()
    }

    /// Kept block hashes, oldest first.
    pub fn recent_hashes(&self) -> Vec<(u32, [u8; 32])> {
        self.hashes.iter().map(|(&h, &hash)| (h, hash)).collect()
    }
}
