//! Scripthash layout: 8 B create_tx_fk entries, **24 B head slots** (key16 + pack8).
//!
//! Head key = first 16 B of SHA256(spk). Value = pack8 (8 B).
//!
//! **Schema 18:** Empty / Inline (1 FK) / **Slab** `{class,used,off}` /
//! **Paged** (megakey last 4 KiB page off) / **Extent** (mode 11).
//! See [`crate::scripthash_pages`] for page buffer layout.
//!
//! Store open refuses a durable pre-15 SH index.

use crate::error::StoreError;
use rbitcoin_primitives::Fk;

/// Body / page entry: create Class A fk only.
pub const SH_ENTRY_LEN: usize = 8;
/// Max create_tx_fks stored inline in the head value (schema 18: 8 B word).
pub const SH_INLINE_CAP: usize = 1;
/// Head key length (prefix of Electrum SHA256(spk)).
pub const SH_HEAD_KEY_LEN: usize = 16;
/// Head value: pack8 word.
pub const SH_HEAD_VALUE_LEN: usize = 8;
/// On-disk head slot size.
pub const SH_HEAD_SLOT_SIZE: usize = SH_HEAD_KEY_LEN + SH_HEAD_VALUE_LEN;
/// Alloc header magic after the RBT1 file header.
pub const SH_ALLOC_MAGIC: [u8; 4] = *b"SHAL";
/// v3 = schema 15 (slabs + combined RBT1/SHAL prefix). v2 = schema-14 pages. v1 = schema-13.
pub const SH_ALLOC_VERSION: u16 = 3;
/// Combined RBT1 + SHAL prefix. Payload starts here (no 4112 hole).
pub const SH_PREFIX_PAGE: usize = 4096;
/// SHAL field region after a 16 B RBT1 header (ends at [`SH_PREFIX_PAGE`]).
pub const SH_ALLOC_HEADER_LEN: usize = SH_PREFIX_PAGE - 16;

/// Size-class constants (page freelist reuses class index for 4 KiB pages).
/// `slab_bytes(c) = 16 << c`. Class 0 = 16 B; class 8 = 4096.
pub const SH_SLAB_BASE: u32 = 4;
pub const SH_MAX_CLASS: u8 = 24;
/// Largest relocating geometric class (2 KiB). Megakey is still `n ≥ 257` fks.
pub const SH_MAX_SLAB_CLASS: u8 = 7;
/// Slab class whose byte size equals one SH page ([`crate::scripthash_pages::SH_PAGE_SIZE`]).
pub const SH_PAGE_SLAB_CLASS: u8 = 8;

pub type ShHeadKey = [u8; SH_HEAD_KEY_LEN];

/// Truncate full Electrum scripthash (32 B) to head key (16 B).
#[inline]
pub fn head_key_from_full(full: &[u8; 32]) -> ShHeadKey {
    let mut k = [0u8; SH_HEAD_KEY_LEN];
    k.copy_from_slice(&full[0..SH_HEAD_KEY_LEN]);
    k
}

#[inline]
pub const fn slab_cap(class: u8) -> u32 {
    SH_SLAB_BASE << class
}

#[inline]
pub const fn slab_bytes(class: u8) -> u64 {
    16u64 << class
}

/// Durable head value for one scripthash key (pack8, 8 B on disk).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShHeadValue {
    Empty,
    Inline {
        entries: [Fk; SH_INLINE_CAP],
        used: u8,
    },
    /// Geometric slab: `used` fks at `off`, size class `class` (0–6).
    Slab {
        class: u8,
        used: u16,
        off: u64,
    },
    /// Schema 19 megakey: pack8 mode 11 last page; that page holds extent_base/n.
    Extent {
        last_page: u64,
    },
}

impl ShHeadValue {
    pub fn used(&self) -> u32 {
        match self {
            ShHeadValue::Empty => 0,
            ShHeadValue::Inline { used, .. } => u32::from(*used),
            ShHeadValue::Slab { used, .. } => u32::from(*used),
            // Count not stored in head; callers that need n walk pages.
            ShHeadValue::Extent { .. } => u32::MAX,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, ShHeadValue::Empty)
    }

    pub fn is_paged(&self) -> bool {
        matches!(self, ShHeadValue::Extent { .. })
    }

    pub fn is_slab(&self) -> bool {
        matches!(self, ShHeadValue::Slab { .. })
    }

    pub fn inline_one(fk: Fk) -> Self {
        let mut entries = [Fk::NULL; SH_INLINE_CAP];
        entries[0] = fk;
        ShHeadValue::Inline { entries, used: 1 }
    }

    pub fn extent(last_page: u64) -> Self {
        ShHeadValue::Extent { last_page }
    }

    pub fn slab(class: u8, used: u16, off: u64) -> Self {
        ShHeadValue::Slab { class, used, off }
    }

