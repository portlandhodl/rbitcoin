//! Shared leftover-run dir helpers (SEAL + discard). Not a catalog spill path.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Control plane for leftover `scripthash.runs` discard / SEAL.
///
/// **`runs_io` invariant:** list + delete for this family hold `runs_io` for the
/// full critical section.
pub struct RunControl {
    pub runs_dir: PathBuf,
    /// Serializes leftover-run list / delete.
    pub runs_io: Arc<Mutex<()>>,
}

impl RunControl {
    pub fn open(store_dir: &Path, subdir: &str) -> Self {
        let runs_dir = store_dir.join(subdir);
        let _ = std::fs::create_dir_all(&runs_dir);
        Self {
            runs_dir,
            runs_io: Arc::new(Mutex::new(())),
        }
    }
}

/// On-disk leftover count under `runs_io`.
///
/// Every name except `SEAL` / `SEAL.tmp`. Does not unlink.
pub fn on_disk_run_count(runs_dir: &Path, runs_io: &Mutex<()>) -> usize {
    let _held = runs_io.lock().unwrap();
    let Ok(rd) = std::fs::read_dir(runs_dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name != "SEAL" && name != "SEAL.tmp"
        })
        .count()
}

/// Snapshot `(runs_dir, runs_io)` from a locked catalog control.
pub fn runs_dir_io(ctrl: &RunControl) -> (PathBuf, Arc<Mutex<()>>) {
    (ctrl.runs_dir.clone(), Arc::clone(&ctrl.runs_io))
}

/// Remove run/mat/merge artifacts under `runs_dir`, **preserving `SEAL`**.
pub fn clear_runs_dir(runs_dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(runs_dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name == "SEAL" || name == "SEAL.tmp" {
                continue;
            }
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}
