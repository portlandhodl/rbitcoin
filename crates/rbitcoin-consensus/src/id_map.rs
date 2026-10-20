//! Txid / outpoint identity hashers and the wave `txid → (fk, body_range)` map.

use rbitcoin_primitives::Fk;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Identity hasher for `[u8; 32]` txids (already uniform). `finish()` is the
/// first 8 bytes; equality still compares the full key.
#[derive(Default, Clone, Copy)]
pub struct TxidHasher(u64);

impl Hasher for TxidHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() >= 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[..8]);
            self.0 = u64::from_le_bytes(raw);
        } else {
            self.0 = 0;
            for (i, &b) in bytes.iter().enumerate() {
                self.0 |= u64::from(b) << (8 * i);
            }
        }
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Mixes every `write` so `(txid, vout)` keys stay distinct.
///
/// [`TxidHasher`] overwrites `self.0` per write — using it on an outpoint
/// would drop the txid hash when the vout bytes arrive.
#[derive(Default, Clone, Copy)]
pub struct OutPointHasher(u64);

impl Hasher for OutPointHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// `(txid, vout)` set that folds every hasher write (see [`OutPointHasher`]).
pub type OutPointSet =
    std::collections::HashSet<([u8; 32], u32), BuildHasherDefault<OutPointHasher>>;

/// Immutable `txid → (create_fk, body_range)` for one resolve wave.
pub type IdMap = HashMap<[u8; 32], (Fk, (u64, u64)), BuildHasherDefault<TxidHasher>>;

/// Confirm-hot `txid → V` (identity hasher; equality still uses the full key).
pub type TxidMap<V> = HashMap<[u8; 32], V, BuildHasherDefault<TxidHasher>>;

/// Confirm-hot txid set (duplicate / need-key collect).
pub type TxidSet = std::collections::HashSet<[u8; 32], BuildHasherDefault<TxidHasher>>;
