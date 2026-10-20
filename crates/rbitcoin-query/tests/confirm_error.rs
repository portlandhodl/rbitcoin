use rbitcoin_consensus::ConsensusError;
use rbitcoin_query::confirm_error::*;
use rbitcoin_query::ConfirmError;
use rbitcoin_store::StoreError;
use std::fmt;

#[test]
fn uring_session_fault_covers_leftover_and_submit() {
    for m in [
        "invariant: io_uring leftover cqe",
        "io_uring submit failed",
        "invariant: io_uring undrained",
    ] {
        let e = ConfirmError::Store(StoreError::Corrupt(m));
        assert!(e.is_uring_session_fault(), "{m}");
    }
    assert!(!ConfirmError::Store(StoreError::NotFound).is_uring_session_fault());
    assert!(!ConfirmError::Rules(ConsensusError::PrevoutSpent).is_uring_session_fault());
}

#[test]
fn archive_unresolved_parent_is_missing_prevout_not_corrupt() {
    let e = ConfirmError::from(StoreError::Corrupt(
        "archive: parent create_fk unresolved (contiguous batch required)",
    ));
    assert!(
        matches!(e, ConfirmError::Rules(ConsensusError::MissingPrevout)),
        "got {e:?}"
    );
    assert!(!e.to_string().contains("corrupt"));
}

#[test]
fn from_store_error_keeps_not_found() {
    let e = ConfirmError::from(StoreError::NotFound);
    assert!(matches!(e, ConfirmError::Store(StoreError::NotFound)));
}

#[test]
fn cancelled_stays_a_store_error() {
    let e = ConfirmError::from(StoreError::Cancelled("stop"));
    assert!(matches!(
        e,
        ConfirmError::Store(StoreError::Cancelled("stop"))
    ));
    assert!(e.is_cancelled());
    assert_eq!(e.to_string(), "store: cancelled: stop");
    assert!(matches!(e.into_pool(), ConsensusError::Cancelled));
}
