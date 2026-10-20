use rbitcoin_primitives::{schema_file_openable, TableKind, SCHEMA_VERSION, STORE_MAGIC};
use rbitcoin_store::error::StoreError;
use rbitcoin_store::io_handle::IoHandle;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
mod advise_tests;
