//! Bitcoin **block** wire walk: Σ `tx.input` without a `Block` decode.
//!
//! Peer enqueue stamps this so IBD lookup can pack/hold waves from the BQ
//! index. The walk refuses the payloads lookup's block decode refuses, so
//! intake can drop a body that lookup would never decode.

use rbitcoin_primitives::read_compact_size;

#[doc(hidden)]
pub const HEADER_LEN: usize = 80;

/// Σ input count over every tx in a serialized block (header + tx vector).
///
/// `None` when lookup's block decode fails on a P2P-sized payload:
/// truncation, a non-minimal CompactSize, a segwit flag other than 0 or 1,
/// or a segwit tx whose witnesses are all empty. Flag 0 is Core's 10-byte
/// tx with no inputs and no outputs. Trailing bytes are ignored, as in that
/// decode.
pub fn block_wire_input_count(payload: &[u8]) -> Option<u32> {
    if payload.len() < HEADER_LEN {
        return None;
    }
    let mut off = HEADER_LEN;
    let n_tx = read_count(payload, &mut off)?;
    // A serialized tx is at least 10 bytes (version, two counts, locktime).
    if n_tx as usize > (payload.len() - off) / 10 {
        return None;
    }
    let mut inputs = 0u32;
    for _ in 0..n_tx {
        inputs = inputs.saturating_add(skip_tx(payload, &mut off)?);
    }
    Some(inputs)
}

#[doc(hidden)]
pub fn read_compact(buf: &[u8], off: &mut usize) -> Option<u64> {
    let (v, n) = read_compact_size(buf.get(*off..)?).ok()?;
    // rust-bitcoin refuses a non-minimal CompactSize.
    let min = match n {
        3 => 0xfd,
        5 => 0x1_0000,
        9 => 0x1_0000_0000,
        _ => 0,
    };
    if v < min {
        return None;
    }
    *off += n;
    Some(v)
}

#[doc(hidden)]
pub fn read_count(buf: &[u8], off: &mut usize) -> Option<u32> {
    u32::try_from(read_compact(buf, off)?).ok()
}

#[doc(hidden)]
pub fn advance(buf: &[u8], off: &mut usize, n: usize) -> Option<()> {
    *off = off.checked_add(n)?;
    (*off <= buf.len()).then_some(())
}

#[doc(hidden)]
pub fn skip_compact_blob(buf: &[u8], off: &mut usize) -> Option<()> {
    let len = usize::try_from(read_compact(buf, off)?).ok()?;
    advance(buf, off, len)
}

#[doc(hidden)]
pub fn skip_input(buf: &[u8], off: &mut usize) -> Option<()> {
    advance(buf, off, 36)?;
    skip_compact_blob(buf, off)?;
    advance(buf, off, 4)
}

#[doc(hidden)]
pub fn skip_output(buf: &[u8], off: &mut usize) -> Option<()> {
    advance(buf, off, 8)?;
    skip_compact_blob(buf, off)
}

/// Skip one tx; return its `nIn`.
#[doc(hidden)]
pub fn skip_tx(buf: &[u8], off: &mut usize) -> Option<u32> {
    advance(buf, off, 4)?;
    let mut n_in = read_count(buf, off)?;
    let segwit = n_in == 0;
    if segwit {
        // BIP144: an empty vin is the marker and flag 1 follows. Core reads
        // flag 0 as a tx with no inputs and no outputs, then the locktime.
        match *buf.get(*off)? {
            0 => {
                advance(buf, off, 5)?;
                return Some(0);
            }
            1 => *off += 1,
            _ => return None,
        }
        n_in = read_count(buf, off)?;
    }
    for _ in 0..n_in {
        skip_input(buf, off)?;
    }
    let n_out = read_count(buf, off)?;
    for _ in 0..n_out {
        skip_output(buf, off)?;
    }
    if segwit {
        let mut any_stack = false;
        for _ in 0..n_in {
            let n_stack = read_count(buf, off)?;
            any_stack |= n_stack > 0;
            for _ in 0..n_stack {
                skip_compact_blob(buf, off)?;
            }
        }
        // rust-bitcoin refuses the segwit flag when every witness is empty.
        if n_in > 0 && !any_stack {
            return None;
        }
    }
    advance(buf, off, 4)?;
    Some(n_in)
}
