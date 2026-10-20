use rbitcoin_query::confirm_stats::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[test]
fn last_union_miss_roundtrips_miss_on_codes() {
    let stats = ConfirmStats::default();
    assert_eq!(stats.last_union_miss().n, 0);
    let tid = [7u8; 32];
    for (on, want) in [
        (Some("head"), Some("head")),
        (Some("body"), Some("body")),
        (Some("idx"), Some("idx")),
        (Some("fence"), Some("fence")),
        (Some("nope"), None),
        (None, None),
    ] {
        stats.note_union_miss(tid, 3, true, on, 9);
        let got = stats.last_union_miss();
        assert_eq!(got.n, 3);
        assert!(got.pending);
        assert_eq!(got.txid, Some(tid));
        assert_eq!(got.miss_on, want, "on={on:?}");
        assert_eq!(got.miss_cands, 9);
    }
}
