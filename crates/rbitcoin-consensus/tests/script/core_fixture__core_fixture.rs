//! Stage Bitcoin Core `src/test/data` JSON from the v31.1 submodule.
//!
//! In-tree copies of Core `src/test/data/*.json` are not kept. Each corpus
//! run hard-links (or copies) the pin into `$CARGO_TARGET_DIR/core-data/`.
//! If the submodule is missing (typical CI checkout), this runs
//! `scripts/core-functional/init-submodule.sh`.
use bitcoin::hashes::Hash;
use bitcoin::Transaction;
use rbitcoin_consensus::block::ScriptCheckJob;
use rbitcoin_consensus::error::ConsensusError;
use rbitcoin_consensus::script::classify::is_anyone_can_spend;
use rbitcoin_consensus::script::classify::ScriptKind;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once};

const HINT: &str = "run ./scripts/core-functional/init-submodule.sh (sparse v31.1 pin)";

/// Directory that holds Core JSON corpora (`RBITCOIN_CORE_DATA` or
/// `third_party/bitcoin/src/test/data` walking up from this crate).
pub fn core_data_dir() -> PathBuf {
    if let Some(d) = try_core_data_dir() {
        return d;
    }
    ensure_submodule();
    try_core_data_dir().unwrap_or_else(|| {
        panic!(
            "missing third_party/bitcoin/src/test/data/script_tests.json \
             after init-submodule.sh; {HINT}"
        )
    })
}

fn try_core_data_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RBITCOIN_CORE_DATA") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return Some(pb);
        }
        panic!("RBITCOIN_CORE_DATA={pb:?} is not a directory ({HINT})");
    }
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur: &Path = &start;
    loop {
        let cand = cur.join("third_party/bitcoin/src/test/data");
        if cand.join("script_tests.json").is_file() {
            return Some(cand);
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

fn repo_root() -> PathBuf {
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur: &Path = &start;
    loop {
        if cur
            .join("scripts/core-functional/init-submodule.sh")
            .is_file()
        {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => panic!("cannot find scripts/core-functional/init-submodule.sh from {start:?}"),
        }
    }
}

fn ensure_submodule() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if try_core_data_dir().is_some() {
            return;
        }
        let script = repo_root().join("scripts/core-functional/init-submodule.sh");
        eprintln!(
            "core_fixture: missing submodule; running {}",
            script.display()
        );
        let status = Command::new(&script)
            .status()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", script.display()));
        if !status.success() {
            panic!("{} failed: {status}; {HINT}", script.display());
        }
    });
}

/// Hard-link or copy `name` from the submodule into `$CARGO_TARGET_DIR/core-data`.
///
/// `name` is a `.json` basename in Core `src/test/data/` (no path separators).
pub fn stage_core_json(name: &str) -> PathBuf {
    assert!(
        !name.is_empty()
            && Path::new(name).file_name() == Some(name.as_ref())
            && name.ends_with(".json"),
        "core fixture name must be a .json basename, got {name:?}"
    );
    let src = core_data_dir().join(name);
    if !src.is_file() {
        panic!("missing core fixture {src:?}; {HINT}");
    }
    let dest_dir = stage_dir();
    fs::create_dir_all(&dest_dir).unwrap_or_else(|e| panic!("mkdir {dest_dir:?}: {e}"));
    let dest = dest_dir.join(name);
    install(&src, &dest);
    dest
}

fn stage_dir() -> PathBuf {
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(td).join("core-data");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/core-data")
}

/// Replace `dest` with a hard link to `src`, or a copy if link is not allowed.
fn install(src: &Path, dest: &Path) {
    static STAGE: Mutex<()> = Mutex::new(());
    let _g = STAGE.lock().unwrap();
    if dest.is_file() {
        return;
    }
    if fs::hard_link(src, dest).is_ok() {
        return;
    }
    let tmp = dest.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::copy(src, &tmp).unwrap_or_else(|e| panic!("copy {src:?} -> {tmp:?}: {e}"));
    match fs::rename(&tmp, dest) {
        Ok(()) => {}
        Err(_) if dest.is_file() => {
            let _ = fs::remove_file(&tmp);
        }
        Err(e) => panic!("rename {tmp:?} -> {dest:?}: {e}"),
    }
}
