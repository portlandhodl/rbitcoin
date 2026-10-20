use rbitcoin_node::config::NodeConfig;
use rbitcoin_node::error::NodeError;
use rbitcoin_node::lock::*;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("rbitcoin-dirlock-{n}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn lock_file_does_not_follow_a_symlink() {
    let dir = tmp();
    let outside = tmp();
    let link = dir.join(".lock");
    std::os::unix::fs::symlink(outside.join("elsewhere.lock"), &link).unwrap();
    let err = lock_dir(&dir).expect_err("symlink lock");
    match err {
        NodeError::Datadir { .. } => {}
        other => panic!("expected datadir open failure, got {other}"),
    }
    assert!(
        !outside.join("elsewhere.lock").exists(),
        "open followed the symlink"
    );
}

#[test]
fn second_lock_on_same_datadir_is_busy() {
    let dir = tmp();
    let cfg = NodeConfig::default().with_datadir(&dir).with_tiny_heads();
    let _held = lock_node_dirs(&cfg).expect("first lock");
    let err = lock_node_dirs(&cfg).expect_err("second lock");
    match err {
        NodeError::Locked(ref p) => assert_eq!(*p, dir, "locked {p:?}"),
        other => panic!("expected Locked, got {other}"),
    }
    assert!(
        format!("{err}").contains("Cannot obtain a lock on directory"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lock_does_not_create_core_blocks_dir() {
    let dir = tmp();
    let cfg = NodeConfig::default().with_datadir(&dir).with_tiny_heads();
    let _held = lock_node_dirs(&cfg).expect("lock");
    assert!(
        !dir.join("blocks").exists(),
        "rbitcoin has no Core blocks/ product"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
