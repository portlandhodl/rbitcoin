//! Keyless addressable `tx.head`: `2^BITS` slots × 4 B or 8 B create_fk entries.
//!
//! **Layout:** each entry is LE create_fk (`0` = empty). No key material and
//! **no HAS_NEXT** — probe continues until an empty slot (no Class A deletes).
//! Callers verify identity via Class A body txid on **lookup**.
//!
//! **Insert (sole writer):** probe until the **same fk** is already present
//! (idempotent) or an **empty** slot — `pwrite` the slot (`0 → fk`; no CAS, no
//! per-slot atomics). **No body_txid** on insert (no BIP30 displacement on write).
//! Foreigners and older same-txid creates are skipped blindly; a second Class A
//! row for the same txid lands at the next empty slot (deeper on the probe chain).
//!
//! **Insert batching:** stable-sort by probe **page** then original index
//! (preserves call order within a page for rare same-batch duplicate txids). One
//! page load + multi-insert in RAM + one `pwrite` per dirty page. Visibility is
//! the syscall plus `published_len` Release — not a CPU fence.
//!
//! **Concurrency:** at most **one** thread may insert into a given head segment
//! (archive writer in IBD; single tip accept path after). Multi-writer races are
//! not supported. Capacity growth is **segment roll** (seal + new open), not bits-widen.
//!
//! **Lookup:** walk candidates from the **last occupied** probe slot toward the
//! first, body-verify — so the deepest same-txid create wins (newest under
//! append-deeper insert).
//!
//! **Probe (page-local open address):** high bits of the txid select a **page** of
//! [`PAGE_SLOTS`] (2¹⁰) slots; within the page, **double hashing** with the next
//! key bits (`h1` / odd `h2`). Depth is capped at [`MAX_PROBE`] (= page size).
//! Lookup/insert load **one page** (4 KiB @ 4 B entries) then hop in RAM — one IO
//! for all candidates. Foreign occupants: body mismatch ⇒ continue. Keyless slots
//! cannot Robin-Hood. First insert at depth > [`PROBE_DEPTH_WARN`] requests online
//!
//! **Segment default:** BITS=**25** → **128 MiB** @ 4 B relative create ids per
//! segment (`2^15` pages × 4 KiB). Capacity ends at [`HEAD_LOAD_START`] (0.80);
//! the segmented head seals (fuse8) and opens a new fixed-bits table — no
//! global bits-widen.

use crate::error::StoreError;
use crate::file::{TableFile, TRAILING_FOOTER_LEN};
use crate::hashhead::HeadScale;
use rbitcoin_primitives::{Fk, TableKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// In-page slot index width (1024 slots / page @ any entry width).
pub const PAGE_SLOT_BITS: u32 = 10;
/// Slots per page (`2^PAGE_SLOT_BITS`).
pub const PAGE_SLOTS: u64 = 1 << PAGE_SLOT_BITS;

/// Hard cap — full in-page exploration (never leave the page).
pub const MAX_PROBE: u32 = 1024;
/// Occupied `(depth, fk)` stored inline in [`ProbeCands`] before a spill `Vec`.
pub const PROBE_CANDS_INLINE: usize = 8;

/// Max bytes of one head page load (1024 × 8 B). 4 B entries use half.
pub const PROBE_REGION_BYTES: usize = (PAGE_SLOTS as usize) * 8;

/// Concurrent probe page preads on a held plan TLS session (matches ring depth).
/// One buffer per in-flight slot — hop keys on CQE, then reuse.
#[doc(hidden)]
pub const PROBE_PAGES_IN_FLIGHT: usize = crate::uring_session::DEFAULT_ENTRIES as usize;

/// Inserts that needed probe depth **> [`PROBE_DEPTH_WARN`]** (warning band).
/// Cumulative counter for lagging/retry logs; WARN only once at first event.
#[doc(hidden)]
pub static PROBE_INSERT_DEPTH_WARN_COUNT: AtomicU64 = AtomicU64::new(0);

/// Inserts that exhausted [`MAX_PROBE`] (archiver sleeps until resize completes).
/// Counter only — the retry loop owns operator-facing logs.
#[doc(hidden)]
pub static PROBE_INSERT_EXHAUSTED: AtomicU64 = AtomicU64::new(0);

/// Depth threshold above which inserts count as “deep” for ops visibility and
/// trigger an early online resize request (first event only).
pub const PROBE_DEPTH_WARN: u32 = 128;

/// Segment address width (2^25 slots × 4 B = 128 MiB). Fixed per segment; roll to grow.
pub const MAINNET_BITS: u32 = 25;
/// Tiny / unit-test width.
pub const TINY_BITS: u32 = 16;
/// Maximum supported address width (probe + create).
pub const MAX_BITS: u32 = 34;
/// Minimum supported address width.
pub const MIN_BITS: u32 = 8;

/// Start sequential rebuild when `txs.count() / slots >=` this.
pub const HEAD_LOAD_START: f64 = 0.80;

/// True when `err` is the sole-writer open-address insert failure (table full
/// along the probe chain — should not happen when segment roll respects 80% load).
#[inline]
pub fn is_probe_exhausted_error(err: &StoreError) -> bool {
    matches!(
        err,
        StoreError::Corrupt(m) if *m == "address head probe exhausted on insert"
    )
}

/// `NetError::Consensus` display of store `Corrupt` (not a block rule).
pub fn is_store_corrupt_display(s: &str) -> bool {
    const PROBE: &str = "address head probe exhausted on insert";
    let probe = StoreError::Corrupt(PROBE);
    if s == probe.to_string() {
        return is_probe_exhausted_error(&probe);
    }
    s.starts_with("corrupt record:")
}

#[inline]
#[doc(hidden)]
pub fn note_probe_depth_on_insert(depth: u32) {
    if depth <= PROBE_DEPTH_WARN {
        return;
    }
    let n = PROBE_INSERT_DEPTH_WARN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 {
        rbitcoin_log::warn!(
            "store: tx.head insert probe depth>{PROBE_DEPTH_WARN} (first depth={depth}; \
             segment may be overfull — roll policy should prevent this)"
        );
    }
}

#[inline]
#[doc(hidden)]
pub fn note_probe_exhausted() {
    PROBE_INSERT_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
}

#[doc(hidden)]
pub const META_MAGIC: &[u8; 4] = b"THM1";
/// `5` = page-local double-hash; layout (bits/entry/generation) lives in the
/// **trailing footer** next to store magic (no `tx.head.meta` sidecar). Slots at
/// offset 0 remain page-aligned. Older versions refused → open recreates + rebuilds.
#[doc(hidden)]
pub const META_VERSION: u16 = 5;

/// On-disk / in-memory address-head geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadLayout {
    pub bits: u32,
    /// 4 (create_fk as u32) or 8 (create_fk as u64).
    pub entry_bytes: u8,
}

