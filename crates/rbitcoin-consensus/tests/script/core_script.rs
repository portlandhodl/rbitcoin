use super::{decode_hex, spend_shape_from_json, SpendShape};

#[test]
fn decode_hex_strips_0x_and_names_odd_length() {
    assert_eq!(decode_hex("0x0a").unwrap(), vec![0x0a]);
    assert_eq!(decode_hex("0Xff").unwrap(), vec![0xff]);
    assert_eq!(decode_hex("abc").unwrap_err(), {
        use bitcoin::hex::FromHex;
        <Vec<u8> as FromHex>::from_hex("abc")
            .unwrap_err()
            .to_string()
    });
    assert_eq!(decode_hex("zz").unwrap_err(), {
        use bitcoin::hex::FromHex;
        <Vec<u8> as FromHex>::from_hex("zz")
            .unwrap_err()
            .to_string()
    });
}

#[test]
fn spend_shape_defaults_and_unsigned_version_bits() {
    assert_eq!(spend_shape_from_json(None).unwrap(), SpendShape::default());
    let row = serde_json::json!({
        "version": 4294967295u32,
        "locktime": 500_000_000,
        "sequence": 0,
        "n_outputs": 0
    });
    let shape = spend_shape_from_json(Some(&row)).unwrap();
    assert_eq!(shape.version, -1);
    assert_eq!(shape.lock_time, 500_000_000);
    assert_eq!(shape.sequence, 0);
    assert_eq!(shape.n_outputs, 0);
}
