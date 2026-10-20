use bitcoin::hashes::Hash;
use bitcoin::Block;
use rbitcoin_consensus::block::{block_has_witness_from_pres, ScriptCheckJob, ValidationContext};
use rbitcoin_consensus::header::pow_hash_meets_target;
use rbitcoin_consensus::{ChainParams, ConsensusError, Milestone};
use rbitcoin_primitives::Height;
use rbitcoin_query::block_pipeline::{assemble_block_prevouts, structural_validate_spends};
use rbitcoin_query::confirm_run::bq_resolve::{
    confirm_bq_resolve_wave_capped, take_wave_items_for_load, BqResolveWave,
    BQ_RESOLVE_WAVE_MAX_BLOCKS, BQ_RESOLVE_WAVE_MAX_INPUTS,
};
use rbitcoin_query::confirm_run::lookup::{
    confirm_wire_load_from_plan, confirm_wire_lookup_stamp, parent_missing_from_chain,
    ParentPinStamp, PlanStampOutcome, WireBlockIn,
};
use rbitcoin_query::confirm_run::phases::{assemble_run, Assembled};
use rbitcoin_query::confirm_run::pin::{collect_spend_abs_after_fill, pin_for_wire_batch};
use rbitcoin_query::confirm_run::scripts::{confirm_scripts_phase, drive_script_waves_with};
use rbitcoin_query::confirm_run::write::finish_post_commit;
use rbitcoin_query::confirm_run::write::{
    confirm_write_phase, confirm_write_phase_or_return, finish_post_commit_hashes,
    replay_spend_annotations,
};
use rbitcoin_query::header_read::validate_header_hashed;
use rbitcoin_query::ConfirmError;
use rbitcoin_query::{FkMap, Query, U32Map, U64Map};
use rbitcoin_store::{StoreError, WriteIoBackend};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
mod bq_resolve;
mod index;
mod lookup;
mod phases;
mod pin;
mod spend_durable_tests;
mod write_idempotent_tests;