impl HeadLayout {
    pub fn new(bits: u32) -> Result<Self, StoreError> {
        if !(MIN_BITS..=MAX_BITS).contains(&bits) {
            return Err(StoreError::Corrupt("address head bits out of range"));
        }
        Ok(Self {
            bits,
            entry_bytes: entry_bytes_for_bits(bits),
        })
    }

    pub fn with_entry_bytes(bits: u32, entry_bytes: u8) -> Result<Self, StoreError> {
        if !(MIN_BITS..=MAX_BITS).contains(&bits) {
            return Err(StoreError::Corrupt("address head bits out of range"));
        }
        if entry_bytes != 4 && entry_bytes != 8 {
            return Err(StoreError::Corrupt(
                "address head entry_bytes must be 4 or 8",
            ));
        }
        // BITS ≥ 33 requires 8 B (u32 fk space insufficient at 0.80 load).
        if bits >= 33 && entry_bytes != 8 {
            return Err(StoreError::Corrupt(
                "address head bits>=33 requires 8-byte entries",
            ));
        }
        Ok(Self { bits, entry_bytes })
    }

    pub fn slots(&self) -> u64 {
        1u64 << self.bits
    }

    pub fn entry_size(&self) -> u64 {
        u64::from(self.entry_bytes)
    }

    pub fn body_bytes(&self) -> u64 {
        self.slots() * self.entry_size()
    }
}

/// Entry width policy: 8 B starting at BITS 33 (capacity exceeds u32 create_fk).
#[inline]
pub fn entry_bytes_for_bits(bits: u32) -> u8 {
    if bits >= 33 {
        8
    } else {
        4
    }
}

/// First 8 bytes of txid as big-endian u64 (bit stream for page / h1).
#[inline]
#[doc(hidden)]
pub fn key_be_u64(txid: &[u8; 32]) -> u64 {
    u64::from_be_bytes([
        txid[0], txid[1], txid[2], txid[3], txid[4], txid[5], txid[6], txid[7],
    ])
}

/// Page index from the **top** `(bits - 10)` bits of the txid (0 if bits ≤ 10).
#[inline]
pub fn page_index(txid: &[u8; 32], bits: u32) -> u64 {
    debug_assert!((MIN_BITS..=MAX_BITS).contains(&bits));
    if bits <= PAGE_SLOT_BITS {
        return 0;
    }
    let page_bits = bits - PAGE_SLOT_BITS;
    key_be_u64(txid) >> (64 - page_bits)
}

