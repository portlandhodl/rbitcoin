use rbitcoin_primitives::scriptnum::*;
use std::fmt;

#[test]
fn scriptnum_minimal_encoding() {
    assert!(scriptnum_is_minimal(&[]));
    assert!(!scriptnum_is_minimal(&[0x00]));
    assert!(!scriptnum_is_minimal(&[0x80]));
    assert!(scriptnum_is_minimal(&[0x01]));
    assert!(!scriptnum_is_minimal(&[0x01, 0x00]));
    assert!(scriptnum_is_minimal(&[0xff, 0x00]));
    assert!(scriptnum_is_minimal(&[0xff, 0x80]));
}

#[test]
fn roundtrip_and_width() {
    for n in [0i64, 1, -1, 127, 128, 255, -255, i32::MAX as i64] {
        let enc = scriptnum_encode(n);
        assert_eq!(scriptnum_decode(&enc, true).unwrap(), n, "{n}");
        assert!(scriptnum_is_minimal(&enc), "{n}");
    }
    let min32 = scriptnum_encode(i32::MIN as i64);
    assert_eq!(min32.len(), 5);
    assert_eq!(
        scriptnum_decode_width(&min32, 5, true).unwrap(),
        i32::MIN as i64
    );
    assert_eq!(scriptnum_decode_width(&[0x80], 5, false).unwrap(), 0);
    assert!(matches!(
        scriptnum_decode(&[0x00], true),
        Err(ScriptNumError::NonMinimal)
    ));
    assert!(matches!(
        scriptnum_decode(&[0, 0, 0, 0, 1], false),
        Err(ScriptNumError::Overflow)
    ));
    assert_eq!(
        scriptnum_decode_width(&[0, 0, 0, 0, 1], 5, false).unwrap(),
        1 << 32
    );
}

#[cfg(miri)]
#[test]
fn miri_scriptnum_range_width5_nonminimal() {
    for n in -1000i64..=1000 {
        let enc = scriptnum_encode(n);
        assert_eq!(scriptnum_decode_width(&enc, 5, true).unwrap(), n);
    }
    assert_eq!(scriptnum_decode_width(&[0x80], 5, false).unwrap(), 0);
    assert!(scriptnum_decode(&[0x00], true).is_err());
    assert!(scriptnum_decode(&[0x80], true).is_err());
    assert!(scriptnum_decode_width(&[0xff, 0x00], 5, true).is_ok());
}
