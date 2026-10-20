//! Pin the crate-root names other crates actually import (X-04 keep list).
//! This test only asserts those names still resolve. Dropped names
//! (`AddressHead`, `NetConfig`, `P2PHandle`, `LiveShardTable`, …) are
//! denied by `lint/ast-grep/rules/crate-root-dropped-pub.yml`.
use rbitcoin_primitives::{
    merkle_branch, merkle_root_from_branch, merkle_root_from_txids, merkle_root_mutated,
};
use rbitcoin_store::address_head::{is_probe_exhausted_error, is_store_corrupt_display};
use rbitcoin_store::block_filter::{
    BlockFilterRecord, BlockFilterSlot, BlockFilterTable, StoredBlockFilter,
};
use rbitcoin_store::block_queue::{BlockQueue, QueuedBlockMeta, TakenRaw};
use rbitcoin_store::block_wire::block_wire_input_count;
use rbitcoin_store::chain::HeaderTxsTable;
use rbitcoin_store::compact::output_flags;
use rbitcoin_store::create_loc::CreateLocPair;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::file::ensure_nofile_budget;
use rbitcoin_store::hashhead::{HeadOpenOpts, HeadScale};
use rbitcoin_store::head_resolve_stats::{leftover_probe_diag_ready, leftover_probe_diag_recorded};
use rbitcoin_store::header_table::{block_header_hash, HeaderRecord};
use rbitcoin_store::height_fence::HeightFence;
use rbitcoin_store::idx_body_pipeline::run_idx_body_pipeline;
use rbitcoin_store::idx_body_pipeline::{BodyMode as IdxBodyMode, IdxBodyJob, SpendSlotRun};
use rbitcoin_store::index_build_uring::{read_index_window, IndexBlock, IndexHeight, IndexWindow};
use rbitcoin_store::input::InputEdge;
use rbitcoin_store::int_map::{FkMap, FkSet, U32Map, U64IdentityHasher, U64Map, U64Set};
use rbitcoin_store::integrity::VERIFY_TIP_BLOCKS;
use rbitcoin_store::io_backend::{ReadIoBackend, WriteIoBackend};
use rbitcoin_store::point_table::PointRecord;
use rbitcoin_store::scripthash::{
    script_hash, sh_heads_insert_capped, ColdProgress, ScriptHashRecord, INCLUDE_HWM_NAME,
    SH_HEADS_CAP,
};
use rbitcoin_store::scripthash_layout::ShHeadValue;
use rbitcoin_store::scripthash_materialize::{
    clear_unsorted_shard_dir, collect_unsorted_shard_files, materialize_sh_unsorted_from_class_a,
    unsorted_collect_workers, unsorted_done_last_fk, unsorted_pack_workers, unsorted_shard_dir,
};
use rbitcoin_store::sorted_run::free_gib_label;
use rbitcoin_store::sorted_run::host_mem_available_bytes;
use rbitcoin_store::sp_tweaks::{SpTweaksTable, TrimLast};
use rbitcoin_store::sp_tweaks_uring::{load_tweak_wave, LoadedTweakTx};
use rbitcoin_store::spend_annotate_uring::spend_ann_backend;
use rbitcoin_store::spend_durable::{
    elapsed_after_checkpoint, spend_sync_due, SPEND_DURABLE_INTERVAL_MS, SPEND_DURABLE_NAME,
};
use rbitcoin_store::store::{keep_unspent_vout_subsequence, SpindleHold, Store, StoreLayout};
use rbitcoin_store::store_secret::StoreSecret;
use rbitcoin_store::tx_table::HeadResizeSizeSnapshot;
use rbitcoin_store::tx_table::{
    decode_packed_tx_outs_with_spender_rels, decode_packed_tx_outs_with_spender_rels_secret,
    decode_packed_tx_with_spender_rels_secret, decode_seqsigwit_secret, encode_packed_tx,
    encode_packed_tx_with_secret, encode_seqsigwit_with_secret, encode_txout_meta_and_outs,
    encode_unspent_output_into_secret, encode_wire_seqsigwit_input, seqsigwit_input_len_upper,
    spend_meta_backend, spent_abs, InputParent, InputRecord, OutputRecord, PackedCreate, TxRecord,
};
use rbitcoin_store::txstat::TxStatRow;
use rbitcoin_store::uring_session::IoCtx;
use rbitcoin_store::uring_session::{
    abort_uring_unusable, note_uring_recover, uring_recover_count, uring_slow_drain_count,
};