/// In-page h1: next 10 bits after the page-select field (mod 2^10).
#[inline]
pub fn h1_in_page(txid: &[u8; 32], bits: u32) -> u64 {
    let v = key_be_u64(txid);
    if bits <= PAGE_SLOT_BITS {
        return (v >> (64 - bits)) & ((1u64 << bits) - 1);
    }
    let page_bits = bits - PAGE_SLOT_BITS;
    (v >> (64 - page_bits - PAGE_SLOT_BITS)) & (PAGE_SLOTS - 1)
}

/// In-page odd step from a second window of the txid (1,3,… within the page).
#[inline]
pub fn h2_in_page(txid: &[u8; 32], bits: u32) -> u64 {
    let v = u64::from_be_bytes([
        txid[4], txid[5], txid[6], txid[7], txid[8], txid[9], txid[10], txid[11],
    ]);
    let mask = if bits <= PAGE_SLOT_BITS {
        (1u64 << bits) - 1
    } else {
        PAGE_SLOTS - 1
    };
    (v | 1) & mask
}

/// Global slot at probe depth `d`: page from high bits, double-hash within page.
///
/// `slot = (page << 10) | ((h1 + d·h2) mod 1024)` when `bits > 10`.
#[inline]
pub fn probe_index(txid: &[u8; 32], d: u32, bits: u32) -> u64 {
    debug_assert!((MIN_BITS..=MAX_BITS).contains(&bits));
    let h1 = h1_in_page(txid, bits);
    let h2 = h2_in_page(txid, bits);
    if bits <= PAGE_SLOT_BITS {
        let mask = (1u64 << bits) - 1;
        return h1.wrapping_add(u64::from(d).wrapping_mul(h2)) & mask;
    }
    let page = page_index(txid, bits);
    let local = h1.wrapping_add(u64::from(d).wrapping_mul(h2)) & (PAGE_SLOTS - 1);
    (page << PAGE_SLOT_BITS) | local
}

/// Number of slots in the probe page for this table width.
#[inline]
pub fn page_slot_count(bits: u32) -> u64 {
    if bits <= PAGE_SLOT_BITS {
        1u64 << bits
    } else {
        PAGE_SLOTS
    }
}

/// Slot file offset: data starts at **0** (trailing magic); page-aligned probes.
#[inline]
pub fn entry_file_off(slot: u64, entry_bytes: u8) -> u64 {
    slot * u64::from(entry_bytes)
}

/// Decode one LE create_fk from a page buffer at local slot index.
#[inline]
pub fn entry_from_page_buf(buf: &[u8], local: u64, entry_bytes: u8) -> Option<u64> {
    let es = entry_bytes as usize;
    let off = (local as usize).checked_mul(es)?;
    if off + es > buf.len() {
        return None;
    }
    Some(match entry_bytes {
        4 => u64::from(u32::from_le_bytes(buf[off..off + 4].try_into().ok()?)),
        8 => u64::from_le_bytes(buf[off..off + 8].try_into().ok()?),
        _ => return None,
    })
}

/// Two loads + hop of one probe page (leftover-miss dump).
pub struct PageHopDump {
    pub scan: ProbeRegionScan,
    pub hop_equal_second: bool,
    pub occupied: u32,
}

/// Occupied `(depth, encoded_fk)` from one hop. Inline for the common 1–2
/// cand path; heap only past [`PROBE_CANDS_INLINE`].
#[derive(Debug, Clone)]
pub struct ProbeCands {
    pub inline: [(u32, u64); PROBE_CANDS_INLINE],
    pub n: u8,
    pub spill: Vec<(u32, u64)>,
}

impl Default for ProbeCands {
    fn default() -> Self {
        Self {
            inline: [(0, 0); PROBE_CANDS_INLINE],
            n: 0,
            spill: Vec::new(),
        }
    }
}

impl ProbeCands {
    #[inline]
    pub fn len(&self) -> usize {
        (self.n as usize).saturating_add(self.spill.len())
    }

