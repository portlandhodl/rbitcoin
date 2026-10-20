use bitcoin::hashes::Hash;
use bitcoin::Transaction;
use rbitcoin_consensus::block::ScriptCheckJob;
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::script::classify::is_anyone_can_spend;
use rbitcoin_consensus::script::classify::ScriptKind;
use rbitcoin_consensus::script::core_fixture::*;

#[test]
fn stages_script_tests_from_submodule() {
    let p = stage_core_json("script_tests.json");
    assert!(p.is_file(), "staged {p:?}");
    let head = fs::read_to_string(&p).unwrap();
    assert!(
        head.trim_start().starts_with('['),
        "expected JSON array at {p:?}"
    );
}

#[test]
fn stages_sighash_json_from_submodule() {
    let p = stage_core_json("sighash.json");
    assert!(p.is_file(), "staged {p:?}");
    let head = fs::read_to_string(&p).unwrap();
    assert!(
        head.trim_start().starts_with('['),
        "expected JSON array at {p:?}"
    );
}

#[test]
fn concurrent_stage_same_json_does_not_lose_tmp() {
    let a = std::thread::spawn(|| stage_core_json("bip341_wallet_vectors.json"));
    let b = std::thread::spawn(|| stage_core_json("bip341_wallet_vectors.json"));
    let pa = a.join().expect("a");
    let pb = b.join().expect("b");
    assert_eq!(pa, pb);
    assert!(pa.is_file());
}

#[test]
fn stages_bip341_wallet_vectors_from_submodule() {
    let p = stage_core_json("bip341_wallet_vectors.json");
    assert!(p.is_file(), "staged {p:?}");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert!(v.get("version").is_some(), "expected version at {p:?}");
    assert!(
        v.get("scriptPubKey").is_some(),
        "expected scriptPubKey at {p:?}"
    );
}

#[test]
#[should_panic]
fn unknown_core_json_name_panics() {
    let _ = stage_core_json("not-a-core-file.json");
}

#[test]
fn in_tree_core_json_copies_are_gone() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in [
        "script_tests.json",
        "tx_valid.json",
        "tx_invalid.json",
        "sighash.json",
        "bip341_wallet_vectors.json",
    ] {
        let p = fixtures.join(name);
        assert!(
            !p.exists(),
            "do not check in {p:?}; cargo test stages from the submodule"
        );
    }
}
