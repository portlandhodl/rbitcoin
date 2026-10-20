use rbitcoin_primitives::Fk;
use rbitcoin_query::scripthash::*;
use rbitcoin_query::*;
use rbitcoin_store::{output_flags, script_hash, spent_abs, IdxBodyMode, SpendSlotRun};
use std::sync::Arc;

fn item(height: i64, txid0: u8) -> ScriptHashHistoryItem {
    let mut txid = [0u8; 32];
    txid[0] = txid0;
    ScriptHashHistoryItem {
        height,
        txid,
        tx_fk: Fk::NULL,
        fee: None,
    }
}

#[test]
fn open_filter_keeps_all_height_asc() {
    let items = vec![item(10, 1), item(5, 2), item(20, 3)];
    let got = apply_history_filter(&items, &HistoryFilter::open());
    assert!(!got.cursor_missing);
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.rows[0].height, 5);
    assert_eq!(got.rows[1].height, 10);
    assert_eq!(got.rows[2].height, 20);
}

#[test]
fn height_window_inclusive_from_exclusive_to() {
    let items = vec![item(1, 1), item(5, 2), item(10, 3), item(15, 4)];
    let f = HistoryFilter::height_window(5, Some(15));
    let got = apply_history_filter(&items, &f);
    assert_eq!(
        got.rows.iter().map(|i| i.height).collect::<Vec<_>>(),
        vec![5, 10]
    );
}

#[test]
fn height_window_open_upper() {
    let items = vec![item(1, 1), item(100, 2)];
    let f = HistoryFilter::height_window(50, None);
    let got = apply_history_filter(&items, &f);
    assert_eq!(got.rows.len(), 1);
    assert_eq!(got.rows[0].height, 100);
}

#[test]
fn newest_first_order() {
    let items = vec![item(1, 1), item(3, 2), item(2, 3)];
    let mut f = HistoryFilter::open();
    f.order = HistoryOrder::NewestFirst;
    let got = apply_history_filter(&items, &f);
    assert_eq!(
        got.rows.iter().map(|i| i.height).collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
}

#[test]
fn after_txid_skips_through_cursor_then_limit() {
    // Newest first: h=30,20,10 with distinct txids
    let items = vec![item(10, 1), item(20, 2), item(30, 3)];
    let mut after = [0u8; 32];
    after[0] = 3; // tip (height 30)
    let f = HistoryFilter {
        from_height: 0,
        to_height: None,
        limit: Some(1),
        after_txid: Some(after),
        order: HistoryOrder::NewestFirst,
        after_absent_keeps_older: false,
        after_height: None,
    };
    let got = apply_history_filter(&items, &f);
    assert!(!got.cursor_missing);
    assert_eq!(got.rows.len(), 1);
    assert_eq!(got.rows[0].height, 20);
    assert_eq!(got.rows[0].txid[0], 2);
}

/// A cursor that is not in this history is not the first page. Serving
/// those rows made `?after_txid=` of some other confirmed tx restart
/// the wallet at the newest row.
#[test]
fn after_txid_missing_is_not_the_first_page() {
    let items = vec![item(10, 1), item(20, 2)];
    let f = HistoryFilter {
        from_height: 0,
        to_height: None,
        limit: Some(1),
        after_txid: Some([0xff; 32]),
        order: HistoryOrder::NewestFirst,
        after_absent_keeps_older: false,
        after_height: None,
    };
    let got = apply_history_filter(&items, &f);
    assert!(got.cursor_missing);
    assert!(got.rows.is_empty());
}

#[test]
fn esplora_chain_page_defaults() {
    let f = HistoryFilter::esplora_chain_page(None);
    assert_eq!(f.limit, Some(25));
    assert_eq!(f.order, HistoryOrder::NewestFirst);
    assert!(f.after_txid.is_none());
}

#[test]
fn limit_truncates_after_window() {
    let items: Vec<_> = (1..=10).map(|h| item(h, h as u8)).collect();
    let f = HistoryFilter {
        from_height: 0,
        to_height: None,
        limit: Some(3),
        after_txid: None,
        order: HistoryOrder::HeightAsc,
        after_absent_keeps_older: false,
        after_height: None,
    };
    let got = apply_history_filter(&items, &f);
    assert_eq!(got.rows.len(), 3);
    assert_eq!(got.rows[0].height, 1);
    assert_eq!(got.rows[2].height, 3);
}