    #[inline]
    pub fn push(&mut self, depth: u32, fk: u64) {
        let i = self.n as usize;
        if i < PROBE_CANDS_INLINE {
            self.inline[i] = (depth, fk);
            self.n = (i + 1) as u8;
            return;
        }
        self.spill.push((depth, fk));
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &(u32, u64)> {
        self.inline[..self.n as usize]
            .iter()
            .chain(self.spill.iter())
    }
}

impl PartialEq for ProbeCands {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

/// Result of hopping through one loaded page.
#[derive(Debug, Clone)]
pub struct ProbeRegionScan {
    /// Occupied create_fks with absolute probe depth (home = 0).
    pub cands: ProbeCands,
    /// Saw an empty slot (probe chain ends).
    pub hit_empty: bool,
    /// Depth of empty slot if [`Self::hit_empty`], else depths explored without empty.
    pub depth_end: u32,
    /// Local slot index of empty (valid if hit_empty).
    pub empty_local: u64,
}

/// Double-hash hop through a loaded page buffer.
pub fn hop_scan_page(
    page_buf: &[u8],
    entry_bytes: u8,
    h1: u64,
    h2: u64,
    page_slots: u64,
    max_probe: u32,
) -> ProbeRegionScan {
    let mask = page_slots - 1;
    let max_d = max_probe.min(page_slots as u32);
    let mut cands = ProbeCands::default();
    for d in 0..max_d {
        let local = h1.wrapping_add(u64::from(d).wrapping_mul(h2)) & mask;
        let Some(e) = entry_from_page_buf(page_buf, local, entry_bytes) else {
            break;
        };
        if e == 0 {
            return ProbeRegionScan {
                cands,
                hit_empty: true,
                depth_end: d,
                empty_local: local,
            };
        }
        cands.push(d, e);
    }
    ProbeRegionScan {
        cands,
        hit_empty: false,
        depth_end: max_d,
        empty_local: 0,
    }
}

/// Global first slot of the probe page that holds `txid`.
#[inline]
pub fn page_base_for_txid(txid: &[u8; 32], bits: u32) -> u64 {
    if bits <= PAGE_SLOT_BITS {
        0
    } else {
        page_index(txid, bits) << PAGE_SLOT_BITS
    }
}

/// Outcome of [`insert_fk_into_page_buf`] (in-buffer only; no file IO).
#[derive(Debug, Clone, Copy)]
pub struct InsertPageOutcome {
    /// Wrote a new empty→fk slot (false if `new_fk` already on the chain).
    pub wrote_new: bool,
    /// Probe depth of the empty slot written (or 0 if idempotent).
    pub depth: u32,
}

/// Insert `new_fk` into a **loaded** probe page buffer (online resize RMW path).
///
/// Idempotent if `new_fk` is already present. Does not touch the file or
/// occupied count — caller applies the buffer via pwrite / store.
pub fn insert_fk_into_page_buf(
    page_buf: &mut [u8],
    page_base: u64,
    bits: u32,
    entry_bytes: u8,
    txid: &[u8; 32],
    new_fk: Fk,
) -> Result<InsertPageOutcome, StoreError> {
    let _ = page_base;
    if new_fk.is_null() {
        return Err(StoreError::InvalidFk);
    }
    if entry_bytes == 4 && new_fk.0 > u64::from(u32::MAX) {
        return Err(StoreError::InvalidFk);
    }
    let new_u = new_fk.0;
    let es = entry_bytes as usize;
    let nslots = (page_buf.len() / es) as u64;
    let h1 = h1_in_page(txid, bits);
    let h2 = h2_in_page(txid, bits);
    let scan = hop_scan_page(page_buf, entry_bytes, h1, h2, nslots, MAX_PROBE);
    for &(_d, e) in scan.cands.iter() {
        if e == new_u {
            return Ok(InsertPageOutcome {
                wrote_new: false,
                depth: 0,
            });
        }
    }
    if !scan.hit_empty {
        note_probe_exhausted();
        return Err(StoreError::Corrupt(
            "address head probe exhausted on insert",
        ));
    }
    store_entry_in_page_buf(page_buf, scan.empty_local, entry_bytes, new_u)?;
    Ok(InsertPageOutcome {
        wrote_new: true,
        depth: scan.depth_end,
    })
}

/// Write LE create_fk into a page buffer at local slot index.
#[doc(hidden)]
pub fn store_entry_in_page_buf(
    page_buf: &mut [u8],
    local: u64,
    entry_bytes: u8,
    new: u64,
) -> Result<(), StoreError> {
    let es = entry_bytes as usize;
    let off = (local as usize)
        .checked_mul(es)
        .ok_or(StoreError::Corrupt("page buf slot overflow"))?;
    if off + es > page_buf.len() {
        return Err(StoreError::Corrupt("page buf slot out of range"));
    }
    match entry_bytes {
        4 => {
            if new > u64::from(u32::MAX) {
                return Err(StoreError::InvalidFk);
            }
            page_buf[off..off + 4].copy_from_slice(&(new as u32).to_le_bytes());
        }
        8 => {
            page_buf[off..off + 8].copy_from_slice(&new.to_le_bytes());
        }
        _ => return Err(StoreError::Corrupt("address head entry_bytes")),
    }
    Ok(())
}

/// Bytes to pread for the full probe page of `txid`.
///
/// Resolve address width for new creates (`scale`, then `RBITCOIN_TX_HEAD_BITS`).
pub fn bits_for_scale(scale: HeadScale) -> u32 {
    if let Ok(s) = std::env::var("RBITCOIN_TX_HEAD_BITS") {
        if let Ok(n) = s.parse::<u32>() {
            if (MIN_BITS..=MAX_BITS).contains(&n) {
                return n;
            }
            rbitcoin_log::warn!(
                "store: RBITCOIN_TX_HEAD_BITS={s:?} out of {MIN_BITS}..={MAX_BITS}, using scale default"
            );
        }
    }
    match scale {
        HeadScale::Tiny => TINY_BITS,
        HeadScale::Mainnet => MAINNET_BITS,
    }
}

pub fn default_layout(scale: HeadScale) -> HeadLayout {
    HeadLayout::new(bits_for_scale(scale)).expect("default bits in range")
}

/// Legacy sidecar path (`tx.head.meta`) — only for best-effort cleanup of old datadirs.
#[doc(hidden)]
pub fn meta_path(head_path: &Path) -> PathBuf {
    let mut p = head_path.as_os_str().to_os_string();
    p.push(".meta");
    PathBuf::from(p)
}

/// Drop leftover sidecar meta from pre-v5 layouts (layout is now in the footer).
pub fn remove_legacy_meta_sidecar(head_path: &Path) {
    let _ = std::fs::remove_file(meta_path(head_path));
}

/// Pack layout + generation into the 16-byte trailing-footer extension.
#[inline]
pub fn encode_layout_ext(layout: HeadLayout, generation: u64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(META_MAGIC);
    buf[4..6].copy_from_slice(&META_VERSION.to_le_bytes());
    buf[6] = layout.bits as u8;
    buf[7] = layout.entry_bytes;
    buf[8..16].copy_from_slice(&generation.to_le_bytes());
    buf
}

/// Decode layout extension from the trailing footer (or fail for rebuild).
pub fn decode_layout_ext(ext: &[u8; 16]) -> Result<(HeadLayout, u64), StoreError> {
    if &ext[0..4] != META_MAGIC {
        return Err(StoreError::Corrupt(
            "tx.head footer layout magic (rebuild tx.head)",
        ));
    }
    let ver = u16::from_le_bytes([ext[4], ext[5]]);
    if ver != META_VERSION {
        return Err(StoreError::Corrupt(
            "tx.head footer layout version (footer-embedded meta; rebuild tx.head)",
        ));
    }
    let bits = u32::from(ext[6]);
    let entry_bytes = ext[7];
    let generation = u64::from_le_bytes(ext[8..16].try_into().unwrap());
    let layout = HeadLayout::with_entry_bytes(bits, entry_bytes)?;
    Ok((layout, generation))
}

/// Fixed-width keyless txid → dense create_fk table.
pub struct AddressHead {
    pub file: TableFile,
    pub layout: HeadLayout,
}

impl AddressHead {
    pub fn create_with_layout(
        path: impl Into<PathBuf>,
        layout: HeadLayout,
    ) -> Result<Self, StoreError> {
        // Trailing footer: slots at offset 0 so each 4 KiB probe page is OS-aligned.
        // Layout (bits/entry/generation) lives in the footer extension — no sidecar.
        let path = path.into();
        if path.is_dir() {
            return Err(StoreError::Corrupt(
                "tx.head is a directory (legacy shards); wipe datadir for address head",
            ));
        }
        let mut file = TableFile::create_trailing_header(&path, TableKind::HashHead)?;
        let body_bytes = layout.body_bytes();
        let need = body_bytes + TRAILING_FOOTER_LEN as u64;
        file.ensure_capacity(need)?;
        // Layout ext must be set before set_logical_len so the footer at EOF
        // carries bits/generation (no sidecar).
        file.set_trailing_ext(encode_layout_ext(layout, 0))?;
        file.set_logical_len(need)?;
        file.zero_range(0, body_bytes)?;
        remove_legacy_meta_sidecar(&path);
        Ok(Self { file, layout })
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if path.is_dir() {
            return Err(StoreError::Corrupt(
                "tx.head is a directory (legacy shards); wipe datadir for address head",
            ));
        }
        // Layout is in the trailing footer (v5). Sidecar-only or older footers fail
        // here → TxTable recreates + rebuilds from Class A.
        let (file, ext) = TableFile::open_trailing_header_from_end(&path, TableKind::HashHead)?;
        let (layout, _generation) = decode_layout_ext(&ext)?;
        let expect_body = layout.body_bytes();
        let body = file.data_len();
        if body == 0 {
            return Err(StoreError::Corrupt("address head size"));
        }
        if body != expect_body {
            return Err(StoreError::Corrupt(
                "address head size mismatch vs footer layout",
            ));
        }
        remove_legacy_meta_sidecar(&path);
        Ok(Self { file, layout })
    }

