use rbitcoin_store::binary_fuse8::*;
use rbitcoin_store::readonly_map::ReadonlyMap;

#[test]
fn single_key_and_small_set_no_false_negatives() {
    let one = BinaryFuse8::try_from_keys(&[0x1122_3344_5566_7788]).unwrap();
    assert!(one.contains(0x1122_3344_5566_7788));
    assert!(one.len() > 0);

    let keys: Vec<u64> = (0u64..50)
        .map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .collect();
    let f = BinaryFuse8::try_from_keys(&keys).unwrap();
    for &k in &keys {
        assert!(f.contains(k), "FN on {k:#x}");
    }
    // Re-encode-style field invariants used by fuse8_filter decode.
    assert_eq!(f.segment_length_mask, f.segment_length.saturating_sub(1));
    assert!(f.segment_length >= 4);
    assert!(f.segment_count_length >= f.segment_length);
}

#[test]
fn empty_keys_constructs_via_caller_dummy_only() {
    // size==0 geometry: empty reverse_order / zero stack still builds.
    assert!(BinaryFuse8::try_from_keys(&[]).is_ok());
    let g0 = fuse8_geom(0);
    assert_eq!(g0.segment_length, 4);
    assert_eq!(g0.segment_length_mask, 3);
    let g1 = fuse8_geom(1);
    assert!(g1.fp_array_len > 0);
    let g50 = fuse8_geom(50);
    assert!(g50.fp_array_len > g1.fp_array_len);
    assert!(g50.block_bits >= 1);
}
