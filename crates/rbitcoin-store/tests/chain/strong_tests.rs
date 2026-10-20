use rbitcoin_primitives::{Fk, Height, TableKind};
use rbitcoin_store::array_table::ArrayTable;
use rbitcoin_store::chain::*;
use rbitcoin_store::error::StoreError;
use std::path::Path;

fn tmp() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rbitcoin-strong-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn strong_bitset_range_and_clear() {
    let dir = tmp();
    let t = StrongTxTable::create(&dir).unwrap();
    t.set_strong_range(Fk(1), 10, Fk(99)).unwrap();
    for i in 1..=10 {
        assert!(t.is_strong(Fk(i)).unwrap());
    }
    assert!(!t.is_strong(Fk(11)).unwrap());
    t.set_unstrong(Fk(5)).unwrap();
    assert!(!t.is_strong(Fk(5)).unwrap());
    assert!(t.is_strong(Fk(4)).unwrap());
    assert!(t.get(Fk(1)).unwrap().is_some());
    assert!(t.get(Fk(5)).unwrap().is_none());
    t.flush().unwrap();
    drop(t);
    let t = StrongTxTable::open(&dir).unwrap();
    assert!(t.is_strong(Fk(1)).unwrap());
    assert!(!t.is_strong(Fk(5)).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn count_strong_in_fk_range_skips_zero_bytes() {
    let dir = tmp();
    let t = StrongTxTable::create(&dir).unwrap();
    assert_eq!(t.count_strong_in_fk_range(0, 8).unwrap(), 0);
    assert_eq!(t.count_strong_in_fk_range(5, 5).unwrap(), 0);
    assert_eq!(t.count_strong_in_fk_range(100, 200).unwrap(), 0);
    t.set_strong(Fk(1), Fk(1)).unwrap();
    t.set_strong(Fk(16), Fk(1)).unwrap();
    assert_eq!(t.count_strong_in_fk_range(1, 17).unwrap(), 2);
    assert_eq!(t.count_strong_in_fk_range(2, 16).unwrap(), 0);
    t.flush().unwrap();
    drop(t);
    let t = StrongTxTable::open(&dir).unwrap();
    assert_eq!(t.count_strong_in_fk_range(1, 17).unwrap(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Count uses the L2 image. A scribbled `strong_tx.body` does not change it.
#[test]
fn strong_tx_count_reads_l2_not_the_file() {
    let dir = tmp();
    let t = StrongTxTable::create(&dir).unwrap();
    t.set_strong_range(Fk(1), 20, Fk(1)).unwrap();
    t.flush().unwrap();
    assert!(t.l2_resident_bytes() > 0);
    assert_eq!(t.count_strong_in_fk_range(1, 21).unwrap(), 20);
    let path = dir.join("strong_tx.body");
    let mut bytes = std::fs::read(&path).unwrap();
    let header = rbitcoin_store::file::FILE_HEADER_LEN;
    assert!(bytes.len() > header);
    for b in &mut bytes[header..] {
        *b = 0;
    }
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(t.count_strong_in_fk_range(1, 21).unwrap(), 20);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `strong_tx` stays L2 even when the Class C array cap would demote it.
#[test]
fn strong_tx_always_l2_above_class_c_cap() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("RBITCOIN_CLASS_C_INRAM_MAX_MB");
    std::env::set_var("RBITCOIN_CLASS_C_INRAM_MAX_MB", "1");
    let dir = tmp();
    // 1 MiB + 1 byte of bits → body > class_c_inram_max_bytes() (1 MiB).
    let fk = Fk(8_388_608 + 8);
    {
        let t = StrongTxTable::create(&dir).unwrap();
        t.set_strong(fk, Fk(1)).unwrap();
        t.flush().unwrap();
        assert!(
            t.l2_resident_bytes() > 1024 * 1024,
            "create path L2={}",
            t.l2_resident_bytes()
        );
    }
    let t = StrongTxTable::open(&dir).unwrap();
    assert!(
        t.l2_resident_bytes() > 1024 * 1024,
        "reopen must keep L2 under a 1 MiB Class C cap; got {}",
        t.l2_resident_bytes()
    );
    assert!(t.is_strong(fk).unwrap());
    match prev {
        Some(v) => std::env::set_var("RBITCOIN_CLASS_C_INRAM_MAX_MB", v),
        None => std::env::remove_var("RBITCOIN_CLASS_C_INRAM_MAX_MB"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_set_strong_during_flush_persists() {
    use std::sync::atomic::{AtomicBool, Ordering as AtomOrd};
    use std::sync::Arc;
    let dir = tmp();
    let t = Arc::new(StrongTxTable::create(&dir).unwrap());
    t.set_strong_range(Fk(1), 80_000, Fk(1)).unwrap();
    t.flush().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flush_t = {
        let t = Arc::clone(&t);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(AtomOrd::Relaxed) {
                t.flush_dirty().unwrap();
            }
        })
    };
    for _ in 0..2_000 {
        t.set_unstrong(Fk(1)).unwrap();
        t.set_strong(Fk(1), Fk(1)).unwrap();
    }
    stop.store(true, AtomOrd::Relaxed);
    flush_t.join().unwrap();
    let want = t.is_strong(Fk(1)).unwrap();
    t.flush().unwrap();
    drop(t);
    let t = StrongTxTable::open(&dir).unwrap();
    assert_eq!(t.is_strong(Fk(1)).unwrap(), want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strong_write_behind_no_flush_keeps_old() {
    let dir = tmp();
    {
        let t = StrongTxTable::create(&dir).unwrap();
        t.set_strong_range(Fk(1), 8, Fk(1)).unwrap();
        t.flush().unwrap();
        t.set_strong(Fk(9), Fk(1)).unwrap();
        // Drop without flush.
    }
    let t = StrongTxTable::open(&dir).unwrap();
    assert!(t.is_strong(Fk(1)).unwrap());
    assert!(
        !t.is_strong(Fk(9)).unwrap(),
        "unflushed strong must not survive"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// High-fk-only dirties must write a suffix, not the full L2 image.
#[test]
fn strong_high_fk_flush_writes_suffix_not_full_image() {
    let dir = tmp();
    let t = StrongTxTable::create(&dir).unwrap();
    // 80_000 bits = 10_000 payload bytes.
    t.set_strong_range(Fk(1), 80_000, Fk(1)).unwrap();
    t.flush_dirty().unwrap();
    t.set_unstrong(Fk(80_000)).unwrap();
    t.flush_dirty().unwrap();
    assert!(t.is_strong(Fk(1)).unwrap());
    assert!(!t.is_strong(Fk(80_000)).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

/// In-prefix high-bit dirties must flush without losing low bits (suffix write).
#[test]
fn strong_suffix_dirty_flush_preserves_prefix() {
    let dir = tmp();
    {
        let s = StrongTxTable::create(&dir).unwrap();
        // Allocate a wide bit image then flush so disk_bytes covers it.
        s.set_strong_range(Fk(1), 10_000, Fk(1)).unwrap();
        s.flush().unwrap();
        // Flip only high bits (still within disk image) — suffix dirty path.
        s.set_strong_range(Fk(9000), 100, Fk(1)).unwrap();
        s.flush().unwrap();
    }
    let s = StrongTxTable::open(&dir).unwrap();
    assert!(s.is_strong(Fk(1)).unwrap());
    assert!(s.is_strong(Fk(5000)).unwrap());
    assert!(s.is_strong(Fk(9000)).unwrap());
    assert!(s.is_strong(Fk(9099)).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strong_write_behind_flush_reopen() {
    let dir = tmp();
    {
        let t = StrongTxTable::create(&dir).unwrap();
        t.set_strong_range(Fk(1), 16, Fk(1)).unwrap();
        t.flush().unwrap();
        t.set_unstrong_range(Fk(5), 4).unwrap();
        t.flush().unwrap();
    }
    let t = StrongTxTable::open(&dir).unwrap();
    assert!(t.is_strong(Fk(1)).unwrap());
    assert!(!t.is_strong(Fk(5)).unwrap());
    assert!(!t.is_strong(Fk(8)).unwrap());
    assert!(t.is_strong(Fk(9)).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strong_partial_bytes_unstrong_range_and_errors() {
    let dir = tmp();
    let t = StrongTxTable::create(&dir).unwrap();
    // Unaligned start so partial-first-byte path runs.
    t.set_strong_range(Fk(3), 20, Fk(1)).unwrap();
    for i in 3..23 {
        assert!(t.is_strong(Fk(i)).unwrap(), "i={i}");
    }
    assert!(!t.is_strong(Fk(2)).unwrap());
    // count==0 no-op
    t.set_strong_range(Fk(1), 0, Fk(1)).unwrap();
    t.set_unstrong_range(Fk(1), 0).unwrap();
    // Clear a mid-range with partial ends.
    t.set_unstrong_range(Fk(5), 10).unwrap();
    for i in 5..15 {
        assert!(!t.is_strong(Fk(i)).unwrap());
    }
    assert!(t.is_strong(Fk(4)).unwrap());
    assert!(t.is_strong(Fk(15)).unwrap());
    // past allocated → no-op
    t.set_unstrong(Fk(9999)).unwrap();
    t.set_unstrong_range(Fk(9000), 10).unwrap();
    assert!(matches!(
        t.set_strong(Fk::NULL, Fk(1)),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        t.set_strong(Fk(1), Fk::NULL),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        t.set_strong_range(Fk::NULL, 1, Fk(1)),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(
        t.set_strong_range(Fk(1), 1, Fk::NULL),
        Err(StoreError::InvalidFk)
    ));
    assert!(matches!(t.is_strong(Fk::NULL), Err(StoreError::InvalidFk)));
    t.flush_async().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
