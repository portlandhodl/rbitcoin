use rbitcoin_primitives::compact::*;
use std::fmt;

#[test]
fn compact_size_roundtrip() {
    for n in [0u64, 1, 252, 253, 1000, u32::MAX as u64, u64::MAX] {
        let mut buf = Vec::new();
        write_compact_size(&mut buf, n);
        let (v, used) = read_compact_size(&buf).unwrap();
        assert_eq!(v, n);
        assert_eq!(used, buf.len());
        assert_eq!(compact_size_len(n), buf.len());
    }
    assert_eq!(compact_size_len(252), 1);
    assert_eq!(compact_size_len(253), 3);
    assert_eq!(compact_size_len(u16::MAX as u64), 3);
    assert_eq!(compact_size_len(u16::MAX as u64 + 1), 5);
    assert_eq!(compact_size_len(u32::MAX as u64), 5);
    assert_eq!(compact_size_len(u32::MAX as u64 + 1), 9);
    assert_eq!(uleb128_len(0), 1);
    assert_eq!(uleb128_len(127), 1);
    assert_eq!(uleb128_len(128), 2);
    assert!(uleb128_len(u64::MAX) >= 9);
}

#[test]
fn uleb_roundtrip() {
    for n in [0u64, 1, 127, 128, 255, 300, u32::MAX as u64, u64::MAX >> 1] {
        let mut buf = Vec::new();
        write_uleb128(&mut buf, n);
        let (v, used) = read_uleb128(&buf).unwrap();
        assert_eq!(v, n);
        assert_eq!(used, buf.len());
    }
}

#[test]
fn write_uleb128_into_matches_vec() {
    for n in [0u64, 127, 128, 255, 300, u32::MAX as u64, u64::MAX] {
        let mut vec = Vec::new();
        write_uleb128(&mut vec, n);
        let mut dst = [0u8; 16];
        let used = write_uleb128_into(&mut dst, n).unwrap();
        assert_eq!(used, vec.len());
        assert_eq!(used, uleb128_len(n));
        assert_eq!(&dst[..used], vec.as_slice());
    }
    assert_eq!(
        write_uleb128_into(&mut [0u8; 1], 128).unwrap_err(),
        PackError("uleb128 dest short")
    );
}

#[test]
fn compact_and_uleb_error_paths() {
    assert_eq!(
        read_compact_size(&[]).unwrap_err(),
        PackError("compact size empty")
    );
    assert_eq!(
        read_compact_size(&[253, 1]).unwrap_err(),
        PackError("compact size u16 truncated")
    );
    assert_eq!(
        read_compact_size(&[254, 1, 2, 3]).unwrap_err(),
        PackError("compact size u32 truncated")
    );
    assert_eq!(
        read_compact_size(&[255, 1, 2, 3, 4, 5, 6, 7]).unwrap_err(),
        PackError("compact size u64 truncated")
    );
    assert_eq!(
        read_uleb128(&[0x80]).unwrap_err(),
        PackError("uleb128 truncated")
    );
    let mut over = vec![0x80u8; 10];
    over.push(0x01);
    assert_eq!(
        read_uleb128(&over).unwrap_err(),
        PackError("uleb128 overflow")
    );
    // Ninth continuation lands on shift 63. Only the low bit fits in a u64.
    let mut at63 = vec![0x80u8; 9];
    at63.push(0x02);
    assert_eq!(
        read_uleb128(&at63).unwrap_err(),
        PackError("uleb128 overflow")
    );
    at63[9] = 0x7f;
    assert_eq!(
        read_uleb128(&at63).unwrap_err(),
        PackError("uleb128 overflow")
    );
    at63[9] = 0x00;
    assert_eq!(read_uleb128(&at63).unwrap(), (0, 10));
    at63[9] = 0x01;
    assert_eq!(read_uleb128(&at63).unwrap(), (1u64 << 63, 10));
    let mut maxb = Vec::new();
    write_uleb128(&mut maxb, u64::MAX);
    assert_eq!(read_uleb128(&maxb).unwrap(), (u64::MAX, maxb.len()));
    let (v, n) = read_compact_size(&[253, 0, 1]).unwrap();
    assert_eq!((v, n), (256, 3));
    let (v, n) = read_compact_size(&[254, 0, 0, 1, 0]).unwrap();
    assert_eq!((v, n), (1 << 16, 5));
    let mut u64b = vec![255u8];
    u64b.extend_from_slice(&u64::MAX.to_le_bytes());
    let (v, n) = read_compact_size(&u64b).unwrap();
    assert_eq!((v, n), (u64::MAX, 9));
    // Non-canonical prefixes still decode (shipped; not Core's reject).
    assert_eq!(read_compact_size(&[253, 1, 0]).unwrap(), (1, 3));
    assert_eq!(read_compact_size(&[254, 1, 0, 0, 0]).unwrap(), (1, 5));
    let mut ncan64 = vec![255u8];
    ncan64.extend_from_slice(&1u64.to_le_bytes());
    assert_eq!(read_compact_size(&ncan64).unwrap(), (1, 9));
}

#[cfg(miri)]
#[test]
fn miri_compact_wide_and_uleb_errors() {
    let mut five = Vec::new();
    write_compact_size(&mut five, u16::MAX as u64 + 1);
    assert_eq!(five.len(), 5);
    assert_eq!(read_compact_size(&five).unwrap().0, u16::MAX as u64 + 1);

    let mut nine = Vec::new();
    write_compact_size(&mut nine, u32::MAX as u64 + 1);
    assert_eq!(nine.len(), 9);
    assert_eq!(read_compact_size(&nine).unwrap().0, u32::MAX as u64 + 1);

    assert!(read_compact_size(&[254, 1, 2, 3]).is_err());
    assert!(read_compact_size(&[255, 1, 2, 3, 4, 5, 6, 7]).is_err());
    assert!(read_uleb128(&[0x80]).is_err());
    let mut over = vec![0x80u8; 10];
    over.push(0x01);
    assert!(read_uleb128(&over).is_err());

    assert_eq!(read_compact_size(&[253, 252, 0]).unwrap().0, 252);
    assert_eq!(read_compact_size(&[254, 253, 0, 0, 0]).unwrap().0, 253);
    let mut ncan64 = vec![255u8];
    ncan64.extend_from_slice(&(u32::MAX as u64).to_le_bytes());
    assert_eq!(read_compact_size(&ncan64).unwrap().0, u32::MAX as u64);
}

#[test]
fn read_compact_size_from_advances_the_slice() {
    let mut rdr: &[u8] = &[1u8, 252, 253, 0x04, 0x01];
    assert_eq!(read_compact_size_from(&mut rdr).unwrap(), 1);
    assert_eq!(read_compact_size_from(&mut rdr).unwrap(), 252);
    assert_eq!(read_compact_size_from(&mut rdr).unwrap(), 260);
    assert!(rdr.is_empty());
    assert!(read_compact_size_from(&mut rdr).is_err());
}
