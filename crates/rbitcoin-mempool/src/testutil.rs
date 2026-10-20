//! Test-only mempool hooks. Not an operator API.

#[cfg(debug_assertions)]
use crate::store::Mempool;

/// Pretend the slot table is full so the next admit evicts instead of growing.
#[cfg(debug_assertions)]
pub fn pin_full_slot_table(pool: &mut Mempool) {
    pool.testing_pin_full_slot_table();
}
