use rbitcoin_primitives::{Fk, TableKind, SCHEMA_VERSION};
use rbitcoin_store::delta_loc::*;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::file::{
    leading_header_bytes, write_synced_tmp_rename, GrowPolicy, TableFile, FILE_HEADER_LEN,
};
use rbitcoin_store::testutil::TempDir;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

#[test]
fn window_and_within() {
    assert_eq!(loc_window(1), 0);
    assert_eq!(loc_within(1), 0);
    assert_eq!(loc_window(1024), 0);
    assert_eq!(loc_within(1024), 1023);
    assert_eq!(loc_window(1025), 1);
    assert_eq!(loc_within(1025), 0);
}

#[test]
fn pack_create_n_out_zero_is_corrupt() {
    match pack_create_pair(1, 0) {
        Err(StoreError::Corrupt(m)) => assert!(m.contains("create n_out"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn pack_create_sentinels() {
    let (s, n, ovf) = pack_create_pair(1, 1).unwrap();
    assert_eq!((s, n, ovf), (1, 1, None));
    let (s, n, ovf) = pack_create_pair(255, 255).unwrap();
    assert_eq!((s, n, ovf), (255, 255, None));
    let (s, n, ovf) = pack_create_pair(256, 1).unwrap();
    assert_eq!((s, n), (0, 1));
    assert_eq!(ovf, Some((256, 1)));
    let (s, n, ovf) = pack_create_pair(1, 256).unwrap();
    assert_eq!((s, n), (1, 0));
    assert_eq!(ovf, Some((1, 256)));
}

#[test]
fn pack_create_pair_past_u16_is_ok() {
    let (s, n, ovf) = pack_create_pair(65536, 1).expect("strides 65536");
    assert_eq!((s, n), (0, 1));
    let (st, no) = ovf.expect("ovf");
    assert_eq!(st, 65536);
    assert_eq!(no, 1);
    let (s, n, ovf) = pack_create_pair(1, 65536).expect("n_out 65536");
    assert_eq!((s, n), (1, 0));
    let (st, no) = ovf.expect("ovf");
    assert_eq!(st, 1);
    assert_eq!(no, 65536);
    let (s, n, ovf) = pack_create_pair(65536, 65536).expect("both");
    assert_eq!((s, n), (0, 0));
    let (st, no) = ovf.expect("ovf");
    assert_eq!(st, 65536);
    assert_eq!(no, 65536);
    let (s, n, ovf) = pack_create_pair(65535, 65535).unwrap();
    assert_eq!((s, n), (0, 0));
    let (st, no) = ovf.expect("ovf");
    assert_eq!(st, 65535);
    assert_eq!(no, 65535);
}

#[test]
fn delta_loc_append_and_batch_order() {
    let dir = TempDir::labeled("delta-loc").unwrap();
    let loc = DeltaLoc::create(dir.path(), "seqsigwit").unwrap();
    let starts: Vec<u64> = (0..4).map(|i| FILE_HEADER_LEN as u64 + i * 16).collect();
    loc.append(&starts, &[16, 16, 16, 16]).unwrap();
    assert_eq!(loc.count(), 4);
    let fks = [Fk(3), Fk(1), Fk(4), Fk::NULL, Fk(99)];
    let got = loc.range_batch(&fks).unwrap();
    assert_eq!(got[0], Some((FILE_HEADER_LEN as u64 + 32, 16)));
    assert_eq!(got[1], Some((FILE_HEADER_LEN as u64, 16)));
    assert_eq!(got[2], Some((FILE_HEADER_LEN as u64 + 48, 16)));
    assert_eq!(got[3], None);
    assert_eq!(got[4], None);
}

#[test]
fn delta_loc_u16_overflow_and_missing() {
    let dir = TempDir::labeled("delta-ovf").unwrap();
    let loc = DeltaLoc::create(dir.path(), "seqsigwit").unwrap();
    let fat = 65536 * IDX_STRIDE;
    loc.append(&[FILE_HEADER_LEN as u64], &[fat]).unwrap();
    let got = loc.range_batch(&[Fk(1)]).unwrap();
    assert_eq!(got[0], Some((FILE_HEADER_LEN as u64, fat)));
    drop(loc);
    std::fs::remove_file(dir.path().join("seqsigwit.loc.ovf")).unwrap();
    let loc = DeltaLoc::open(dir.path(), "seqsigwit").unwrap();
    match loc.range_batch(&[Fk(1)]) {
        Err(StoreError::Corrupt(m)) => assert!(m.contains("overflow missing"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn pack_u16_seqsigwit_overflow_at_512kib() {
    let (d, ovf) = pack_u16_strides_seqsigwit(1).unwrap();
    assert_eq!((d, ovf), (1, None));
    let (d, ovf) = pack_u16_strides_seqsigwit(65535).unwrap();
    assert_eq!((d, ovf), (65535, None));
    let (d, ovf) = pack_u16_strides_seqsigwit(65536).unwrap();
    assert_eq!(d, 0);
    assert_eq!(ovf, Some(65536));
}
