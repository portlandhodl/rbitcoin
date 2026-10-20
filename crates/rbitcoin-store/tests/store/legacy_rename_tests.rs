use rbitcoin_primitives::{schema_file_openable, Fk, Height, SCHEMA_VERSION, STORE_MAGIC};
use rbitcoin_store::chain::{ConfirmedTable, HeaderTxsTable, StrongTxTable};
use rbitcoin_store::error::StoreError;
use rbitcoin_store::hashhead::{HeadOpenOpts, HeadScale};
use rbitcoin_store::header_table::{HeaderRecord, HeaderTable};
use rbitcoin_store::height_fence::{HeightFence, MtpRing};
use rbitcoin_store::point_table::{self, PointRecord};
use rbitcoin_store::scripthash::ScriptHashTable;
use rbitcoin_store::spender_table::SpenderTable;
use rbitcoin_store::store::*;
use rbitcoin_store::tx_table::{InputRecord, OutputRecord, TxRecord, TxTable};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[test]
fn legacy_names_rename_and_both_exist_errors() {
    let dir = rbitcoin_store::testutil::TempDir::labeled("legacy-rename").unwrap();
    std::fs::write(dir.join("inputs.loc"), b"loc").unwrap();
    std::fs::write(dir.join("inwit.body"), b"body").unwrap();
    rename_legacy_input_files(dir.path()).unwrap();
    rename_legacy_inwit_files(dir.path()).unwrap();
    assert_eq!(std::fs::read(dir.join("input.loc")).unwrap(), b"loc");
    assert_eq!(std::fs::read(dir.join("seqsigwit.body")).unwrap(), b"body");
    assert!(!dir.join("inputs.loc").exists());
    std::fs::write(dir.join("inputs.off"), b"old").unwrap();
    std::fs::write(dir.join("input.off"), b"new").unwrap();
    let err = rename_legacy_input_files(dir.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::Layout(ref m) if m.contains("both exist")),
        "{err}"
    );
}
