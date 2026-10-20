//! **loc → body** pipeline for confirm load (`txout` / `seqsigwit` stems).
//!
//! Ranges from [`crate::create_loc::CreateLoc`] / [`crate::delta_loc::DeltaLoc`]
//! (FdOnly). Body backend from
//! [`crate::io_backend::read_io_backend`] (global
//! `RBITCOIN_IO`): **uring** or **pread**. Class A body is also FdOnly.
//!
//! **Concurrency:** read-only on published ranges; prep + confirm-load may run
//! concurrent waves (each thread's bulk_io TL ring). Caller owns job buffers
//! until the call returns.

use crate::bulk_io::{self, ReadOp};
use crate::error::StoreError;
use crate::io_backend::{self, ReadIoBackend};
use crate::var_table::VarTable;
use rbitcoin_primitives::Fk;

/// What body bytes to fetch after the range is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    /// Full record (txout or seqsigwit).
    Full,
    /// `txout` outs / pin: starting OS page, or full span if need is likely to spill.
    Outs,
}

/// One cold Class A layout + body job.
///
/// **Input:** set `id` (1-based Class A fk) and optionally `range` when known
/// (sticky / FIFO). **Output:** `range`, `body` (on success), `ok`.
#[derive(Debug)]
pub struct IdxBodyJob {
    /// 1-based create id (`Fk.0` when non-null).
    pub id: u64,
    /// Known `(body_off, body_len)` skips loc fill when `n_out` is set.
    pub range: Option<(u64, u64)>,
    /// Body bytes (mode-sized) when `ok`.
    pub body: Vec<u8>,
    /// True when body pread completed for the expected length.
    pub ok: bool,
    /// Sparse Outs need (sorted unique). Empty = all outs (SH / ensure).
    pub need_vouts: Vec<u32>,
    /// Loc `n_out` (≥ 1) for txout decode. Unused for seqsigwit/full.
    pub n_out: u32,
}

impl IdxBodyJob {
    pub fn new(id: u64, range: Option<(u64, u64)>) -> Self {
        Self {
            id,
            range,
            body: Vec::new(),
            ok: false,
            need_vouts: Vec::new(),
            n_out: 0,
        }
    }

    pub fn from_fk(fk: Fk, range: Option<(u64, u64)>) -> Option<Self> {
        let id = fk.get()?;
        if id == 0 {
            return None;
        }
        Some(Self::new(id, range))
    }
}

/// Body-wave IO counts for `ibd: perf` (extend / page-grouped SQEs).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IdxBodyIoStats {
    /// Second-wave Outs jobs that pread the remainder of the idx span.
    pub extend_n: u64,
    /// Body `ReadOp`s submitted (first wave + extend), after page grouping.
    pub body_sqe_n: u64,
    /// First-wave Outs jobs that pread the full idx span (spill guess).
    pub guess_full_n: u64,
}

#[doc(hidden)]
pub const BODY_OS_PAGE: u64 = crate::tx_table::BODY_PAGE_SIZE;
/// Cap a page-straddle coalesce at two OS pages. Do not chain into SH-sized reads.
#[doc(hidden)]
pub const BODY_GROUP_MAX_PAGES: u64 = 2;
/// Also merge a run whose hole is at most this and whose span stays within [`BODY_GROUP_MAX_SPAN`].
#[doc(hidden)]
pub const BODY_GROUP_MAX_HOLE: u64 = 64 * 1024;
#[doc(hidden)]
pub const BODY_GROUP_MAX_SPAN: u64 = 256 * 1024;
#[doc(hidden)]
pub const OUTS_META_GUESS: u64 = 4;
/// Kind + 5-byte amount uleb + P2TR payload (not RAW/OP_RETURN fat scripts).
#[doc(hidden)]
pub const OUTS_GUESS_PER_VOUT: u64 = 38;

/// First-wave Outs pread length.
///
/// Records are 8-aligned, not OS-page aligned. `room` is bytes from `off` to
/// the next OS page. Empty need uses the idx span; sparse need uses
/// `4 + (max_vout+1)*38` (LAYOUT17 typical meta + kind + 5-byte amount + P2TR).
/// If that estimate is likely to spill, read the full span; otherwise only the
/// remainder of this page.
pub fn outs_first_wave_len(off: u64, full_len: u64, need_vouts: &[u32]) -> u64 {
    if full_len == 0 {
        return 0;
    }
    let room = BODY_OS_PAGE - (off % BODY_OS_PAGE);
    let est = if need_vouts.is_empty() {
        full_len
    } else {
        let k = u64::from(need_vouts.iter().copied().max().unwrap_or(0));
        OUTS_META_GUESS.saturating_add(k.saturating_add(1).saturating_mul(OUTS_GUESS_PER_VOUT))
    };
    if est > room {
        full_len
    } else {
        full_len.min(room)
    }
}

