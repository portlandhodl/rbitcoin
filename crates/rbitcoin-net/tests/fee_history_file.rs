use bitcoin::hashes::{sha256d, Hash};
use rbitcoin_net::fee_history::HistoricalFeeBlock;
use rbitcoin_net::fee_history_file::*;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

fn dir() -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("rbitcoin-feehist-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn block(rate: Option<u64>) -> HistoricalFeeBlock {
    HistoricalFeeBlock {
        p10_sat_kvb: rate,
        txstat_bytes: 8,
    }
}

fn hash(n: u8) -> [u8; 32] {
    [n; 32]
}

#[test]
fn snapshot_and_journal_round_trip() {
    let d = dir();
    assert!(load(&d).unwrap().is_none(), "no file yet");
    let rows = [(10, block(Some(1_000))), (11, block(None))];
    let generation = write_snapshot(&d, &rows, &[(11, hash(11))]).unwrap();
    let mut j = start_journal(&d, generation).unwrap();
    append(&mut j, 12, &block(Some(2_000)), &hash(12)).unwrap();
    append(&mut j, 12, &block(Some(2_500)), &hash(13)).unwrap();
    drop(j);

    let loaded = load(&d).unwrap().unwrap();
    assert_eq!(loaded.rows, rows);
    assert_eq!(loaded.hashes, [(11, hash(11))]);
    assert_eq!(
        loaded.journal,
        [
            (12, block(Some(2_000)), hash(12)),
            (12, block(Some(2_500)), hash(13))
        ]
    );
    assert_eq!(loaded.journal_note, None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn snapshot_without_a_journal_is_a_clean_load() {
    let d = dir();
    write_snapshot(&d, &[(1, block(Some(500)))], &[]).unwrap();
    let loaded = load(&d).unwrap().unwrap();
    assert!(loaded.journal.is_empty());
    assert_eq!(loaded.journal_note, None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_torn_journal_keeps_the_records_before_the_tear() {
    let d = dir();
    let generation = write_snapshot(&d, &[(1, block(Some(500)))], &[]).unwrap();
    let mut j = start_journal(&d, generation).unwrap();
    append(&mut j, 2, &block(Some(600)), &hash(2)).unwrap();
    append(&mut j, 3, &block(Some(700)), &hash(3)).unwrap();
    j.write_all(&[7u8; 20]).unwrap();
    drop(j);
    let loaded = load(&d).unwrap().unwrap();
    assert_eq!(loaded.journal.len(), 2);
    assert!(loaded.journal_note.unwrap().contains("torn after 2"));

    // a flipped byte inside a record ends the replay there
    let path = d.join(JOURNAL_FILE);
    let mut raw = std::fs::read(&path).unwrap();
    raw[JOURNAL_HEADER + JOURNAL_RECORD + 9] ^= 1;
    std::fs::write(&path, raw).unwrap();
    assert_eq!(load(&d).unwrap().unwrap().journal.len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_journal_from_an_older_snapshot_is_not_replayed() {
    let d = dir();
    let old = write_snapshot(&d, &[(1, block(Some(500)))], &[]).unwrap();
    let mut j = start_journal(&d, old).unwrap();
    append(&mut j, 2, &block(Some(600)), &hash(2)).unwrap();
    drop(j);
    write_snapshot(&d, &[(1, block(Some(500))), (2, block(Some(600)))], &[]).unwrap();
    let loaded = load(&d).unwrap().unwrap();
    assert!(loaded.journal.is_empty());
    assert!(loaded.journal_note.unwrap().contains("older snapshot"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_damaged_or_foreign_snapshot_is_refused() {
    let d = dir();
    write_snapshot(&d, &[(1, block(Some(500)))], &[(1, hash(1))]).unwrap();
    let path = d.join(SNAPSHOT_FILE);
    let good = std::fs::read(&path).unwrap();

    let mut flipped = good.clone();
    flipped[SNAPSHOT_HEADER + 9] ^= 1;
    std::fs::write(&path, &flipped).unwrap();
    assert!(load(&d).unwrap_err().contains("checksum"));

    let mut v2 = good.clone();
    v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&path, &v2).unwrap();
    assert!(load(&d).unwrap_err().contains("version 2"));

    std::fs::write(&path, &good[..good.len() - 1]).unwrap();
    assert!(load(&d).unwrap_err().contains("length"));

    std::fs::write(&path, b"not a snapshot at all, but long enough").unwrap();
    assert!(load(&d).unwrap_err().contains("not a fee history"));

    std::fs::write(&path, &good[..16]).unwrap();
    assert!(
        load(&d).unwrap_err().contains("not a fee history"),
        "a header without the checksum trailer is not a snapshot"
    );
    write_snapshot(&d, &[], &[]).unwrap();
    let empty = load(&d).unwrap().unwrap();
    assert!(empty.rows.is_empty() && empty.hashes.is_empty());

    let generation = write_snapshot(&d, &[(1, block(Some(500)))], &[]).unwrap();
    drop(start_journal(&d, generation).unwrap());
    let exact = load(&d).unwrap().unwrap();
    assert!(exact.journal.is_empty());
    assert_eq!(exact.journal_note, None);
    let mut j = start_journal(&d, generation).unwrap();
    append(&mut j, 2, &block(Some(600)), &hash(2)).unwrap();
    drop(j);
    let jpath = d.join(JOURNAL_FILE);
    let mut raw = std::fs::read(&jpath).unwrap();
    raw[0] = b'X';
    std::fs::write(&jpath, &raw).unwrap();
    let foreign = load(&d).unwrap().unwrap();
    assert!(foreign.journal.is_empty(), "{foreign:?}");
    assert!(
        foreign.journal_note.unwrap().contains("bad header"),
        "a long journal with a bad magic is not replayed"
    );
    let _ = std::fs::remove_dir_all(&d);
}