    pub fn bits(&self) -> u32 {
        self.layout.bits
    }

    pub fn entry_bytes(&self) -> u8 {
        self.layout.entry_bytes
    }

    #[inline]
    pub fn entry_off(&self, slot: u64) -> u64 {
        entry_file_off(slot, self.layout.entry_bytes)
    }

    /// Load a full probe page starting at global `page_base` into `buf`.
    ///
    /// **One** bulk pread of up to `n_slots × entry_bytes` (4 KiB @ 4 B / 1024
    /// slots). Caps to the slot data region (excludes trailing footer).
    ///
    /// Returns bytes filled (multiple of entry size). Callers must pass the
    /// corresponding slot count into [`hop_scan_page`] (`bytes / entry_bytes`).
    #[doc(hidden)]
    pub fn load_page_slots(
        &self,
        page_base: u64,
        n_slots: u64,
        buf: &mut [u8],
    ) -> Result<usize, StoreError> {
        let es = self.layout.entry_bytes as usize;
        if es != 4 && es != 8 {
            return Err(StoreError::Corrupt("address head entry_bytes"));
        }
        let n = n_slots as usize;
        let mut need = n.saturating_mul(es);
        if need > buf.len() {
            return Err(StoreError::Corrupt("probe page buffer short"));
        }
        let off = self.entry_off(page_base);
        // Never read into the trailing footer — only the create_fk slot array.
        let data_end = self.file.data_len();
        let avail = data_end.saturating_sub(off) as usize;
        need = need.min(avail);
        need = (need / es) * es;
        if need == 0 {
            return Ok(0);
        }
        {
            use crate::bulk_io::{self, ReadOp};
            let fd = self.file.read_fd();
            let slice = &mut buf[..need];
            let mut ops = [ReadOp {
                fd,
                offset: off,
                buf: slice,
                result: i32::MIN,
            }];
            bulk_io::pread_batch(&mut ops);
            if ops[0].result < 0 {
                return Err(StoreError::io(
                    self.file.path(),
                    std::io::Error::from_raw_os_error(-ops[0].result),
                ));
            }
            if (ops[0].result as usize) < need {
                self.file.read_at(off, &mut buf[..need])?;
            }
        }
        Ok(need)
    }

