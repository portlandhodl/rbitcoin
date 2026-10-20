use rbitcoin_store::open_address::*;

#[test]
fn primary_slot_stable_and_in_range() {
    let k = [0xabu8; 16];
    let s = primary_slot(&k, 1024);
    assert!(s < 1024);
    assert_eq!(s, primary_slot(&k, 1024));
}

#[test]
fn fnv1a_32_matches_stream_and_primary_mask() {
    // Same FNV stream as 32-byte open-address helpers (mask = primary slot).
    let k = [9u8; 32];
    let slots = 64usize;
    let mut h = FNV_OFFSET;
    for b in &k {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    assert_eq!(fnv1a_64(&k), h);
    let slot = (fnv1a_64(&k) as usize) & (slots - 1);
    assert_eq!(slot, (h as usize) & (slots - 1));
    assert!(slot < slots);
}

#[test]
fn load_constants() {
    assert_eq!(MAX_LOAD_NUM, 7);
    assert_eq!(MAX_LOAD_DEN, 8);
}