    /// Collect live create fks from an inline value (oldest→newest).
    pub fn inline_entries(&self) -> &[Fk] {
        match self {
            ShHeadValue::Inline { entries, used } => &entries[..*used as usize],
            _ => &[],
        }
    }

    /// All create_tx_fks in this value (inline only; paged needs body read).
    pub fn inline_fks(&self) -> Vec<Fk> {
        self.inline_entries().to_vec()
    }
}

/// Leftover pack8 mode 10 (schema-18 Paged megakey).
pub const INDEX_REFUSE_PAGED_SH: &str = "index refuses pack8 Paged (mode 10) scripthash heads; wipe store/scripthash* then restart (Class A kept; SH rematerializes with --sh-index)";

#[doc(hidden)]
pub const SH8_MODE_SHIFT: u32 = 62;
#[doc(hidden)]
pub const SH8_OFF_MASK: u64 = (1u64 << 40) - 1;
#[doc(hidden)]
pub const SH8_USED_SHIFT: u32 = 40;
#[doc(hidden)]
pub const SH8_USED_MASK: u64 = 0xffff;
#[doc(hidden)]
pub const SH8_CLASS_SHIFT: u32 = 56;
#[doc(hidden)]
pub const SH8_CLASS_MASK: u64 = 0x3f;
#[doc(hidden)]
pub const SH8_PAYLOAD62: u64 = (1u64 << 62) - 1;

/// Schema 18 sealed SH value: 8 B, mode in bits 63–62.
pub fn pack8(v: &ShHeadValue) -> Result<u64, StoreError> {
    match v {
        ShHeadValue::Empty => Ok(0),
        ShHeadValue::Inline { entries, used } if *used == 1 => {
            let fk = entries[0].0;
            if fk > SH8_PAYLOAD62 {
                return Err(StoreError::Corrupt("sh pack8: fk overflow"));
            }
            Ok(fk)
        }
        ShHeadValue::Slab { class, used, off } => {
            if *off > SH8_OFF_MASK || u64::from(*class) > SH8_CLASS_MASK {
                return Err(StoreError::Corrupt("sh pack8: slab field overflow"));
            }
            Ok((1u64 << SH8_MODE_SHIFT)
                | (*off & SH8_OFF_MASK)
                | ((u64::from(*used) & SH8_USED_MASK) << SH8_USED_SHIFT)
                | ((u64::from(*class) & SH8_CLASS_MASK) << SH8_CLASS_SHIFT))
        }
        ShHeadValue::Extent { last_page } => {
            if *last_page > SH8_PAYLOAD62 || *last_page == 0 {
                return Err(StoreError::Corrupt("sh pack8: last_page overflow"));
            }
            Ok((3u64 << SH8_MODE_SHIFT) | *last_page)
        }
        ShHeadValue::Inline { .. } => Err(StoreError::Corrupt("sh pack8: inline cap 1")),
    }
}

/// Inverse of [`pack8`]. Mode 2 (old Paged megakey) refuses.
pub fn pack8_bytes(v: &ShHeadValue) -> Result<[u8; SH_HEAD_VALUE_LEN], StoreError> {
    Ok(pack8(v)?.to_le_bytes())
}

pub fn unpack8_bytes(buf: &[u8; SH_HEAD_VALUE_LEN]) -> Result<ShHeadValue, StoreError> {
    unpack8(u64::from_le_bytes(*buf))
}

pub fn unpack8(w: u64) -> Result<ShHeadValue, StoreError> {
    if w == 0 {
        return Ok(ShHeadValue::Empty);
    }
    match w >> SH8_MODE_SHIFT {
        0 => Ok(ShHeadValue::inline_one(Fk(w & SH8_PAYLOAD62))),
        1 => {
            let off = w & SH8_OFF_MASK;
            let used = ((w >> SH8_USED_SHIFT) & SH8_USED_MASK) as u16;
            let class = ((w >> SH8_CLASS_SHIFT) & SH8_CLASS_MASK) as u8;
            if used as usize <= SH_INLINE_CAP {
                return Err(StoreError::Corrupt("sh unpack8: slab used inline"));
            }
            Ok(ShHeadValue::slab(class, used, off))
        }
        2 => Err(StoreError::Corrupt(INDEX_REFUSE_PAGED_SH)),
        3 => {
            let last = w & SH8_PAYLOAD62;
            if last == 0 {
                return Err(StoreError::Corrupt("sh unpack8: null last_page"));
            }
            Ok(ShHeadValue::extent(last))
        }
        _ => Err(StoreError::Corrupt("sh unpack8: bad mode")),
    }
}

/// Payload region starts at the combined RBT1+SHAL prefix page (offset 4096).
///
/// `file_header_len` is accepted so call sites stay stable; schema 15 does not
/// place SHAL in a second unaligned page after RBT1.
pub fn payload_start(_file_header_len: usize) -> u64 {
    SH_PREFIX_PAGE as u64
}