    #[doc(hidden)]
    pub fn load_page_slots_read_at(
        &self,
        page_base: u64,
        n_slots: u64,
        buf: &mut [u8],
    ) -> Result<usize, StoreError> {
        let es = self.layout.entry_bytes as usize;
        if es != 4 && es != 8 {
            return Err(StoreError::Corrupt("address head entry_bytes"));
        }
        let mut need = (n_slots as usize).saturating_mul(es);
        if need > buf.len() {
            return Err(StoreError::Corrupt("probe page buffer short"));
        }
        let off = self.entry_off(page_base);
        let avail = self.file.data_len().saturating_sub(off) as usize;
        need = (need.min(avail) / es) * es;
        if need == 0 {
            return Ok(0);
        }
        self.file.read_at(off, &mut buf[..need])?;
        Ok(need)
    }

    /// Load the probe page twice and hop. Leftover-miss dump only.
    pub fn dump_page_hop(&self, mixed: &[u8; 32]) -> Result<PageHopDump, StoreError> {
        let bits = self.layout.bits;
        let page_base = page_base_for_txid(mixed, bits);
        let page_slots = page_slot_count(bits);
        let es = self.layout.entry_bytes;
        let mut buf = [0u8; PROBE_REGION_BYTES];
        let n = self.load_page_slots_read_at(page_base, page_slots, &mut buf)?;
        let es_u = es as usize;
        if n < es_u {
            return Ok(PageHopDump {
                scan: hop_scan_page(&[], es, 0, 1, 0, MAX_PROBE),
                hop_equal_second: true,
                occupied: 0,
            });
        }
        let nslots = (n / es_u) as u64;
        let h1 = h1_in_page(mixed, bits);
        let h2 = h2_in_page(mixed, bits);
        let scan = hop_scan_page(&buf[..n], es, h1, h2, nslots, MAX_PROBE);
        let mut occupied = 0u32;
        for i in 0..nslots {
            if entry_from_page_buf(&buf[..n], i, es).unwrap_or(0) != 0 {
                occupied = occupied.saturating_add(1);
            }
        }
        let mut buf2 = [0u8; PROBE_REGION_BYTES];
        let n2 = self.load_page_slots_read_at(page_base, page_slots, &mut buf2)?;
        let nslots2 = (n2 / es_u) as u64;
        let scan2 = hop_scan_page(&buf2[..n2.min(buf2.len())], es, h1, h2, nslots2, MAX_PROBE);
        Ok(PageHopDump {
            hop_equal_second: scan.cands == scan2.cands && scan.hit_empty == scan2.hit_empty,
            scan,
            occupied,
        })
    }

