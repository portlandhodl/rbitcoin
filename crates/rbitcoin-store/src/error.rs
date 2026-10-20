use rbitcoin_primitives::PackError;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum StoreError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    BadMagic,
    BadSchema(u16),
    BadKind {
        expected: u16,
        got: u16,
    },
    NotFound,
    InvalidFk,
    NotDirectory(PathBuf),
    Corrupt(&'static str),
    /// Soft capacity (io_uring SQ in-flight cap) — not data corruption.
    BudgetFull(&'static str),
    /// Cooperative abort (SIGINT / IBD stop) — not data corruption.
    Cancelled(&'static str),
    /// io_uring cannot be opened in this process — not on-disk corruption.
    Unavailable,
    /// Operator layout / open-option error (not on-disk corruption).
    Layout(String),
    /// Published chain prefix moved (reorg) during a confirmed-tx read. Retry.
    Stale(&'static str),
    /// Request refused (DoS cap) — not on-disk corruption.
    Rejected(&'static str),
    /// Unpaged scripthash join over `--max-sh-creates` (DoS cap) — not on-disk corruption.
    ShCreatesCap {
        creates: u32,
        cap: u32,
    },
    /// Class A seqsigwit dropped at/below the prune watermark. Not corruption.
    Pruned {
        height: u32,
    },
}

impl StoreError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        StoreError::Io {
            path: path.into(),
            source,
        }
    }

    /// `io` error carried by [`StoreError::Io`], or `other` for a non-IO variant.
    pub fn into_io(self) -> io::Error {
        match self {
            StoreError::Io { source, .. } => source,
            other => io::Error::other(other.to_string()),
        }
    }

    /// Ring in-flight cap — not on-disk corruption. Do not WARN as `corrupt record`.
    pub fn is_io_backpressure(&self) -> bool {
        matches!(self, StoreError::BudgetFull(m) if m.contains("SQ"))
    }

    /// Completion-session failure (not on-disk corruption, not SQ backpressure).
    pub fn is_uring_session_fault(&self) -> bool {
        match self {
            StoreError::Corrupt(m) => Self::is_uring_session_fault_msg(m),
            _ => false,
        }
    }

    pub fn is_uring_session_fault_msg(m: &str) -> bool {
        let s = m.to_ascii_lowercase();
        s.contains("io_uring") && !s.contains("linux-only") && !s.contains("unavailable")
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io { path, source } => {
                write!(f, "io error at {}: {source}", path.display())
            }
            StoreError::BadMagic => f.write_str("invalid store magic"),
            StoreError::BadSchema(v) => write!(f, "unsupported schema version {v}"),
            StoreError::BadKind { expected, got } => {
                write!(f, "unexpected table kind (expected {expected}, got {got})")
            }
            StoreError::NotFound => f.write_str("record not found"),
            StoreError::InvalidFk => f.write_str("invalid foreign key"),
            StoreError::NotDirectory(p) => {
                write!(f, "store path is not a directory: {}", p.display())
            }
            StoreError::Corrupt(m) => write!(f, "corrupt record: {m}"),
            StoreError::BudgetFull(m) => write!(f, "budget full: {m}"),
            StoreError::Cancelled(m) => write!(f, "cancelled: {m}"),
            StoreError::Unavailable => f.write_str("io_uring unavailable"),
            StoreError::Layout(m) => write!(f, "{m}"),
            StoreError::Stale(m) => write!(f, "{m}"),
            StoreError::Rejected(m) => f.write_str(m),
            StoreError::ShCreatesCap { creates, cap } => write!(
                f,
                "scripthash join exceeds --max-sh-creates ({creates} creates, cap {cap})"
            ),
            StoreError::Pruned { height } => write!(f, "pruned data at height {height}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<PackError> for StoreError {
    fn from(e: PackError) -> Self {
        StoreError::Corrupt(e.0)
    }
}