#[test]
fn crate_root_exports_cross_crate_names() {
    let _ = std::any::type_name::<Store>();
    let _ = std::any::type_name::<StoreError>();
    let _ = std::any::type_name::<StoreLayout>();
    let _ = std::any::type_name::<HeadScale>();
    let _ = std::any::type_name::<HeadOpenOpts>();
    let _ = std::any::type_name::<StoreSecret>();
    let _ = std::any::type_name::<BlockQueue>();
    let _ = std::any::type_name::<QueuedBlockMeta>();
    let _ = std::any::type_name::<TakenRaw>();
    let _ = std::any::type_name::<HeaderRecord>();
    let _ = std::any::type_name::<HeightFence>();
    let _ = std::any::type_name::<FkMap<()>>();
    let _ = std::any::type_name::<FkSet>();
    let _ = std::any::type_name::<U32Map<()>>();
    let _ = std::any::type_name::<U64Map<()>>();
    let _ = std::any::type_name::<U64Set>();
    let _ = std::any::type_name::<U64IdentityHasher>();
    let _ = std::any::type_name::<PointRecord>();
    let _ = std::any::type_name::<ScriptHashRecord>();
    let _ = std::any::type_name::<ShHeadValue>();
    let _ = std::any::type_name::<ColdProgress>();
    let _ = std::any::type_name::<IdxBodyMode>();
    let _ = std::any::type_name::<IdxBodyJob>();
    let _ = std::any::type_name::<InputRecord>();
    let _ = std::any::type_name::<OutputRecord>();
    let _ = std::any::type_name::<TxRecord>();
    let _ = std::any::type_name::<dyn PackedCreate>();
    let _ = std::any::type_name::<SpTweaksTable>();
    let _ = std::any::type_name::<ReadIoBackend>();
    let _ = std::any::type_name::<WriteIoBackend>();
    let _ = std::any::type_name::<HeadResizeSizeSnapshot>();
    let _ = INCLUDE_HWM_NAME;
    let _ = SH_HEADS_CAP;
    let _ = block_header_hash;
    let _ = block_wire_input_count;
    let _ = ensure_nofile_budget;
    let _ = leftover_probe_diag_ready;
    let _ = leftover_probe_diag_recorded;
    let _ = load_tweak_wave;
    let _ = merkle_branch;
    let _ = merkle_root_from_txids;
    let _ = merkle_root_mutated;
    let _ = script_hash;
    let _ = sh_heads_insert_capped;
    let _ = spend_ann_backend;
    let _ = spend_meta_backend;
    let _ = spent_abs;
    let _ = clear_unsorted_shard_dir;
    let _ = collect_unsorted_shard_files;
    let _ = decode_seqsigwit_secret;
    let _ = decode_packed_tx_outs_with_spender_rels;
    let _ = decode_packed_tx_outs_with_spender_rels_secret;
    let _ = decode_packed_tx_with_spender_rels_secret;
    let _ = encode_packed_tx;
    let _ = encode_packed_tx_with_secret;
    let _ = encode_txout_meta_and_outs;
    let _ = encode_unspent_output_into_secret;
    let _ = free_gib_label;
    let _ = is_probe_exhausted_error;
    let _ = is_store_corrupt_display;
    let _ = materialize_sh_unsorted_from_class_a;
    let _ = output_flags::MULTI_SPENDER;
    let _ = unsorted_collect_workers;
    let _ = unsorted_done_last_fk;
    let _ = unsorted_pack_workers;
    let _ = unsorted_shard_dir;
}
