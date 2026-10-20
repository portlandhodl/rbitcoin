use rbitcoin_primitives::Fk;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::fuse8_filter::{fuse_key_from_mixed, SealedFuse8};
use rbitcoin_store::io_handle::IoHandle;
use rbitcoin_store::scripthash_layout::{
    pack8_bytes, unpack8_bytes, ShHeadKey, ShHeadValue, SH_HEAD_KEY_LEN, SH_HEAD_SLOT_SIZE,
    SH_HEAD_VALUE_LEN,
};
use rbitcoin_store::scripthash_sorted_head::*;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "rbitcoin-shsort-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&p);
    p.join("head")
}

fn key_of(i: u32) -> ShHeadKey {
    let mut k = [0u8; 16];
    k[0..4].copy_from_slice(&i.to_be_bytes());
    k
}

fn recs(n: u32) -> Vec<(ShHeadKey, [u8; SH_HEAD_VALUE_LEN])> {
    (0..n)
        .map(|i| {
            let v = ShHeadValue::inline_one(Fk(u64::from(i) + 1));
            (key_of(i), pack8_bytes(&v).unwrap())
        })
        .collect()
}

#[test]
fn sorted_head_hit_miss_update() {
    let path = tmp();
    let n = 10_000u32;
    let recs = recs(n);
    let h = SortedHead::write(&path, &recs).unwrap();
    assert_eq!(h.count, u64::from(n));
    assert!(h.fuse.is_some());
    assert_eq!(
        h.fuse.as_ref().unwrap().fingerprint_heap_bytes(),
        0,
        "sorted head fuse is mapped, not the build Box"
    );
    assert_eq!(h.path(), path.as_path());
    assert!(path.is_file());
    assert!(idx_path(&path).is_file());
    assert!(fuse_path(&path).is_file());

    h.preads.store(0, Ordering::Relaxed);
    let got = h.get(&key_of(1234)).unwrap().unwrap();
    assert_eq!(got.inline_fks(), vec![Fk(1235)]);
    assert!(
        h.preads.load(Ordering::Relaxed) <= 2,
        "hit must be ≤2 preads, got {}",
        h.preads.load(Ordering::Relaxed)
    );

    h.preads.store(0, Ordering::Relaxed);
    assert!(h.get(&key_of(n + 10_000)).unwrap().is_none());

    let new_val = ShHeadValue::slab(0, 2, 4096);
    assert!(h.update_value(&key_of(7), &new_val).unwrap());
    assert_eq!(h.get(&key_of(7)).unwrap().unwrap(), new_val);
    assert!(!h.update_value(&key_of(n + 1), &new_val).unwrap());

    let h2 = SortedHead::open(&path).unwrap();
    assert!(h2.fuse.is_some());
    assert_eq!(h2.get(&key_of(7)).unwrap().unwrap(), new_val);

    let unsorted = vec![(key_of(2), recs[0].1), (key_of(1), recs[0].1)];
    assert!(SortedHead::write(path.with_extension("bad"), &unsorted).is_err());
    let junk = path.with_extension("junk");
    std::fs::write(&junk, b"XXXX").unwrap();
    assert!(matches!(SortedHead::open(&junk), Err(StoreError::BadMagic)));
    let mut bad_ver = std::fs::read(&path).unwrap();
    bad_ver[4..6].copy_from_slice(&99u16.to_le_bytes());
    let verp = path.with_extension("ver");
    std::fs::write(&verp, &bad_ver).unwrap();
    std::fs::copy(idx_path(&path), idx_path(&verp)).unwrap();
    std::fs::copy(fuse_path(&path), fuse_path(&verp)).unwrap();
    assert!(SortedHead::open(&verp).is_err());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(idx_path(&path));
    let _ = std::fs::remove_file(fuse_path(&path));
    let _ = std::fs::remove_file(&junk);
    let _ = std::fs::remove_file(&verp);
    let _ = std::fs::remove_file(idx_path(&verp));
    let _ = std::fs::remove_file(fuse_path(&verp));
}

#[test]
fn sorted_ovf_fuse_skips_pread_on_miss() {
    let path = tmp();
    let n = 10_000u32;
    let recs = recs(n);
    let h = SortedHead::write(&path, &recs).unwrap();
    assert!(h.fuse.is_some());
    assert!(fuse_path(&path).is_file());

    h.preads.store(0, Ordering::Relaxed);
    assert!(h.get(&key_of(1234)).unwrap().is_some());
    assert!(h.preads.load(Ordering::Relaxed) <= 2);

    let mut saw_fuse_miss = false;
    for extra in 0..2000u32 {
        let k = key_of(n + 10_000 + extra);
        h.preads.store(0, Ordering::Relaxed);
        let got = h.get(&k).unwrap();
        if got.is_none() && h.preads.load(Ordering::Relaxed) == 0 {
            saw_fuse_miss = true;
            break;
        }
        assert!(got.is_none(), "absent key must not decode as present");
    }
    assert!(
        saw_fuse_miss,
        "ovf fuse must skip data/idx IO on a true miss"
    );

    assert!(SortedHead::open(&path).unwrap().fuse.is_some());
    let _ = std::fs::remove_file(fuse_path(&path));
    assert!(SortedHead::open(&path).is_err());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(idx_path(&path));
}
