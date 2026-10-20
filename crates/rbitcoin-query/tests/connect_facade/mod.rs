use bitcoin::hashes::Hash;
use bitcoin::Block;
use rbitcoin_consensus::block::{validate_block_structure_hashed, ValidationContext};
use rbitcoin_consensus::unpark_script_publisher;
use rbitcoin_consensus::{ChainParams, Milestone};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::confirm_run;
use rbitcoin_query::confirm_run::{
    confirm_bq_resolve_wave_capped, confirm_scripts_phase, confirm_wire_load_from_plan,
    confirm_wire_load_phase, confirm_wire_load_phase_pipelined, confirm_wire_lookup_stamp,
    confirm_wire_run, confirm_wire_run_preverified, confirm_write_phase,
    confirm_write_phase_or_return, drive_script_waves_with, finish_post_commit_hashes,
    parent_missing_from_chain, replay_spend_annotations, take_wave_items_for_load, BqResolveWave,
    ConfirmLoadOutcome, ConfirmScriptOutcome, LoadedBatch, PlanStampOutcome, ScriptOkBatch,
    ScriptPreverified, WireBlockIn, WireLoadPipeline, BQ_RESOLVE_WAVE_MAX_BLOCKS,
    BQ_RESOLVE_WAVE_MAX_INPUTS,
};
use rbitcoin_query::convert::{block_to_apply, block_to_apply_with_txids_prev, header_to_record};
use rbitcoin_query::ConfirmError;
use rbitcoin_query::{Query, TxApply, TxPrecompute, WirePlanNeed};
use rbitcoin_store::HeaderRecord;
use std::sync::Arc;
mod coverage_tests;
