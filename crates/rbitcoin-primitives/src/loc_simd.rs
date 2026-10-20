//! create.loc window SIMD: deinterleave `(stride, n_out)` pairs and inclusive `u8 << 3`.

#[cfg(any(
    debug_assertions,
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
#[doc(hidden)]
pub fn deinterleave_pairs_u8x8_scalar(buf: &[u8; 16]) -> ([u8; 8], [u8; 8], bool) {
    let mut st = [0u8; 8];
    let mut no = [0u8; 8];
    let mut has_zero = false;
    for k in 0..8 {
        st[k] = buf[k * 2];
        no[k] = buf[k * 2 + 1];
        if st[k] == 0 || no[k] == 0 {
            has_zero = true;
        }
    }
    (st, no, has_zero)
}

#[cfg(any(
    debug_assertions,
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
#[doc(hidden)]
pub fn inclusive_u8x8_times_8_scalar(st: &[u8; 8], no: &[u8; 8]) -> ([u32; 8], [u32; 8]) {
    let mut tx = [0u32; 8];
    let mut sp = [0u32; 8];
    let mut t = 0u32;
    let mut s = 0u32;
    for k in 0..8 {
        t = t.saturating_add(u32::from(st[k]) << 3);
        s = s.saturating_add(u32::from(no[k]) << 3);
        tx[k] = t;
        sp[k] = s;
    }
    (tx, sp)
}

/// Split 16 interleaved `(stride, n_out)` bytes. `true` if any lane is 0 (overflow slot).
pub fn deinterleave_pairs_u8x8(buf: [u8; 16]) -> ([u8; 8], [u8; 8], bool) {
    #[cfg(target_arch = "x86_64")]
    let out = unsafe { sse2_deinterleave_pairs_u8x8(buf.as_ptr()) };
    #[cfg(target_arch = "aarch64")]
    let out = unsafe { neon_deinterleave_pairs_u8x8(buf.as_ptr()) };
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let out = deinterleave_pairs_u8x8_scalar(&buf);
    out
}

/// Inclusive prefix of eight `u8 << 3` values (create.loc stride-8 window; fits u32).
pub fn inclusive_u8x8_times_8(st: &[u8; 8], no: &[u8; 8]) -> ([u32; 8], [u32; 8]) {
    #[cfg(target_arch = "x86_64")]
    let out = unsafe {
        (
            sse2_u8x8_times_8_inclusive(st.as_ptr()),
            sse2_u8x8_times_8_inclusive(no.as_ptr()),
        )
    };
    #[cfg(target_arch = "aarch64")]
    let out = unsafe {
        (
            neon_u8x8_times_8_inclusive(st.as_ptr()),
            neon_u8x8_times_8_inclusive(no.as_ptr()),
        )
    };
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let out = inclusive_u8x8_times_8_scalar(st, no);
    out
}

/// # Safety
/// `p` must be readable for 16 bytes.
#[cfg(target_arch = "x86_64")]
pub unsafe fn sse2_deinterleave_pairs_u8x8(p: *const u8) -> ([u8; 8], [u8; 8], bool) {
    use std::arch::x86_64::{
        __m128i, _mm_and_si128, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8,
        _mm_packus_epi16, _mm_set1_epi16, _mm_setzero_si128, _mm_srli_epi16, _mm_storel_epi64,
    };
    let v = _mm_loadu_si128(p as *const __m128i);
    let has_zero = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_setzero_si128())) != 0;
    let mask = _mm_set1_epi16(0x00FF);
    let st16 = _mm_and_si128(v, mask);
    let no16 = _mm_srli_epi16(v, 8);
    let z = _mm_setzero_si128();
    let st8 = _mm_packus_epi16(st16, z);
    let no8 = _mm_packus_epi16(no16, z);
    let mut st = [0u8; 8];
    let mut no = [0u8; 8];
    _mm_storel_epi64(st.as_mut_ptr() as *mut __m128i, st8);
    _mm_storel_epi64(no.as_mut_ptr() as *mut __m128i, no8);
    (st, no, has_zero)
}

/// # Safety
/// `p` must be readable for 16 bytes.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn neon_deinterleave_pairs_u8x8(p: *const u8) -> ([u8; 8], [u8; 8], bool) {
    use std::arch::aarch64::{vceq_u8, vdup_n_u8, vld2_u8, vmaxv_u8, vst1_u8};
    let v = vld2_u8(p);
    let z = vdup_n_u8(0);
    let has_zero = vmaxv_u8(vceq_u8(v.0, z)) != 0 || vmaxv_u8(vceq_u8(v.1, z)) != 0;
    let mut st = [0u8; 8];
    let mut no = [0u8; 8];
    vst1_u8(st.as_mut_ptr(), v.0);
    vst1_u8(no.as_mut_ptr(), v.1);
    (st, no, has_zero)
}

/// # Safety
/// `p` must be readable for 8 bytes.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn sse2_u8x8_times_8_inclusive(p: *const u8) -> [u32; 8] {
    use std::arch::x86_64::{
        __m128i, _mm_add_epi32, _mm_cvtsi128_si32, _mm_loadl_epi64, _mm_set1_epi32,
        _mm_setzero_si128, _mm_slli_epi32, _mm_slli_si128, _mm_srli_si128, _mm_storeu_si128,
        _mm_unpackhi_epi16, _mm_unpacklo_epi16, _mm_unpacklo_epi8,
    };
    let prefix4 = |v: __m128i| {
        let s = _mm_add_epi32(v, _mm_slli_si128(v, 4));
        _mm_add_epi32(s, _mm_slli_si128(s, 8))
    };
    let v = _mm_loadl_epi64(p as *const __m128i);
    let z = _mm_setzero_si128();
    let w = _mm_unpacklo_epi8(v, z);
    let lo = prefix4(_mm_slli_epi32(_mm_unpacklo_epi16(w, z), 3));
    let hi = prefix4(_mm_slli_epi32(_mm_unpackhi_epi16(w, z), 3));
    let lo_sum = _mm_cvtsi128_si32(_mm_srli_si128(lo, 12));
    let hi = _mm_add_epi32(hi, _mm_set1_epi32(lo_sum));
    let mut out = [0u32; 8];
    _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, lo);
    _mm_storeu_si128(out.as_mut_ptr().add(4) as *mut __m128i, hi);
    out
}

/// # Safety
/// `p` must be readable for 8 bytes.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
pub unsafe fn neon_u8x8_times_8_inclusive(p: *const u8) -> [u32; 8] {
    use std::arch::aarch64::{
        uint32x4_t, vaddq_u32, vdupq_n_u32, vextq_u32, vget_high_u16, vget_low_u16, vgetq_lane_u32,
        vld1_u8, vmovl_u16, vmovl_u8, vshlq_n_u32, vst1q_u32,
    };
    let prefix4 = |v: uint32x4_t| {
        let z = vdupq_n_u32(0);
        let s = vaddq_u32(v, vextq_u32(z, v, 3));
        vaddq_u32(s, vextq_u32(z, s, 2))
    };
    let v = vld1_u8(p);
    let v16 = vmovl_u8(v);
    let lo = prefix4(vshlq_n_u32(vmovl_u16(vget_low_u16(v16)), 3));
    let hi = prefix4(vshlq_n_u32(vmovl_u16(vget_high_u16(v16)), 3));
    let hi = vaddq_u32(hi, vdupq_n_u32(vgetq_lane_u32(lo, 3)));
    let mut out = [0u32; 8];
    vst1q_u32(out.as_mut_ptr(), lo);
    vst1q_u32(out.as_mut_ptr().add(4), hi);
    out
}
