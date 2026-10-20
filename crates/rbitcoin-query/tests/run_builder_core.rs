use rbitcoin_query::run_builder_core::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[test]
fn clear_runs_dir_keeps_seal() {
    let dir = std::env::temp_dir().join(format!(
        "rbitcoin-runctrl-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let ctrl = RunControl::open(&dir, "sh.runs");

    let runs = ctrl.runs_dir.clone();
    std::fs::write(runs.join("SEAL"), b"keep").unwrap();
    std::fs::write(runs.join("1.run"), b"x").unwrap();
    std::fs::write(runs.join("junk.tmp"), b"y").unwrap();
    let sub = runs.join("nested");
    let _ = std::fs::create_dir_all(&sub);
    std::fs::write(sub.join("z"), b"z").unwrap();
    clear_runs_dir(&runs);
    assert!(runs.join("SEAL").is_file());
    assert!(!runs.join("1.run").exists());
    assert!(!runs.join("junk.tmp").exists());
    assert!(!sub.exists());

    let (rd, io) = runs_dir_io(&ctrl);
    assert_eq!(rd, runs);
    assert_eq!(on_disk_run_count(&rd, &io), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn leftover_count_does_not_gc_uncataloged_run() {
    let dir = rbitcoin_store::testutil::TempDir::labeled("runcount").unwrap();
    let ctrl = RunControl::open(&dir, "sh.runs");
    let runs = &ctrl.runs_dir;
    std::fs::write(runs.join("000001.run"), b"leftover").unwrap();
    let (rd, io) = runs_dir_io(&ctrl);
    assert_eq!(on_disk_run_count(&rd, &io), 1);
    assert!(
        runs.join("000001.run").exists(),
        "leftover count must not GC"
    );
}