    /// Sort `entries` in place by probe page and insert. No extra pair copy.
    pub fn insert_many_in_place(&self, entries: &mut [([u8; 32], Fk)]) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let bits = self.layout.bits;
        let es = self.layout.entry_bytes;
        let page_slots = page_slot_count(bits);
        let es_u = es as usize;

        entries.sort_by_key(|a| page_base_for_txid(&a.0, bits));

        let mut buf = [0u8; PROBE_REGION_BYTES];
        let mut i = 0;
        while i < entries.len() {
            let page_base = page_base_for_txid(&entries[i].0, bits);
            let mut j = i + 1;
            while j < entries.len() && page_base_for_txid(&entries[j].0, bits) == page_base {
                j += 1;
            }

            let n = self.load_page_slots(page_base, page_slots, &mut buf)?;
            if n < es_u {
                note_probe_exhausted();
                return Err(StoreError::Corrupt("address head probe page empty"));
            }

            let mut dirty = false;
            for &(ref txid, fk) in &entries[i..j] {
                let outcome =
                    insert_fk_into_page_buf(&mut buf[..n], page_base, bits, es, txid, fk)?;
                if outcome.wrote_new {
                    note_probe_depth_on_insert(outcome.depth);
                    dirty = true;
                }
            }
            if dirty {
                let off = self.entry_off(page_base);
                self.file.write_at(off, &buf[..n])?;
            }
            i = j;
        }

        Ok(())
    }

    /// Probe with a shared [`crate::IoCtx`] (held session or standalone).
    pub fn probe_fks_batch_ctx(
        &self,
        txids: &[[u8; 32]],
        ctx: &mut crate::IoCtx<'_>,
    ) -> Result<Vec<Vec<Fk>>, StoreError> {
        self.probe_fks_batch_inner(txids, ctx)
    }

