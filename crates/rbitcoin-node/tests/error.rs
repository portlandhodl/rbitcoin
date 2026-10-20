use rbitcoin_node::error::*;
use rbitcoin_primitives::ParseNetworkError;
use rbitcoin_store::StoreError;
use std::fmt;
use std::path::PathBuf;

use std::error::Error;
use std::io;

#[test]
fn display_source_and_from() {
    let cfg = NodeError::Config("bad".into());
    assert_eq!(format!("{cfg}"), "configuration error: bad");
    assert!(cfg.source().is_none());

    let init = NodeError::Init("peer-timeout must be a positive integer.".into());
    assert_eq!(
        format!("{init}"),
        "Error: peer-timeout must be a positive integer."
    );
    assert!(init.source().is_none());

    let fut = NodeError::FutureTip;
    assert_eq!(format!("{fut}"), FUTURE_BLOCK_DB_MSG);
    assert!(
        format!("{fut}").contains("more than two hours ahead of the node clock"),
        "{fut}"
    );
    assert!(
        !format!("{fut}").contains("reindex-chainstate"),
        "operator text must not name Core -reindex-chainstate: {fut}"
    );
    assert!(fut.source().is_none());

    let net: NodeError = ParseNetworkError { input: "x".into() }.into();
    assert!(format!("{net}").contains("unknown network"));
    assert!(net.source().is_some());

    let dd = NodeError::Datadir {
        path: PathBuf::from("/nope"),
        source: io::Error::new(io::ErrorKind::NotFound, "missing"),
    };
    assert!(format!("{dd}").contains("datadir error at /nope"));
    assert!(dd.source().is_some());

    let store: NodeError = StoreError::Corrupt("x").into();
    assert!(format!("{store}").contains("x"));
    // StoreError itself is the source for NodeError::Store.
    assert!(store.source().is_some());

    let locked = NodeError::Locked(PathBuf::from("/dd"));
    assert!(format!("{locked}").contains("Cannot obtain a lock on directory /dd"));
    assert!(locked.source().is_none());
}

#[test]
fn tip_too_far_in_future_is_strictly_beyond_two_hours() {
    assert!(!tip_too_far_in_future(1_000, 1_000));
    assert!(!tip_too_far_in_future(
        1_000 + MAX_FUTURE_BLOCK_TIME as u32,
        1_000
    ));
    assert!(tip_too_far_in_future(
        1_000 + MAX_FUTURE_BLOCK_TIME as u32 + 1,
        1_000
    ));
}