/// One `spent.body` pread. `members` are indices into the caller's offset
/// slice, in file order. A singleton is the 8-byte slot. A merged group is
/// the page-aligned span.
pub struct SpendSlotPeek {
    pub off: u64,
    pub len: u64,
    pub members: Vec<usize>,
}

/// Group 8-byte spend slots the way pin reads group body windows.
///
/// Caller order is not file order. A singleton preads its 8 bytes. A merged
/// group preads the page-aligned span from [`group_body_peeks`].
pub fn plan_spend_slot_peeks(offs: &[u64]) -> Vec<SpendSlotPeek> {
    if offs.is_empty() {
        return Vec::new();
    }
    let slot = crate::tx_table::OutputRecord::SPENT_SLOT_LEN as u64;
    let mut order: Vec<usize> = (0..offs.len()).collect();
    order.sort_by(|&a, &b| offs[a].cmp(&offs[b]).then(a.cmp(&b)));
    let windows: Vec<(u64, u64)> = order.iter().map(|&i| (offs[i], slot)).collect();
    group_body_peeks(&windows)
        .into_iter()
        .map(|(page_off, span, members)| {
            let members: Vec<usize> = members.iter().map(|&m| order[m]).collect();
            if members.len() == 1 {
                let i = members[0];
                SpendSlotPeek {
                    off: offs[i],
                    len: slot,
                    members,
                }
            } else {
                SpendSlotPeek {
                    off: page_off,
                    len: span,
                    members,
                }
            }
        })
        .collect()
}

#[doc(hidden)]
pub fn window_pages(off: u64, len: u64) -> (u64, u64) {
    let lo = off / BODY_OS_PAGE;
    let hi = if len == 0 {
        lo
    } else {
        off.saturating_add(len).saturating_sub(1) / BODY_OS_PAGE
    };
    (lo, hi)
}

/// One open `group_body_peeks` run. The caps live in [`group_body_peeks`]'s caller.
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct PeekGroup {
    pub glo: u64,
    pub ghi: u64,
    pub min_off: u64,
    pub max_end: u64,
}

impl PeekGroup {
    #[doc(hidden)]
    pub fn start(off: u64, len: u64) -> Self {
        let (glo, ghi) = window_pages(off, len);
        Self {
            glo,
            ghi,
            min_off: off,
            max_end: off.saturating_add(len),
        }
    }

    /// `off` is the next window in file order, the same walk as [`group_body_peeks`].
    #[doc(hidden)]
    pub fn extends(&self, off: u64, len: u64) -> bool {
        let (plo, phi) = window_pages(off, len);
        let new_hi = self.ghi.max(phi);
        let new_end = self.max_end.max(off.saturating_add(len));
        let page_off = (self.min_off / BODY_OS_PAGE) * BODY_OS_PAGE;
        let span = new_end.saturating_sub(page_off);
        let hole = off.saturating_sub(self.max_end);
        let page_merge = plo <= self.ghi && new_hi.saturating_sub(self.glo) < BODY_GROUP_MAX_PAGES;
        let gap_merge = hole <= BODY_GROUP_MAX_HOLE && span <= BODY_GROUP_MAX_SPAN;
        page_merge || gap_merge
    }

    #[doc(hidden)]
    pub fn absorb(&mut self, off: u64, len: u64) {
        let (_plo, phi) = window_pages(off, len);
        self.ghi = self.ghi.max(phi);
        self.max_end = self.max_end.max(off.saturating_add(len));
    }

    #[doc(hidden)]
    pub fn page_span(&self) -> (u64, u64) {
        let page_off = (self.min_off / BODY_OS_PAGE) * BODY_OS_PAGE;
        (page_off, self.max_end.saturating_sub(page_off))
    }
}

/// Slots that still form one [`group_body_peeks`] group.
///
/// Callers flush a spend batch when the next slot no longer joins this run.
/// Unsorted slots use the same grouper.
#[derive(Clone, Debug)]
pub struct SpendSlotRun {
    pub offs: Vec<u64>,
    pub group: Option<PeekGroup>,
    pub spilled: bool,
}

