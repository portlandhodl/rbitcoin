use bitcoin::hashes::Hash;

/// Milestone policy (assumevalid-style): skip **script/sig** checks at/below height.
///
/// Prevout existence, double-spend, maturity, and fees are always checked on
/// contiguous tip confirm. Only pure-Rust script/signature verification is gated.
///
/// A height-only milestone (`anchor: None`) is the explicit `--milestone HEIGHT`
/// speed switch. The mainnet and testnet3 defaults also carry
/// [`MilestoneAnchor`]: skip only when this block and the anchor hash are the
/// header-path occupants at their heights. The anchor hash is the commitment
/// to those ancestors; chain work does not have to meet `min_work_be` first.
/// Lookups are the caller's; this type does not walk ancestors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MilestoneAnchor {
    pub hash: bitcoin::BlockHash,
    /// 32-byte big-endian chain work floor (Core `nMinimumChainWork` shape).
    pub min_work_be: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Milestone {
    pub height: u32,
    pub anchor: Option<MilestoneAnchor>,
}

impl Milestone {
    pub const NONE: Milestone = Milestone {
        height: 0,
        anchor: None,
    };

    /// Explicit height skip. No ancestor or min-work gate.
    pub const fn height(height: u32) -> Self {
        Self {
            height,
            anchor: None,
        }
    }

    /// Height-only skip. An anchored milestone never skips on height alone.
    pub fn skips_scripts_at(self, height: u32) -> bool {
        self.anchor.is_none() && self.height > 0 && height <= self.height
    }

    /// Script skip for `block` at `height`.
    ///
    /// `header_at` is an O(1) view of the header path (or the confirmed
    /// chain). A missing lookup does not skip. Once both hashes are present,
    /// work below the anchor floor still skips.
    pub fn skips_scripts(
        self,
        height: u32,
        block: &[u8; 32],
        header_at: impl Fn(u32) -> Option<[u8; 32]>,
    ) -> bool {
        if self.height == 0 || height > self.height {
            return false;
        }
        let Some(anchor) = self.anchor else {
            return true;
        };
        if header_at(height).as_ref() != Some(block) {
            return false;
        }
        header_at(self.height).as_ref() == Some(anchor.hash.as_byte_array())
    }
}

impl Default for Milestone {
    fn default() -> Self {
        Self::NONE
    }
}
