use bitcoin::secp256k1::Secp256k1;
use bitcoin::secp256k1::{PublicKey, SecretKey};
use bitcoin::Network;
use rbitcoin_consensus::{taproot_matches_scan, ChainParams};
use rbitcoin_electrum::silent_scan::*;
use rbitcoin_primitives::Height;
use rbitcoin_query::tweaks_for_height;
use rbitcoin_query::Query;
use serde_json::{json, Value};

#[test]
fn parse_sub_indexes_a_non_string_param() {
    match parse_sub(&json!([1, "02"]), Network::Regtest, Some(0)) {
        Err(err) => assert_eq!(err, "param 0 expected string"),
        Ok(_) => panic!("a non-string scan key must fail"),
    }
}

#[test]
fn parse_sub_rejects_short_scan_key() {
    let err = match parse_sub(&json!(["00", "02".repeat(33)]), Network::Regtest, Some(0)) {
        Err(e) => e,
        Ok(_) => panic!("expected error"),
    };
    assert!(err.contains("32 bytes"), "{err}");
    let _ = Secp256k1::new();
}

#[test]
fn parse_sub_labels_start_and_networks() {
    let scan = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";
    let spend = "025cc9856d6f8375350e123978daac200c260cb5b5ae83106cab90484dcd8fcf36";
    let sub = parse_sub(
        &json!([scan, spend, "12-20", [0, 1, 1]]),
        Network::Bitcoin,
        Some(100),
    )
    .unwrap();
    assert_eq!(sub.start, 12);
    assert_eq!(sub.labels, vec![0, 1]);
    assert!(sub.address.starts_with("sp1"), "{}", sub.address);
    let tsp = parse_sub(&json!([scan, spend]), Network::Signet, Some(3)).unwrap();
    assert!(tsp.address.starts_with("tsp1"), "{}", tsp.address);
    let t4 = parse_sub(&json!([scan, spend]), Network::Testnet4, Some(3)).unwrap();
    assert!(t4.address.starts_with("tsp"), "{}", t4.address);
    let tn = parse_sub(&json!([scan, spend]), Network::Testnet, Some(3)).unwrap();
    assert!(tn.address.starts_with("tsp"), "{}", tn.address);
    let no_tip = parse_sub(&json!([scan, spend, 50]), Network::Regtest, None).unwrap();
    assert_eq!(no_tip.start, 50);
    let null_start = parse_sub(&json!([scan, spend, null]), Network::Regtest, Some(3)).unwrap();
    assert_eq!(null_start.start, 0);
    let bounded = parse_sub(&json!([scan, spend, 0]), Network::Regtest, Some(1_000)).unwrap();
    assert_eq!(bounded.start, 1_000 - SP_HISTORY_WINDOW);
    let wide = parse_sub(&json!([scan, spend, 1]), Network::Regtest, Some(10_000)).unwrap();
    assert_eq!(wide.start, 10_000 - SP_HISTORY_WINDOW);
    assert!(10_000 - wide.start <= SP_HISTORY_WINDOW);
    let bad_spend = match parse_sub(&json!([scan, "02"]), Network::Regtest, Some(0)) {
        Err(e) => e,
        Ok(_) => panic!("spend"),
    };
    assert!(!bad_spend.is_empty(), "{bad_spend}");
    let ts = match parse_sub(
        &json!([scan, spend, 600_000_000]),
        Network::Regtest,
        Some(0),
    ) {
        Err(e) => e,
        Ok(_) => panic!("timestamp"),
    };
    assert!(ts.contains("timestamp"), "{ts}");
    let many = json!([scan, spend, 0, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]]);
    let too = match parse_sub(&many, Network::Regtest, Some(0)) {
        Err(e) => e,
        Ok(_) => panic!("labels"),
    };
    assert!(too.contains("too many"), "{too}");
    let r = subscribe_result(&sub);
    assert_eq!(r["start_height"], 12);
    let plain = parse_sub(
        &json!([scan, spend, "12", ["x", 2]]),
        Network::Regtest,
        Some(20),
    )
    .unwrap();
    assert_eq!(plain.start, 12);
    assert_eq!(plain.labels, vec![0, 2]);
    let clamp = parse_sub(&json!([scan, spend, 100]), Network::Regtest, Some(3)).unwrap();
    assert_eq!(clamp.start, 3);
    let bool_start = parse_sub(&json!([scan, spend, true]), Network::Regtest, Some(0)).unwrap();
    assert_eq!(bool_start.start, 0);
}

#[test]
fn tx_matches_bip352_simple_send() {
    let scan = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";
    let spend = "025cc9856d6f8375350e123978daac200c260cb5b5ae83106cab90484dcd8fcf36";
    let sub = parse_sub(&json!([scan, spend, 0, [0, 1]]), Network::Regtest, Some(0)).unwrap();
    let tweak = rbitcoin_primitives::hex_decode(
        "024ac253c216532e961988e2a8ce266a447c894c781e52ef6cee902361db960004",
    )
    .unwrap();
    let xonly = rbitcoin_primitives::hex_decode(
        "3e9fce73d4e77a4809908e3c3a2e54ee147b9312dc5044a193d1fc85de46e3c1",
    )
    .unwrap();
    let mut tw33 = [0u8; 33];
    tw33.copy_from_slice(&tweak);
    let mut x32 = [0u8; 32];
    x32.copy_from_slice(&xonly);
    let hit = rbitcoin_consensus::TxTweak {
        tweak: tw33,
        output_pubkeys: vec![rbitcoin_consensus::TaprootOut {
            vout: 0,
            xonly: x32,
            value: 1,
        }],
    };
    assert!(tx_matches(&sub, &hit));
    let miss = rbitcoin_consensus::TxTweak {
        tweak: tw33,
        output_pubkeys: vec![rbitcoin_consensus::TaprootOut {
            vout: 0,
            xonly: [0u8; 32],
            value: 1,
        }],
    };
    assert!(!tx_matches(&sub, &miss));
}
