//! Confirm pipeline error. Rule rejects stay [`ConsensusError`]. Disk, io_uring,
//! and cancellation stay [`StoreError`] and must not be cached as a failed block.

use rbitcoin_consensus::ConsensusError;
use rbitcoin_store::StoreError;
use std::fmt;

#[derive(Debug)]
pub enum ConfirmError {
    Rules(ConsensusError),
    Store(StoreError),
}

impl fmt::Display for ConfirmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfirmError::Rules(e) => write!(f, "{e}"),
            ConfirmError::Store(e) => write!(f, "store: {e}"),
        }
    }
}

impl std::error::Error for ConfirmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfirmError::Rules(e) => Some(e),
            ConfirmError::Store(e) => Some(e),
        }
    }
}

impl From<ConsensusError> for ConfirmError {
    fn from(e: ConsensusError) -> Self {
        ConfirmError::Rules(e)
    }
}

impl From<StoreError> for ConfirmError {
    fn from(e: StoreError) -> Self {
        // A missing parent on an untrusted block is not store corruption.
        // Cancellation stays a store result. Callers must not cache it.
        match e {
            StoreError::Corrupt(m) if m.contains("parent create_fk unresolved") => {
                ConfirmError::from(rbitcoin_consensus::block::unresolved_parent_create())
            }
            other => ConfirmError::Store(other),
        }
    }
}

impl ConfirmError {
    pub fn is_uring_session_fault(&self) -> bool {
        matches!(self, ConfirmError::Store(se) if se.is_uring_session_fault())
    }

    pub fn rules(&self) -> Option<&ConsensusError> {
        match self {
            ConfirmError::Rules(e) => Some(e),
            ConfirmError::Store(_) => None,
        }
    }

    /// Cooperative abort. Not a verdict on the block.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            ConfirmError::Store(StoreError::Cancelled(_))
                | ConfirmError::Rules(ConsensusError::Cancelled)
        )
    }

    /// Script-pool jobs only return [`ConsensusError`]. A store fault becomes
    /// [`ConsensusError::Internal`] so the pool does not grow a second taxonomy.
    /// io_uring text stays recognizable to [`StoreError::is_uring_session_fault_msg`].
    pub fn into_pool(self) -> ConsensusError {
        match self {
            ConfirmError::Rules(e) => e,
            ConfirmError::Store(StoreError::Cancelled(_)) => ConsensusError::Cancelled,
            ConfirmError::Store(StoreError::Corrupt(m))
                if StoreError::is_uring_session_fault_msg(m) =>
            {
                ConsensusError::Internal("io_uring submit failed")
            }
            ConfirmError::Store(StoreError::Corrupt(m)) => ConsensusError::Internal(m),
            ConfirmError::Store(_) => ConsensusError::Internal("index assemble store fault"),
        }
    }
}