impl Default for SpendSlotRun {
    fn default() -> Self {
        Self::new()
    }
}

impl SpendSlotRun {
    pub fn new() -> Self {
        Self {
            offs: Vec::new(),
            group: None,
            spilled: false,
        }
    }

    pub fn clear(&mut self) {
        self.offs.clear();
        self.group = None;
        self.spilled = false;
    }

    /// Every `extra` slot still merges with the slots already pushed.
    pub fn accepts_all(&self, extra: &[u64]) -> bool {
        if extra.is_empty() {
            return true;
        }
        if self.spilled {
            return false;
        }
        let slot = crate::tx_table::OutputRecord::SPENT_SLOT_LEN as u64;
        if self.sorted_append(extra) {
            let mut group = self.group;
            for &off in extra {
                match group {
                    None => group = Some(PeekGroup::start(off, slot)),
                    Some(mut open) => {
                        if !open.extends(off, slot) {
                            return false;
                        }
                        open.absorb(off, slot);
                        group = Some(open);
                    }
                }
            }
            return true;
        }
        let mut all = Vec::with_capacity(self.offs.len().saturating_add(extra.len()));
        all.extend_from_slice(&self.offs);
        all.extend_from_slice(extra);
        single_sorted_group(&all).is_some()
    }

    pub fn push_all(&mut self, extra: &[u64]) {
        for &off in extra {
            self.push(off);
        }
    }

    #[doc(hidden)]
    pub fn sorted_append(&self, extra: &[u64]) -> bool {
        if let Some(&last) = self.offs.last() {
            if extra[0] < last {
                return false;
            }
        }
        extra.windows(2).all(|pair| pair[1] >= pair[0])
    }

    #[doc(hidden)]
    pub fn push(&mut self, off: u64) {
        let slot = crate::tx_table::OutputRecord::SPENT_SLOT_LEN as u64;
        if self.spilled {
            self.offs.push(off);
            return;
        }
        let Some(group) = self.group else {
            self.group = Some(PeekGroup::start(off, slot));
            self.offs.push(off);
            return;
        };
        if self.offs.last().is_some_and(|&last| off >= last) && group.extends(off, slot) {
            self.group.as_mut().expect("open group").absorb(off, slot);
            self.offs.push(off);
            return;
        }
        self.offs.push(off);
        if let Some(group) = single_sorted_group(&self.offs) {
            self.group = Some(group);
        } else {
            self.spilled = true;
            self.group = None;
        }
    }
}

#[doc(hidden)]
pub fn single_sorted_group(offs: &[u64]) -> Option<PeekGroup> {
    if offs.is_empty() {
        return None;
    }
    let slot = crate::tx_table::OutputRecord::SPENT_SLOT_LEN as u64;
    let mut order = offs.to_vec();
    order.sort_unstable();
    let mut group = PeekGroup::start(order[0], slot);
    for &off in order.iter().skip(1) {
        if !group.extends(off, slot) {
            return None;
        }
        group.absorb(off, slot);
    }
    Some(group)
}

/// Offset-sorted `[off, off+len)` windows → `(page_off, span_len, window indices)`.
///
/// Jobs that share an OS page, straddle two pages, or leave a hole of at most
/// 64 KiB become one SQE while the merged span stays at most 256 KiB. A wider
/// hole or a longer span stays split. Do not chain into SH-sized reads. A
/// singleton group preads the job window; only a merged group uses a page-span
/// bounce buffer.
#[doc(hidden)]
pub fn group_body_peeks(windows: &[(u64, u64)]) -> Vec<(u64, u64, Vec<usize>)> {
    let mut groups = Vec::new();
    if windows.is_empty() {
        return groups;
    }
    let mut start = 0usize;
    let mut group = PeekGroup::start(windows[0].0, windows[0].1);
    for (i, &(off, len)) in windows.iter().enumerate().skip(1) {
        if group.extends(off, len) {
            group.absorb(off, len);
            continue;
        }
        let (page_off, span) = group.page_span();
        groups.push((page_off, span, (start..i).collect()));
        start = i;
        group = PeekGroup::start(off, len);
    }
    let (page_off, span) = group.page_span();
    groups.push((page_off, span, (start..windows.len()).collect()));
    groups
}

#[doc(hidden)]
pub struct PeekDest {
    pub job: usize,
    pub off: u64,
    pub dest: usize,
    pub len: usize,
}

