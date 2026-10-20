use hex_conservative::{DisplayHex, FromHex};
use hex_conservative::{HexToArrayError, HexToBytesError};

use rbitcoin_primitives::hex::*;

#[test]
fn roundtrip() {
    let data = [0u8, 1, 0xab, 0xff];
    assert_eq!(encode(data), "0001abff");
    assert_eq!(decode("0001abff").unwrap(), data);
    assert_eq!(decode("0001ABFF").unwrap(), data);
    assert_eq!(decode("0x0a").unwrap(), vec![0x0a]);
    assert_eq!(decode("0Xff").unwrap(), vec![0xff]);
    assert_eq!(encode([]), "");
}

#[test]
fn rejects_bad_with_the_upstream_error() {
    assert_eq!(
        decode("0").unwrap_err().to_string(),
        <Vec<u8> as FromHex>::from_hex("0").unwrap_err().to_string()
    );
    assert_eq!(
        decode("zz").unwrap_err().to_string(),
        <Vec<u8> as FromHex>::from_hex("zz")
            .unwrap_err()
            .to_string()
    );
}

#[test]
fn display_hash_roundtrips_known_internal_bytes() {
    let mut internal = [0u8; 32];
    internal[0] = 0xab;
    internal[31] = 0xcd;
    let display = "cd000000000000000000000000000000000000000000000000000000000000ab";
    assert_eq!(display_hash_hex(internal), display);
    assert_eq!(parse_display_hash32(display).unwrap(), internal);
    assert_eq!(
        parse_display_hash32(&format!("0x{display}")).unwrap(),
        internal
    );
}

#[test]
fn id_parse_returns_the_upstream_array_error() {
    assert_eq!(
        parse_display_hash32("zz").unwrap_err().to_string(),
        <[u8; 32] as FromHex>::from_hex("zz")
            .unwrap_err()
            .to_string()
    );
    assert_eq!(
        parse_display_hash32("c").unwrap_err().to_string(),
        <[u8; 32] as FromHex>::from_hex("c")
            .unwrap_err()
            .to_string()
    );
    let bad = "gg".repeat(32);
    assert_eq!(
        parse_display_hash32(&bad).unwrap_err().to_string(),
        <[u8; 32] as FromHex>::from_hex(&bad)
            .unwrap_err()
            .to_string()
    );
}
