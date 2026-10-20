//! Newest-first identity pick for head resolve (BIP30 / fence).
//!
//! A wave fills identities in at most two page-grouped `txid.body` shots
//! (first [`ID_FILL_CHUNK`] cands, then the rest). One walk of the filled
//! prefix returns the fence-connected winner (or the newest body match when
//! no fence), whether any body matched, and the miss count. Entries after
//! the winner still count as misses.

use crate::height_fence::HeightFence;
use crate::int_map::{U64Map, U64Set};
use rbitcoin_primitives::Fk;

/// First identity shot size. Later cands wait for shot B only if the key is
/// still unfinished (no connected win; unconnected body match is not enough).
pub const ID_FILL_CHUNK: usize = 4;

/// Unread prefix of `take` cands for keys that are not `skip`.
pub fn next_id_shot(
    cands_by_key: &[Vec<Fk>],
    filled: &[usize],
    skip: &[bool],
    take: usize,
) -> Vec<Fk> {
    let mut need = Vec::new();
    let mut seen = U64Set::default();
    for (ki, cands) in cands_by_key.iter().enumerate() {
        if skip.get(ki).copied().unwrap_or(false) {
            continue;
        }
        let start = filled.get(ki).copied().unwrap_or(0);
        for &fk in cands.iter().skip(start).take(take) {
            let Some(id) = fk.get() else {
                continue;
            };
            if seen.insert(id) {
                need.push(fk);
            }
        }
    }
    need
}

/// One pass over `cands[..filled]`: winner, any body match, and miss count.
///
/// `winner` is the fence-connected body match. With no fence it is the newest
/// body match. `unconnected_fallback` is the end-of-list TipThenAny case: no
/// connected hit, so the newest body match wins. Newer unconnected matches do
/// not win while a fence is present and this flag is clear.
///
/// `miss` counts prefix entries that are not `want`, including entries after
/// the winner.
pub struct PrefixWalk {
    pub winner: Option<(Fk, u64)>,
    pub had_body: bool,
    pub miss: u64,
}

pub fn walk_id_prefix(
    cands: &[Fk],
    filled: usize,
    want: &[u8; 32],
    id_of: &U64Map<[u8; 32]>,
    heights: Option<&HeightFence>,
    unconnected_fallback: bool,
) -> PrefixWalk {
    let n = filled.min(cands.len());
    let mut first_match: Option<(Fk, u64)> = None;
    let mut connected: Option<(Fk, u64)> = None;
    let mut miss = 0u64;
    for (i, &fk) in cands.iter().take(n).enumerate() {
        let rank = (i + 1) as u64;
        let body_hit = match fk.get() {
            Some(id) => id_of.get(&id).is_some_and(|got| got.as_slice() == want),
            None => false,
        };
        if !body_hit {
            // The stat covers the whole filled prefix, not only the prefix
            // before the winner.
            miss = miss.saturating_add(1);
            continue;
        }
        if first_match.is_none() {
            first_match = Some((fk, rank));
        }
        if connected.is_none() && heights.is_some_and(|ht| ht.height_of(fk).is_some()) {
            connected = Some((fk, rank));
        }
    }
    let winner = if heights.is_some() {
        connected.or(if unconnected_fallback {
            first_match
        } else {
            None
        })
    } else {
        first_match
    };
    PrefixWalk {
        winner,
        had_body: first_match.is_some(),
        miss,
    }
}

/// Which lookup table a TipOnly leftover miss failed on.
///
/// Confirm leftover is `tx.head` probe → `txid.body` identity → fence.
/// The operator line names the first table that did not produce a usable fact.
/// A connected identity is a pick, not a miss, so it is not a class here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftoverMissOn {
    /// No probe candidates (hot + cold).
    Head,
    /// Probe cands, no `txid.body` match for the wanted txid.
    Body,
    /// Identity match exists, no fence height (TipOnly drops it).
    Fence,
}

impl LeftoverMissOn {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Body => "body",
            Self::Fence => "fence",
        }
    }
}

/// Classify a TipOnly leftover miss from facts the resolve machine already has.
///
/// A connected identity always has `picked` set, and TipOnly classification
/// only runs for keys whose pick is empty. `connected == true` never arrives.
pub fn classify_leftover_miss(n_cands: usize, had_identity: bool) -> LeftoverMissOn {
    if n_cands == 0 {
        LeftoverMissOn::Head
    } else if !had_identity {
        LeftoverMissOn::Body
    } else {
        LeftoverMissOn::Fence
    }
}
