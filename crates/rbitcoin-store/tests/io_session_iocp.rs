#![cfg(windows)]
use rbitcoin_store::error::StoreError;
use rbitcoin_store::io_handle::IoHandle;
use rbitcoin_store::io_session_iocp::*;
use std::collections::{HashSet, VecDeque};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::ptr::null_mut;

const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rbitcoin-iocp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

fn open_overlapped(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path)
        .expect("overlapped tmp")
}

#[test]
fn associate_twice_same_port_ok() {
    let path = tmp("twice");
    let f = open_overlapped(&path);
    let h = IoHandle::from_file(&f);
    assert!(h.pwrite(0, b"ab") > 0);
    let mut eng = IocpEngine::open(8).unwrap();
    eng.associate(h).unwrap();
    eng.associate(h).unwrap();
    drop(f);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn associate_non_overlapped_is_error() {
    let path = tmp("sync");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("sync tmp");
    let h = IoHandle::from_file(&f);
    let mut eng = IocpEngine::open(8).unwrap();
    let err = eng.associate(h).unwrap_err();
    let s = err.to_string();
    assert!(
        s.contains("FILE_FLAG_OVERLAPPED"),
        "expected overlapped hint, got {s}"
    );
    drop(f);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn positional_pread_on_associated_handle_skips_completion_port() {
    let path = tmp("xfer-skip");
    let f = open_overlapped(&path);
    let h = IoHandle::from_file(&f);
    assert!(h.pwrite(0, b"hello") > 0);
    let mut eng = IocpEngine::open(8).unwrap();
    eng.associate(h).unwrap();
    let mut buf = [0u8; 5];
    assert_eq!(h.pread(0, &mut buf), 5);
    assert_eq!(&buf, b"hello");
    let extra = eng.harvest_ready();
    assert!(
        extra.is_empty(),
        "positional xfer must not post OVERLAPPED to IOCP (harvest would Box::from_raw a stack frame): {extra:?}"
    );
    drop(f);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn two_threads_bulk_pread_same_handle_completes() {
    use rbitcoin_store::bulk_io::{pread_batch, ReadOp};
    use std::sync::atomic::{AtomicU32, Ordering};

    let path = tmp("two-thread-pread");
    let f = open_overlapped(&path);
    let h = IoHandle::from_file(&f);
    assert!(h.pwrite(0, b"hello") > 0);
    let done = AtomicU32::new(0);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let mut buf = [0u8; 5];
                let mut ops = [ReadOp {
                    fd: h,
                    offset: 0,
                    buf: &mut buf,
                    result: i32::MIN,
                }];
                pread_batch(&mut ops);
                assert!(
                    ops[0].result >= 5,
                    "bulk pread must complete (not drain-hang) result={}",
                    ops[0].result
                );
                assert_eq!(&buf, b"hello");
                done.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(done.load(Ordering::Relaxed), 2);
    drop(f);
    let _ = std::fs::remove_file(&path);
}