/// Singleton group → job window (`off`, `len`). Merged group → page-span bounce.
#[doc(hidden)]
pub fn group_sqe(
    page_off: u64,
    span: u64,
    members: &[usize],
    dests: &[PeekDest],
) -> (u64, u64, Option<usize>) {
    if let [wi] = members {
        let d = &dests[*wi];
        (d.off, d.len as u64, Some(*wi))
    } else {
        (page_off, span, None)
    }
}

#[doc(hidden)]
pub fn pread_grouped_peeks(
    jobs: &mut [IdxBodyJob],
    dests: &[PeekDest],
    body_fd: crate::io_handle::IoHandle,
    body_path: &std::path::Path,
    backend: ReadIoBackend,
    mark_ok: bool,
) -> Result<u64, StoreError> {
    if dests.is_empty() {
        return Ok(0);
    }
    let windows: Vec<(u64, u64)> = dests.iter().map(|d| (d.off, d.len as u64)).collect();
    let groups = group_body_peeks(&windows);
    let mut bounce: Vec<Option<Vec<u8>>> = groups
        .iter()
        .map(|(_, span, members)| {
            if members.len() == 1 {
                None
            } else {
                Some(vec![0u8; *span as usize])
            }
        })
        .collect();
    // SAFETY: each SQE buffer is a distinct job.body slice (singleton) or bounce
    // allocation (merged). Dest windows do not overlap jobs in one wave.
    let mut ops: Vec<ReadOp<'_>> = Vec::with_capacity(groups.len());
    for (g, (page_off, span, members)) in groups.iter().enumerate() {
        let (off, len, direct) = group_sqe(*page_off, *span, members, dests);
        let slice = if let Some(wi) = direct {
            let d = &dests[wi];
            let body = &mut jobs[d.job].body;
            let ptr = body[d.dest..d.dest + d.len].as_mut_ptr();
            // SAFETY: dest window is this job's body slice for this wave only.
            unsafe { std::slice::from_raw_parts_mut(ptr, d.len) }
        } else {
            let Some(buf) = bounce[g].as_mut() else {
                return Err(StoreError::Corrupt(
                    "invariant: merged body peek missing bounce",
                ));
            };
            let ptr = buf.as_mut_ptr();
            // SAFETY: bounce[g] is unique to this merged group until after pread.
            unsafe { std::slice::from_raw_parts_mut(ptr, len as usize) }
        };
        ops.push(ReadOp {
            fd: body_fd,
            offset: off,
            buf: slice,
            result: i32::MIN,
        });
    }
    bulk_io::pread_batch_backend(&mut ops, backend);
    for (g, ((page_off, _, members), ro)) in groups.iter().zip(ops.iter()).enumerate() {
        if ro.result < 0 {
            return Err(StoreError::io(
                body_path,
                std::io::Error::from_raw_os_error(-ro.result),
            ));
        }
        let got = ro.result as usize;
        if let [wi] = members.as_slice() {
            let d = &dests[*wi];
            if got < d.len {
                if !mark_ok {
                    jobs[d.job].ok = false;
                }
                continue;
            }
            if mark_ok {
                jobs[d.job].ok = true;
            }
            continue;
        }
        let Some(buf) = bounce[g].as_ref() else {
            return Err(StoreError::Corrupt(
                "invariant: merged body peek missing bounce",
            ));
        };
        for &wi in members {
            let d = &dests[wi];
            let rel = d.off.saturating_sub(*page_off) as usize;
            let need_end = rel.saturating_add(d.len);
            if got < need_end || rel + d.len > buf.len() {
                if !mark_ok {
                    jobs[d.job].ok = false;
                }
                continue;
            }
            jobs[d.job].body[d.dest..d.dest + d.len].copy_from_slice(&buf[rel..rel + d.len]);
            if mark_ok {
                jobs[d.job].ok = true;
            }
        }
    }
    Ok(groups.len() as u64)
}

/// Resolve idx (FdOnly pread) then body (backend from env). Mutates `jobs` in place.
///
/// Jobs with invalid / OOB ids are left `ok = false` without failing the batch
/// (caller applies confirm hard invariants vs head-resolve skip policy).
pub fn run_idx_body_pipeline(
    table: &VarTable,
    jobs: &mut [IdxBodyJob],
    mode: BodyMode,
) -> Result<IdxBodyIoStats, StoreError> {
    run_idx_body_pipeline_backend(table, jobs, mode, io_backend::read_io_backend())
}

