use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::{OutPoint, Transaction, Txid};
use rbitcoin_net::MempoolHub;
use rbitcoin_primitives::Height;
use rbitcoin_rpc::methods::mempool::*;
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};

/// A session answers only once fee history or flow is warm (thousands
/// of blocks or a minute of relay), so the success shape is pinned here.
#[test]
fn an_estimate_is_feerate_and_blocks_floored_at_mempoolminfee() {
    let r = smart_fee_json(2, 0.000_020_00, 1_000);
    assert_eq!(r["feerate"], sat_btc_json(2_000), "{r}");
    assert_eq!(r["blocks"], 2, "{r}");
    assert_eq!(
        r.as_object().unwrap().len(),
        2,
        "only feerate and blocks: {r}"
    );
    let floored = smart_fee_json(2, 0.000_001_00, 5_000);
    assert_eq!(floored["feerate"], sat_btc_json(5_000), "{floored}");
    let none = smart_fee_json(6, -1.0, 5_000);
    assert!(none.get("feerate").is_none(), "{none}");
    assert_eq!(none["blocks"], 6, "{none}");
    let off = smart_fee_json(12, 0.000_010_50, 100);
    assert_eq!(off["blocks"], 12, "{off}");
    assert!(off.get("feerate").is_some(), "{off}");
    assert!(off.get("errors").is_none(), "{off}");
    let far = smart_fee_json(100, 0.000_008_00, 100);
    assert_eq!(far["blocks"], 100, "{far}");
    assert!(far.get("feerate").is_some(), "{far}");
}
