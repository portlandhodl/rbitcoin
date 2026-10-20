//! Historical `getdata` witness-block serve meters for the 5s `tip: perf` line.

use crate::perf_meter::{PerfCounter, PerfMax};

#[doc(hidden)]
pub static SERVE_N: PerfCounter = PerfCounter::new();
#[doc(hidden)]
pub static SERVE_BYTES: PerfCounter = PerfCounter::new();
#[doc(hidden)]
pub static SERVE_TX: PerfCounter = PerfCounter::new();
#[doc(hidden)]
pub static SERVE_WALL_NS: PerfCounter = PerfCounter::new();
#[doc(hidden)]
pub static SERVE_MAX_NS: PerfMax = PerfMax::new();

/// One 5s window of historical block-serve reconstruct+encode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServePerfSample {
    pub n: u64,
    pub bytes: u64,
    pub tx_count: u64,
    pub wall_ns: u64,
    pub max_ns: u64,
}

pub fn note_serve(tx_count: u32, bytes: usize, wall_ns: u128) {
    let ns = wall_ns.min(u128::from(u64::MAX)) as u64;
    SERVE_N.add(1);
    SERVE_BYTES.add(bytes as u64);
    SERVE_TX.add(u64::from(tx_count));
    SERVE_WALL_NS.add(ns);
    SERVE_MAX_NS.note(ns);
}

/// Serve meters since the previous sample, for `DEBUG tip: perf`. Running
/// totals are untouched ([`serve_perf_totals`]).
pub fn sample_reset_serve_perf() -> ServePerfSample {
    ServePerfSample {
        n: SERVE_N.take_window(),
        bytes: SERVE_BYTES.take_window(),
        tx_count: SERVE_TX.take_window(),
        wall_ns: SERVE_WALL_NS.take_window(),
        max_ns: SERVE_MAX_NS.take(),
    }
}

/// Running historical block serves for `/metrics`: `(blocks, bytes)`.
pub fn serve_perf_totals() -> (u64, u64) {
    (SERVE_N.total(), SERVE_BYTES.total())
}

/// `serve n= bytes= tx= avg_us= max_us=` — reconstruct+encode, not BIP324 send.
pub fn format_serve_perf(s: &ServePerfSample) -> String {
    let avg_us = s.wall_ns.checked_div(s.n).unwrap_or(0) / 1_000;
    let max_us = s.max_ns / 1_000;
    format!(
        "serve n={} bytes={} tx={} avg_us={} max_us={}",
        s.n, s.bytes, s.tx_count, avg_us, max_us
    )
}
