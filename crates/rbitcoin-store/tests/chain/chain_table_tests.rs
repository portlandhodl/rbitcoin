use rbitcoin_primitives::{Fk, Height, TableKind};
use rbitcoin_store::array_table::ArrayTable;
use rbitcoin_store::chain::*;
use rbitcoin_store::error::StoreError;
use std::path::Path;

fn tmp() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rbitcoin-chain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[allow(clippy::cognitive_complexity)] // one fixture, many chain surface arms
#[test]
fn confirmed_tx_height_header_txs_surface() {
    let dir = tmp();
    // Confirmed
    let c = ConfirmedTable::create(&dir).unwrap();
    assert!(c.tip_height().is_none());
    c.set(Height(0), Fk(1)).unwrap();
    c.set(Height(1), Fk(2)).unwrap();
    assert_eq!(c.tip_height(), Some(Height(1)));
    assert_eq!(c.get(Height(0)).unwrap(), Some(Fk(1)));
    c.set_many(&[]).unwrap();
    c.set_many(&[(Height(2), Fk(3)), (Height(3), Fk(4))])
        .unwrap();
    assert_eq!(c.tip_height(), Some(Height(3)));
    assert!(matches!(
        c.set(Height(4), Fk::NULL),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        c.set_many(&[(Height(5), Fk::NULL)]),
        Err(StoreError::InvalidFk)
    ));
    c.disconnect_tip(Height(3)).unwrap();
    assert_eq!(c.tip_height(), Some(Height(2)));
    assert!(matches!(
        c.disconnect_tip(Height(0)),
        Err(StoreError::Corrupt(_))
    ));
    c.disconnect_tip(Height(2)).unwrap();
    c.disconnect_tip(Height(1)).unwrap();
    c.disconnect_tip(Height(0)).unwrap();
    assert!(c.tip_height().is_none());
    assert!(matches!(
        c.disconnect_tip(Height(0)),
        Err(StoreError::Corrupt(_))
    ));
    c.flush().unwrap();
    c.flush_async().unwrap();

    // HeaderTxs
    let ht = HeaderTxsTable::create(&dir).unwrap();
    assert!(ht.get_range(Fk(1)).unwrap().is_none());
    ht.put_range(Fk(1), Fk(100), 3).unwrap();
    assert_eq!(ht.get_range(Fk(1)).unwrap(), Some((Fk(100), 3)));
    assert_eq!(
        ht.get_list(Fk(1)).unwrap().unwrap(),
        vec![Fk(100), Fk(101), Fk(102)]
    );
    assert!(ht.has_body(Fk(1)).unwrap());
    assert!(!ht.has_body(Fk(2)).unwrap());
    ht.put_list(Fk(2), &[Fk(200), Fk(201)]).unwrap();
    assert!(matches!(
        ht.put_list(Fk(3), &[]),
        Err(StoreError::Corrupt(_))
    ));
    // Non-contiguous triggers debug_assert in debug builds; only empty is a
    // stable Err path under RUSTFLAGS=-Dwarnings debug tests.
    assert!(matches!(
        ht.put_range(Fk::NULL, Fk(1), 1),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        ht.put_range(Fk(3), Fk::NULL, 1),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        ht.put_range(Fk(3), Fk(1), 0),
        Err(StoreError::InvalidFk)
    ));
    ht.put_lists_batch(&[]).unwrap();
    ht.put_lists_batch(&[(Fk(3), &[Fk(300)] as &[_]), (Fk(4), &[Fk(400), Fk(401)])])
        .unwrap();
    assert!(matches!(
        ht.put_lists_batch(&[(Fk(5), &[] as &[_])]),
        Err(StoreError::Corrupt(_))
    ));
    ht.put_ranges_batch(&[]).unwrap();
    ht.put_ranges_batch(&[(Fk(5), Fk(500), 2)]).unwrap();
    assert!(matches!(
        ht.put_ranges_batch(&[(Fk::NULL, Fk(1), 1)]),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        ht.put_ranges_batch(&[(Fk(6), Fk::NULL, 1)]),
        Err(StoreError::InvalidFk)
    ));
    assert_eq!(ht.count_bodies().unwrap(), 5);
    assert!(matches!(ht.get_range(Fk::NULL), Err(StoreError::InvalidFk)));

    // clear_body: drop association without freeing tx rows (merkle soft-reget path).
    assert!(matches!(
        ht.clear_body(Fk::NULL),
        Err(StoreError::InvalidFk)
    ));
    assert!(!ht.clear_body(Fk(99)).unwrap(), "no body → false");
    assert!(ht.clear_body(Fk(1)).unwrap(), "had body → true");
    assert!(!ht.has_body(Fk(1)).unwrap());
    assert!(!ht.clear_body(Fk(1)).unwrap(), "already clear → false");
    assert_eq!(ht.count_bodies().unwrap(), 4);

    ht.flush().unwrap();
    ht.flush_async().unwrap();
    drop(ht);
    let ht = HeaderTxsTable::open(&dir).unwrap();
    assert_eq!(ht.get_range(Fk(5)).unwrap(), Some((Fk(500), 2)));
    assert!(
        !ht.has_body(Fk(1)).unwrap(),
        "clear_body durable after reopen"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn header_txs_contains_tx_range_edges() {
    let dir = tmp();
    let ht = HeaderTxsTable::create(&dir).unwrap();
    assert!(!ht.contains_tx(Fk(1), Fk::NULL).unwrap());
    assert!(!ht.contains_tx(Fk(1), Fk(1)).unwrap(), "no body");
    ht.put_range(Fk(1), Fk(10), 3).unwrap();
    assert!(ht.contains_tx(Fk(1), Fk(10)).unwrap());
    assert!(ht.contains_tx(Fk(1), Fk(12)).unwrap());
    assert!(!ht.contains_tx(Fk(1), Fk(9)).unwrap());
    assert!(!ht.contains_tx(Fk(1), Fk(13)).unwrap());
    assert!(!ht.contains_tx(Fk(2), Fk(10)).unwrap(), "other header");
    assert_eq!(ht.tx_index(Fk(1), Fk(10)).unwrap(), Some(0));
    assert_eq!(ht.tx_index(Fk(1), Fk(12)).unwrap(), Some(2));
    assert_eq!(ht.tx_index(Fk(1), Fk(9)).unwrap(), None);
    assert_eq!(ht.tx_index(Fk(1), Fk(13)).unwrap(), None);
    assert_eq!(ht.tx_index(Fk(1), Fk::NULL).unwrap(), None);
    assert_eq!(ht.tx_index(Fk(2), Fk(10)).unwrap(), None, "other header");
    let _ = std::fs::remove_dir_all(&dir);
}
