//! Shared open-address helpers for hash heads (`HashHead`, `ScriptHashHead`).
//!
//! Both tables use 16-byte key prefixes, linear probing, and the same FNV-1a
//! primary hash. Full strategy merge (one generic table) is deferred; this
//! module keeps probe math / load factor from drifting.

/// Occupied/slots ceiling; insert past this is full (not an in-place rehash).
pub const MAX_LOAD_NUM: u64 = 7;
pub const MAX_LOAD_DEN: u64 = 8;

/// FNV-1a 64-bit offset basis (HashHead / ScriptHashHead / overflow probe).
pub const FNV_OFFSET: u64 = 0xcbf29ce484222325;
pub const FNV_PRIME: u64 = 0x100000001b3;

/// FNV-1a 64 over arbitrary bytes (shared open-address / overflow probe math).
#[inline]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Primary slot for a 16-byte open-address key (`slots` must be power of two).
#[inline]
pub fn primary_slot(key: &[u8; 16], slots: u64) -> u64 {
    debug_assert!(slots.is_power_of_two() && slots >= 2);
    fnv1a_64(key) & (slots - 1)
}
