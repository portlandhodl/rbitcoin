use rbitcoin_store::testutil::*;
use rbitcoin_store::Store;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn copy_store_tree_copies_nested_files() {
    let src = TempDir::labeled("copy-src").unwrap();
    let nested = src.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("page"), b"store-bytes").unwrap();
    let dst_root = TempDir::labeled("copy-dst").unwrap();
    let dst = dst_root.path().join("out");
    copy_store_tree(src.path(), &dst);
    let got = std::fs::read(dst.join("nested/page")).unwrap();
    assert_eq!(got, b"store-bytes");
}