/// Like [`run_idx_body_pipeline`] with an explicit body backend (tests / tools).
pub fn run_idx_body_pipeline_backend(
    table: &VarTable,
    jobs: &mut [IdxBodyJob],
    mode: BodyMode,
    backend: ReadIoBackend,
) -> Result<IdxBodyIoStats, StoreError> {
    if jobs.is_empty() {
        return Ok(IdxBodyIoStats::default());
    }
    // Callers stamp `range` from create.loc / seqsigwit.loc. Missing range → skip.

    let body_fd = table.body_read_fd();
    let body_pub = table.body_published_len();
    let body_path = table.body_file_path();

    let mut submitted: Vec<usize> = Vec::new();
    let mut guess_full_n = 0u64;
    for (i, j) in jobs.iter_mut().enumerate() {
        j.ok = false;
        j.body.clear();
        let Some((off, full_len)) = j.range else {
            continue;
        };
        let want = match mode {
            BodyMode::Outs => outs_first_wave_len(off, full_len, &j.need_vouts),
            BodyMode::Full => full_len,
        };
        if want == 0 || off.saturating_add(want) > body_pub {
            continue;
        }
        if mode == BodyMode::Outs {
            let room = BODY_OS_PAGE - (off % BODY_OS_PAGE);
            if want == full_len && full_len > room {
                guess_full_n = guess_full_n.saturating_add(1);
            }
        }
        j.body.resize(want as usize, 0);
        submitted.push(i);
    }
    if submitted.is_empty() {
        return Ok(IdxBodyIoStats::default());
    }

    submitted.sort_unstable_by_key(|&i| jobs[i].range.map(|(o, _)| o).unwrap_or(0));

    let dests: Vec<PeekDest> = submitted
        .iter()
        .map(|&i| PeekDest {
            job: i,
            off: jobs[i].range.unwrap().0,
            dest: 0,
            len: jobs[i].body.len(),
        })
        .collect();
    let mut stats = IdxBodyIoStats {
        body_sqe_n: pread_grouped_peeks(jobs, &dests, body_fd, body_path, backend, true)?,
        guess_full_n,
        ..Default::default()
    };
    if mode == BodyMode::Outs {
        let (extend_n, extend_sqe) = extend_truncated_txout_jobs(table, jobs, backend)?;
        stats.extend_n = extend_n;
        stats.body_sqe_n = stats.body_sqe_n.saturating_add(extend_sqe);
    }
    Ok(stats)
}

/// Second wave: when the first-wave peek does not cover needed outputs, pread
/// the remainder of the idx span. Empty need walks every out; sparse need
/// skips extend when the peek already contains those vouts.
#[doc(hidden)]
pub fn extend_truncated_txout_jobs(
    table: &VarTable,
    jobs: &mut [IdxBodyJob],
    backend: ReadIoBackend,
) -> Result<(u64, u64), StoreError> {
    let mut rest: Vec<(usize, usize)> = Vec::new();
    let body_pub = table.body_published_len();
    for (i, j) in jobs.iter_mut().enumerate() {
        if !j.ok {
            continue;
        }
        let Some((off, full_len)) = j.range else {
            continue;
        };
        if (j.body.len() as u64) >= full_len {
            continue;
        }
        if crate::tx_table::txout_first_page_covers_need(&j.body, j.n_out, &j.need_vouts) {
            continue;
        }
        if off.saturating_add(full_len) > body_pub {
            j.ok = false;
            continue;
        }
        let have = j.body.len();
        j.body.resize(full_len as usize, 0);
        rest.push((i, have));
    }
    if rest.is_empty() {
        return Ok((0, 0));
    }
    let extend_n = rest.len() as u64;
    rest.sort_unstable_by_key(|&(i, have)| {
        jobs[i]
            .range
            .map(|(o, _)| o.saturating_add(have as u64))
            .unwrap_or(0)
    });
    let dests: Vec<PeekDest> = rest
        .iter()
        .map(|&(i, have)| PeekDest {
            job: i,
            off: jobs[i].range.unwrap().0.saturating_add(have as u64),
            dest: have,
            len: jobs[i].body.len().saturating_sub(have),
        })
        .collect();
    let sqe = pread_grouped_peeks(
        jobs,
        &dests,
        table.body_read_fd(),
        table.body_file_path(),
        backend,
        false,
    )?;
    Ok((extend_n, sqe))
}