    #[doc(hidden)]
    pub fn probe_fks_batch_inner(
        &self,
        txids: &[[u8; 32]],
        ctx: &mut crate::IoCtx<'_>,
    ) -> Result<Vec<Vec<Fk>>, StoreError> {
        let n_keys = txids.len();
        if n_keys == 0 {
            return Ok(Vec::new());
        }
        let bits = self.layout.bits;
        let es = self.layout.entry_bytes;
        let page_slots = page_slot_count(bits);
        let es_u = es as usize;

        let mut order: Vec<(u64, usize)> = txids
            .iter()
            .enumerate()
            .map(|(i, t)| (page_base_for_txid(t, bits), i))
            .collect();
        order.sort_unstable_by_key(|&(p, i)| (p, i));

        let mut page_bases: Vec<u64> = Vec::new();
        let mut page_ranges: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < order.len() {
            let page_base = order[i].0;
            let mut j = i + 1;
            while j < order.len() && order[j].0 == page_base {
                j += 1;
            }
            page_bases.push(page_base);
            page_ranges.push((i, j));
            i = j;
        }

        let mut out = vec![Vec::new(); n_keys];

        match ctx.session() {
            Some(session) => self.probe_pages_streaming_on_session(
                session,
                txids,
                bits,
                es,
                es_u,
                page_slots,
                &order,
                &page_bases,
                &page_ranges,
                &mut out,
            )?,
            None => {
                let mut buf = [0u8; PROBE_REGION_BYTES];
                for (pi, &page_base) in page_bases.iter().enumerate() {
                    let n = self.load_page_slots(page_base, page_slots, &mut buf)?;
                    if n < es_u {
                        continue;
                    }
                    let nslots = (n / es_u) as u64;
                    let (lo, hi) = page_ranges[pi];
                    for &(_, orig) in &order[lo..hi] {
                        let txid = &txids[orig];
                        let h1p = h1_in_page(txid, bits);
                        let h2p = h2_in_page(txid, bits);
                        let scan = hop_scan_page(&buf[..n], es, h1p, h2p, nslots, MAX_PROBE);
                        out[orig] = scan.cands.iter().map(|&(_, e)| Fk(e)).collect();
                    }
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)] // IO/session args stay unbundled
    /// Stream unique probe pages on a held session: ≤[`PROBE_PAGES_IN_FLIGHT`]
    /// page buffers, fill ring, hop keys on CQE, reuse slot.
    #[doc(hidden)]
    pub fn probe_pages_streaming_on_session(
        &self,
        session: &mut crate::uring_session::UringSession,
        txids: &[[u8; 32]],
        bits: u32,
        es: u8,
        es_u: usize,
        page_slots: u64,
        order: &[(u64, usize)],
        page_bases: &[u64],
        page_ranges: &[(usize, usize)],
        out: &mut [Vec<Fk>],
    ) -> Result<(), StoreError> {
        let n_pages = page_bases.len();
        if n_pages == 0 {
            return Ok(());
        }

        let fd = self.file.read_fd();
        let path = self.file.path();

        // Fixed pool — never one buffer per unique page in the stamp.
        let pool_n = PROBE_PAGES_IN_FLIGHT.min(n_pages).max(1);
        let mut bufs: Vec<Vec<u8>> = (0..pool_n).map(|_| vec![0u8; PROBE_REGION_BYTES]).collect();
        let mut slot_page: Vec<Option<usize>> = vec![None; pool_n];
        let mut free_slots: Vec<usize> = (0..pool_n).collect();
        let mut next_page = 0usize;
        let mut in_flight = 0usize;
        session.begin_batch()?;
        let epoch = session.epoch();

        // On error, drain while bufs still live (SQE destinations).
        let run = (|| -> Result<(), StoreError> {
            loop {
                while next_page < n_pages
                    && !free_slots.is_empty()
                    && session.free_sq() > 0
                    && in_flight < pool_n
                {
                    let slot = free_slots.pop().unwrap();
                    let pi = next_page;
                    next_page += 1;
                    let page_base = page_bases[pi];
                    let need = self.probe_page_need(page_base, page_slots);
                    if need == 0 {
                        free_slots.push(slot);
                        continue;
                    }
                    let off = self.entry_off(page_base);
                    let buf = &mut bufs[slot][..need];
                    buf.fill(0);
                    let ud = crate::uring_session::pack_ud(
                        crate::uring_session::KIND_PROBE,
                        epoch,
                        slot as u32,
                    );
                    // SAFETY: `bufs[slot]` lives until this probe wave drains.
                    unsafe { session.push_pread(fd, off, buf, ud) }?;
                    slot_page[slot] = Some(pi);
                    in_flight += 1;
                }
                session.sync_submission();
                let _ = session.submit();

                if in_flight == 0 {
                    break;
                }

                let cqes = session.harvest_ready()?;
                if cqes.is_empty() {
                    session.submit_and_wait_one()?;
                    continue;
                }

                for (ud, res) in cqes {
                    let (kind, ep, slot) = crate::uring_session::unpack_ud(ud);
                    let slot = slot as usize;
                    if kind != crate::uring_session::KIND_PROBE
                        || ep != epoch
                        || slot >= pool_n
                        || slot_page[slot].is_none()
                    {
                        session.poison();
                        return Err(StoreError::Corrupt("invariant: io_uring leftover cqe"));
                    }
                    in_flight = in_flight.saturating_sub(1);
                    let pi = slot_page[slot].take().unwrap();
                    let page_base = page_bases[pi];
                    let need = self.probe_page_need(page_base, page_slots);

                    if res < 0 {
                        return Err(StoreError::io(
                            path,
                            std::io::Error::from_raw_os_error(-res),
                        ));
                    }

                    let mut n = res as usize;
                    if n < need {
                        // Short — complete via libc pread (no nested TLS).
                        self.file
                            .read_at(self.entry_off(page_base), &mut bufs[slot][..need])?;
                        n = need;
                    }
                    n = n.min(need);

                    if n >= es_u {
                        let nslots = (n / es_u) as u64;
                        let (lo, hi) = page_ranges[pi];
                        let page = &bufs[slot][..n];
                        for &(_, orig) in &order[lo..hi] {
                            let txid = &txids[orig];
                            let h1p = h1_in_page(txid, bits);
                            let h2p = h2_in_page(txid, bits);
                            let scan = hop_scan_page(page, es, h1p, h2p, nslots, MAX_PROBE);
                            out[orig] = scan.cands.iter().map(|&(_, e)| Fk(e)).collect();
                        }
                    }
                    free_slots.push(slot);
                }
            }
            Ok(())
        })();

        session.drain_all()?;
        run
    }

    /// Byte length of a probe page load (slot array only, not footer).
    #[doc(hidden)]
    pub fn probe_page_need(&self, page_base: u64, n_slots: u64) -> usize {
        let es = self.layout.entry_bytes as usize;
        let mut need = (n_slots as usize).saturating_mul(es);
        need = need.min(PROBE_REGION_BYTES);
        let off = self.entry_off(page_base);
        let data_end = self.file.data_len();
        let avail = data_end.saturating_sub(off) as usize;
        need = need.min(avail);
        (need / es) * es
    }

    pub fn flush(&self) -> Result<(), StoreError> {
        self.file.flush()
    }

    pub fn flush_async(&self) -> Result<(), StoreError> {
        self.file.flush_async()
    }
}
