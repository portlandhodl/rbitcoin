//! Most-work ranking helpers (header work sums, LCA on a parent graph).
//!
//! Layer 1 of most-work selection (`docs/architecture.md`): candidate
//! ranking uses header work only. Apply/validation is separate
//! (`ChainHub::accept_branch`).

use bitcoin::Work;

/// Header work could not be summed or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkOverflow;

/// Sum header work values (Bitcoin most-work accumulation).
/// Overflow is an error. Addition is byte-wise so a full `Work` does not
/// hit rust-bitcoin's debug overflow assert.
pub fn sum_work(iter: impl Iterator<Item = Work>) -> Result<Work, WorkOverflow> {
    let mut acc: Option<Work> = None;
    for w in iter {
        acc = Some(match acc {
            None => w,
            Some(a) => checked_add_work(a, w)?,
        });
    }
    Ok(acc.unwrap_or_else(|| Work::from_be_bytes([0u8; 32])))
}

#[doc(hidden)]
pub fn checked_add_work(a: Work, b: Work) -> Result<Work, WorkOverflow> {
    let mut out = a.to_be_bytes();
    let rhs = b.to_be_bytes();
    let mut carry = 0u16;
    for i in (0..32).rev() {
        let s = u16::from(out[i]) + u16::from(rhs[i]) + carry;
        out[i] = s as u8;
        carry = s >> 8;
    }
    if carry != 0 {
        Err(WorkOverflow)
    } else {
        Ok(Work::from_be_bytes(out))
    }
}

/// `Header::work` panics in debug when `bits` is a zero target.
pub fn header_work_checked(header: &bitcoin::block::Header) -> Result<Work, WorkOverflow> {
    if header.bits.to_consensus() == 0 {
        return Err(WorkOverflow);
    }
    let target = bitcoin::Target::from_compact(header.bits);
    if target.to_be_bytes() == [0u8; 32] {
        return Err(WorkOverflow);
    }
    Ok(header.work())
}

/// Strictly more work (Bitcoin most-work rule).
#[inline]
pub fn work_better(new: Work, old: Work) -> bool {
    new > old
}

/// Process-local set of invalid block / candidate-tip hashes after failed apply.
#[derive(Debug, Default, Clone)]
pub struct InvalidHashSet {
    pub hashes: std::collections::HashSet<[u8; 32]>,
}

impl InvalidHashSet {
    /// The set does not grow past this. A hash that does not fit is still
    /// invalid for the decision that just failed it; later lookups may miss it.
    pub const CAP: usize = 4_096;

    pub fn mark(&mut self, hash: [u8; 32]) {
        if self.hashes.len() >= Self::CAP && !self.hashes.contains(&hash) {
            return;
        }
        self.hashes.insert(hash);
    }

    pub fn iter(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        self.hashes.iter().copied()
    }

    pub fn contains(&self, hash: [u8; 32]) -> bool {
        self.hashes.contains(&hash)
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}
