use rbitcoin_mempool::error::*;
use rbitcoin_primitives::PackError;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[test]
fn display_and_source_all_variants() {
    let io_err = MempoolError::io("/tmp/mp", io::Error::new(io::ErrorKind::NotFound, "nope"));
    assert!(format!("{io_err}").contains("io /tmp/mp"));
    assert!(io_err.source().is_some());

    let magic = MempoolError::BadMagic;
    assert_eq!(format!("{magic}"), "mempool bad magic");
    assert!(magic.source().is_none());

    let schema = MempoolError::BadSchema(3);
    assert_eq!(format!("{schema}"), "mempool bad schema 3");
    assert!(schema.source().is_none());

    let corrupt = MempoolError::Corrupt("slot OOB");
    assert_eq!(format!("{corrupt}"), "mempool corrupt: slot OOB");
    assert!(corrupt.source().is_none());

    let full = MempoolError::Full;
    assert_eq!(format!("{full}"), "mempool full (no free slots)");
    assert!(full.source().is_none());

    let from_pack: MempoolError = PackError("compact size empty").into();
    assert!(matches!(
        from_pack,
        MempoolError::Corrupt("compact size empty")
    ));
}
