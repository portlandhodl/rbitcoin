//! Hex spelling for the workspace.
//!
//! The bytes are [`hex_conservative`](https://docs.rs/hex-conservative), the
//! crate behind `bitcoin::hex`. These functions exist so a call site does
//! not spell `AsRef`, and so `0x` stripping and display-order reversal live
//! in one place. This crate does not depend on `bitcoin`: store is
//! hashes-only and already depends on primitives.
//!
//! Errors are hex-conservative's own.

use hex_conservative::{DisplayHex, FromHex};

pub use hex_conservative::{HexToArrayError, HexToBytesError};

#[doc(hidden)]
pub fn strip_0x(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

/// Lowercase hex encoding of `data`.
#[inline]
pub fn encode(data: impl AsRef<[u8]>) -> String {
    DisplayHex::to_lower_hex_string(data.as_ref())
}

/// Decode a hex string (even length, optional `0x` prefix). Accepts a-f/A-F.
#[inline]
pub fn decode(s: &str) -> Result<Vec<u8>, HexToBytesError> {
    Vec::<u8>::from_hex(strip_0x(s))
}

/// Core / Electrum / Esplora **display order** hex for a 32-byte hash or txid.
///
/// Store and rust-bitcoin `to_byte_array()` use **internal** byte order; RPC
/// clients expect the reversed hex (same as `BlockHash`/`Txid` `Display`).
#[inline]
pub fn display_hash_hex(mut h: [u8; 32]) -> String {
    h.reverse();
    encode(h)
}

/// Parse display-order 32-byte hex → internal byte order.
///
/// `0x` / `0X` is stripped here. Any other failure is [`HexToArrayError`]:
/// a length other than 64 characters, including a short non-hex id, is
/// reported as a length error before the digits are read.
#[inline]
pub fn parse_display_hash32(hex: &str) -> Result<[u8; 32], HexToArrayError> {
    let mut raw = <[u8; 32]>::from_hex(strip_0x(hex))?;
    raw.reverse();
    Ok(raw)
}
