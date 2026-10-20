use rbitcoin_primitives::PackError;
use rbitcoin_store::error::*;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[test]
fn display_and_source_arms() {
    let io = StoreError::io("/tmp/x", io::Error::new(io::ErrorKind::NotFound, "nope"));
    let s = io.to_string();
    assert!(s.contains("io error"));
    assert!(s.contains("/tmp/x"));
    assert!(io.source().is_some());

    let arms: Vec<StoreError> = vec![
        StoreError::BadMagic,
        StoreError::BadSchema(9),
        StoreError::BadKind {
            expected: 1,
            got: 2,
        },
        StoreError::NotFound,
        StoreError::InvalidFk,
        StoreError::NotDirectory(PathBuf::from("/not/a/dir")),
        StoreError::Corrupt("broken"),
        StoreError::BudgetFull("block_queue"),
        StoreError::Cancelled("stop"),
        StoreError::Unavailable,
        StoreError::Layout("seqsigwit is on a cold datadir".into()),
        StoreError::Stale("chain view moved"),
        StoreError::Rejected("block wire does not decode"),
        StoreError::Pruned { height: 12 },
        StoreError::ShCreatesCap {
            creates: 14437,
            cap: 10000,
        },
    ];
    let texts: Vec<String> = arms.iter().map(|e| e.to_string()).collect();
    assert_eq!(texts[0], "invalid store magic");
    assert!(texts[1].contains("unsupported schema version 9"));
    assert!(texts[2].contains("expected 1"));
    assert!(texts[2].contains("got 2"));
    assert_eq!(texts[3], "record not found");
    assert_eq!(texts[4], "invalid foreign key");
    assert!(texts[5].contains("not a directory"));
    assert!(texts[6].contains("corrupt record: broken"));
    assert!(texts[7].contains("budget full: block_queue"));
    assert!(texts[8].contains("cancelled: stop"));
    assert_eq!(texts[9], "io_uring unavailable");
    assert_eq!(texts[10], "seqsigwit is on a cold datadir");
    assert_eq!(texts[11], "chain view moved");
    assert_eq!(texts[12], "block wire does not decode");
    assert_eq!(texts[13], "pruned data at height 12");
    assert_eq!(
        texts[14],
        "scripthash join exceeds --max-sh-creates (14437 creates, cap 10000)"
    );
    for e in &arms {
        assert!(e.source().is_none());
    }

    let sq = StoreError::BudgetFull("io_session SQ (in_flight cap)");
    assert!(sq.is_io_backpressure());
    assert!(
        !sq.to_string().contains("corrupt"),
        "SQ full must not print as corrupt record: {}",
        sq
    );
    assert!(!StoreError::Corrupt("broken").is_io_backpressure());
}

#[test]
fn uring_session_fault_strings() {
    for m in [
        "invariant: io_uring undrained",
        "invariant: io_uring wait timeout",
        "invariant: io_uring session poisoned",
        "invariant: io_uring unexpected cqe",
        "invariant: io_uring cq overflow",
        "io_uring submit_and_wait failed",
        "invariant: io_uring leftover cqe",
        "io_uring submit failed",
        "invariant: io_uring held pread failed",
    ] {
        assert!(StoreError::Corrupt(m).is_uring_session_fault(), "{m}");
    }
    assert!(!StoreError::Corrupt("invariant: held pread failed").is_uring_session_fault());
    assert!(!StoreError::BudgetFull("io_uring SQ").is_uring_session_fault());
    assert!(!StoreError::Unavailable.is_uring_session_fault());
    assert!(!StoreError::Corrupt("io_uring is Linux-only").is_uring_session_fault());
    assert!(!StoreError::Corrupt("broken").is_uring_session_fault());
    assert!(!StoreError::Io {
        path: PathBuf::from("/tmp/x"),
        source: io::Error::other("disk"),
    }
    .is_uring_session_fault());
}

#[test]
fn pack_error_is_corrupt() {
    let e: StoreError = PackError("compact size empty").into();
    assert!(matches!(e, StoreError::Corrupt("compact size empty")));
}
