use rbitcoin_primitives::loc_simd::*;

fn interleave(st: [u8; 8], no: [u8; 8]) -> [u8; 16] {
    let mut buf = [0u8; 16];
    for i in 0..8 {
        buf[i * 2] = st[i];
        buf[i * 2 + 1] = no[i];
    }
    buf
}

#[test]
fn deinterleave_pairs_lanes_and_zero_in_same_load() {
    let mut buf = [0u8; 16];
    for i in 0..8 {
        buf[i * 2] = (i as u8) + 1;
        buf[i * 2 + 1] = (i as u8) + 2;
    }
    let (st, no, z) = deinterleave_pairs_u8x8(buf);
    assert_eq!(st, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(no, [2, 3, 4, 5, 6, 7, 8, 9]);
    assert!(!z);
    buf[0] = 0;
    assert!(deinterleave_pairs_u8x8(buf).2);
    buf[0] = 1;
    buf[15] = 0;
    assert!(deinterleave_pairs_u8x8(buf).2);
}

#[test]
fn inclusive_u8x8_times_8_matches_shift_sum() {
    let p = [255u8, 1, 0, 8, 255, 9, 2, 3];
    let mut expect = [0u32; 8];
    let mut acc = 0u32;
    for (i, b) in p.iter().enumerate() {
        acc += u32::from(*b) << 3;
        expect[i] = acc;
    }
    let (tx, sp) = inclusive_u8x8_times_8(&p, &p);
    assert_eq!(tx, expect);
    assert_eq!(sp, expect);
    assert_eq!(inclusive_u8x8_times_8_scalar(&p, &p), (expect, expect));
}

#[test]
fn simd_matches_scalar() {
    let cases: [[u8; 16]; 4] = [
        [0; 16],
        [255; 16],
        interleave([1, 2, 3, 4, 5, 6, 7, 8], [8, 7, 6, 5, 4, 3, 2, 1]),
        {
            let mut buf = [0u8; 16];
            buf[0] = 0;
            buf[15] = 255;
            buf
        },
    ];
    for buf in cases {
        assert_eq!(
            deinterleave_pairs_u8x8(buf),
            deinterleave_pairs_u8x8_scalar(&buf)
        );
        let (st, no, _) = deinterleave_pairs_u8x8_scalar(&buf);
        assert_eq!(
            inclusive_u8x8_times_8(&st, &no),
            inclusive_u8x8_times_8_scalar(&st, &no)
        );
    }
}

#[cfg(miri)]
#[test]
fn miri_loc_simd_vectors() {
    let mut seed = 0x9e37_79b9u32;
    for _ in 0..32 {
        let mut buf = [0u8; 16];
        for b in &mut buf {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (seed >> 24) as u8;
        }
        assert_eq!(
            deinterleave_pairs_u8x8(buf),
            deinterleave_pairs_u8x8_scalar(&buf)
        );
        let (st, no, _) = deinterleave_pairs_u8x8_scalar(&buf);
        assert_eq!(
            inclusive_u8x8_times_8(&st, &no),
            inclusive_u8x8_times_8_scalar(&st, &no)
        );
    }
}
