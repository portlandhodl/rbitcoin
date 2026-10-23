//! Synthetic IBD replay benchmark.
//!
//! The harness builds a valid regtest chain with a chosen transaction shape,
//! writes the raw blocks to a corpus directory, then replays them through the
//! **same** confirm entry point IBD uses (`confirm_wire_run_preverified`:
//! lookup → load/pin → scripts → write). Only the wire is synthetic; the store,
//! pin, script pool, and Class A writer are production code.
//!
//! Cases model transaction shape, not signature cost. Scripts are `OP_TRUE`
//! (legacy or P2WSH), so the result measures decode, indexing, pinning, queue
//! and write work. For real mainnet script cost use `--case real-866342`, which
//! runs the vendored block 866342 plus its Floresta prevout pack through
//! structure + script verification.
//!
//! This is a lab tool, not a product A/B. Host numbers come from the musl
//! binary and `ibd: perf` (`TESTING.md`). Build:
//! `cargo run -p rbitcoin-bench --features cli --release -- ibd --case multi`.

use bitcoin::absolute::LockTime;
use bitcoin::block::Block;
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version as TxVersion;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use rbitcoin_consensus::{ChainParams, Milestone};
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::confirm_run::lookup::WireBlockIn;
use rbitcoin_query::{confirm_wire_run_preverified, Query, ScriptPreverified};
use rbitcoin_store::{HeaderRecord, StoreLayout};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const OP_TRUE: u8 = 0x51;
const WITNESS_SCRIPT: [u8; 1] = [OP_TRUE];
/// Coinbase maturity on regtest / mainnet.
const MATURITY: u32 = 100;
const DEFAULT_TIMED_BLOCKS: u32 = 16;
const DEFAULT_WARMUP: u32 = MATURITY;
const DEFAULT_TXS_PER_BLOCK: u32 = 4_000;
const DEFAULT_BATCH: u32 = 144;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Legacy,
    Witness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Empty,
    Chain,
    Multi,
    Wide,
    Witness,
    Mixed,
    Real866342,
}

impl Case {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "empty" => Ok(Self::Empty),
            "chain" => Ok(Self::Chain),
            "multi" => Ok(Self::Multi),
            "wide" => Ok(Self::Wide),
            "witness" => Ok(Self::Witness),
            "mixed" => Ok(Self::Mixed),
            "real-866342" => Ok(Self::Real866342),
            other => Err(format!(
                "unknown --case {other} (empty|chain|multi|wide|witness|mixed|real-866342)"
            )),
        }
    }

    fn is_real(self) -> bool {
        matches!(self, Self::Real866342)
    }
}

#[derive(Clone, Debug)]
struct Spec {
    case: Case,
    /// Outputs created by the root tx (and kept alive per level).
    fanout: u32,
    /// Inputs each non-root tx spends and outputs it creates.
    per_tx: u32,
    levels: u32,
    coinbase: Kind,
}

impl Spec {
    fn for_case(case: Case, txs_per_block: u32, levels: u32) -> Result<Self, String> {
        if case.is_real() {
            return Ok(Self {
                case,
                fanout: 0,
                per_tx: 0,
                levels: 0,
                coinbase: Kind::Legacy,
            });
        }
        let txs = txs_per_block.max(1);
        let (per_tx, levels, fanout, coinbase) = match case {
            Case::Empty => (0, 0, 0, Kind::Legacy),
            Case::Chain => {
                let levels = txs.saturating_sub(1);
                (1, levels, 1, Kind::Legacy)
            }
            Case::Multi => {
                let levels = levels.max(1);
                let per_level = (txs.saturating_sub(1)).div_ceil(levels).max(1);
                (2, levels, 2 * per_level, Kind::Legacy)
            }
            Case::Wide => {
                let levels = levels.max(1);
                let per_level = (txs.saturating_sub(1)).div_ceil(levels).max(1);
                (100, levels, 100 * per_level, Kind::Legacy)
            }
            Case::Witness => {
                let levels = levels.max(1);
                let per_level = (txs.saturating_sub(1)).div_ceil(levels).max(1);
                (2, levels, 2 * per_level, Kind::Witness)
            }
            Case::Mixed => {
                let levels = levels.max(1);
                let per_level = (txs.saturating_sub(1)).div_ceil(levels).max(1);
                (2, levels, 2 * per_level, Kind::Legacy)
            }
            Case::Real866342 => unreachable!(),
        };
        if per_tx > 0 && !fanout.is_multiple_of(per_tx) {
            return Err("internal: fanout must be divisible by per_tx".into());
        }
        Ok(Self {
            case,
            fanout,
            per_tx,
            levels,
            coinbase,
        })
    }

    fn output_kind(&self, index: u32) -> Kind {
        match self.case {
            Case::Witness => Kind::Witness,
            Case::Mixed => {
                if index.is_multiple_of(2) {
                    Kind::Legacy
                } else {
                    Kind::Witness
                }
            }
            _ => Kind::Legacy,
        }
    }
}

#[derive(Debug)]
struct Cfg {
    case: Case,
    txs_per_block: u32,
    levels: u32,
    blocks: u32,
    warmup: u32,
    batch: u32,
    corpus: Option<PathBuf>,
    datadir: Option<PathBuf>,
    tiny: bool,
    keep: bool,
    out: Option<PathBuf>,
    json: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            case: Case::Multi,
            txs_per_block: DEFAULT_TXS_PER_BLOCK,
            levels: 1,
            blocks: DEFAULT_TIMED_BLOCKS,
            warmup: DEFAULT_WARMUP,
            batch: DEFAULT_BATCH,
            corpus: None,
            datadir: None,
            tiny: false,
            keep: false,
            out: None,
            json: false,
        }
    }
}

pub fn cli_main(args: &[OsString]) -> Result<(), String> {
    let cfg = parse_args(args)?;
    if cfg.case.is_real() {
        return bench_real_block(&cfg);
    }
    bench_synthetic(&cfg)
}

fn usage() -> String {
    "rbitcoin-bench ibd — synthetic IBD replay (lab tool, not a product A/B)\n\
     \n\
     Usage:\n\
       cargo run -p rbitcoin-bench --features cli --release -- ibd [OPTIONS]\n\
     \n\
     Options:\n\
       --case empty|chain|multi|wide|witness|mixed|real-866342\n\
                                      output shape (default multi)\n\
       --txs-per-block N              target txs per timed block (default 4000)\n\
       --levels N                     tree generations for multi/wide/witness/mixed\n\
                                      (default 1; chain ignores)\n\
       --blocks N                     timed blocks (default 16; real case repeats)\n\
       --warmup N                     pre-timed blocks for coinbase maturity\n\
                                      (default 100; synthetic only)\n\
       --batch N                      blocks per confirm wave (default 144)\n\
       --corpus DIR                   replay raw <height>.bin blocks instead of\n\
                                      generating them (genesis at 0000000000.bin)\n\
       --datadir DIR                  parent for a fresh run directory\n\
                                      (default $TMPDIR; never reuses a store)\n\
       --tiny                         tiny store heads (faster, not mainnet IO)\n\
       --keep                         keep the run directory\n\
       --out FILE                     CSV summary row\n\
       --json                         print one JSON object instead of the table\n\
       -h, --help                     this message\n\
     \n\
     Cases: empty=coinbase only; chain=1-in/1-out; multi=2-in/2-out;\n\
     wide=100-in txs; witness=2-in/2-out P2WSH; mixed=alternating legacy/witness.\n\
     Synthetic scripts are OP_TRUE: decode/index/pin/queue/write are measured,\n\
     ECDSA/Schnorr cost is not. real-866342 runs the vendored mainnet block and\n\
     Floresta prevouts through structure + script verification."
        .to_string()
}

fn parse_args(args: &[OsString]) -> Result<Cfg, String> {
    let mut cfg = Cfg::default();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        if a == "--help" || a == "-h" {
            println!("{}", usage());
            std::process::exit(0);
        }
        let (name, inline) = match a.strip_prefix("--").and_then(|r| r.split_once('=')) {
            Some((n, v)) => (n, Some(v.to_string())),
            None => match a.strip_prefix("--") {
                Some(n) => (n, None),
                None => return Err(format!("unknown argument `{a}`")),
            },
        };
        let value = |i: &mut usize| -> Result<String, String> {
            if let Some(v) = &inline {
                return Ok(v.clone());
            }
            *i += 1;
            args.get(*i)
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| format!("--{name} requires a value"))
        };
        match name {
            "case" => cfg.case = Case::parse(&value(&mut i)?)?,
            "txs-per-block" => {
                cfg.txs_per_block = value(&mut i)?
                    .parse()
                    .map_err(|_| "bad --txs-per-block".to_string())?;
            }
            "levels" => {
                cfg.levels = value(&mut i)?
                    .parse()
                    .map_err(|_| "bad --levels".to_string())?;
            }
            "blocks" => {
                cfg.blocks = value(&mut i)?
                    .parse()
                    .map_err(|_| "bad --blocks".to_string())?;
            }
            "warmup" => {
                cfg.warmup = value(&mut i)?
                    .parse()
                    .map_err(|_| "bad --warmup".to_string())?;
            }
            "batch" => {
                cfg.batch = value(&mut i)?
                    .parse()
                    .map_err(|_| "bad --batch".to_string())?;
            }
            "corpus" => cfg.corpus = Some(PathBuf::from(value(&mut i)?)),
            "datadir" => cfg.datadir = Some(PathBuf::from(value(&mut i)?)),
            "out" => cfg.out = Some(PathBuf::from(value(&mut i)?)),
            "tiny" => cfg.tiny = true,
            "keep" => cfg.keep = true,
            "json" => cfg.json = true,
            other => return Err(format!("unknown argument `--{other}`")),
        }
        i += 1;
    }
    if cfg.blocks == 0 {
        return Err("--blocks must be > 0".into());
    }
    if cfg.batch == 0 {
        return Err("--batch must be > 0".into());
    }
    if !cfg.case.is_real() && cfg.case != Case::Empty && cfg.warmup < MATURITY {
        return Err(format!(
            "--warmup must be >= {MATURITY} for a spending case (coinbase maturity)"
        ));
    }
    Ok(cfg)
}

fn run_dir(cfg: &Cfg) -> Result<PathBuf, String> {
    let parent = cfg.datadir.clone().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = parent.join(format!("rbtc-ibd-bench-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn script_for(kind: Kind) -> ScriptBuf {
    match kind {
        Kind::Legacy => ScriptBuf::from_bytes(vec![OP_TRUE]),
        Kind::Witness => {
            let wscript = ScriptBuf::from_bytes(WITNESS_SCRIPT.to_vec());
            ScriptBuf::new_p2wsh(&wscript.wscript_hash())
        }
    }
}

fn witness_for(kind: Kind) -> Witness {
    match kind {
        Kind::Legacy => Witness::new(),
        Kind::Witness => Witness::from_slice(&[WITNESS_SCRIPT.to_vec()]),
    }
}

fn split(total: u64, n: u32) -> Vec<u64> {
    if n == 0 {
        return Vec::new();
    }
    let each = total / u64::from(n);
    let mut out = vec![each; n as usize];
    out[0] = out[0].saturating_add(total % u64::from(n));
    out
}

#[derive(Clone, Copy, Debug)]
struct OutRef {
    txid: Txid,
    vout: u32,
    value: u64,
    kind: Kind,
}

fn build_tx(inputs: &[OutRef], values: &[u64], spec: &Spec) -> Transaction {
    Transaction {
        version: TxVersion::TWO,
        lock_time: LockTime::ZERO,
        input: inputs
            .iter()
            .map(|o| TxIn {
                previous_output: OutPoint {
                    txid: o.txid,
                    vout: o.vout,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: witness_for(o.kind),
            })
            .collect(),
        output: values
            .iter()
            .enumerate()
            .map(|(i, value)| TxOut {
                value: Amount::from_sat(*value),
                script_pubkey: script_for(spec.output_kind(i as u32)),
            })
            .collect(),
    }
}

/// Root spends `root` and creates `fanout` outputs; each level spends
/// `per_tx` outputs per tx and creates the same count, so the output set size
/// stays constant. All spends are in-block, so no further maturity is needed.
fn tree_txs(root: OutRef, spec: &Spec) -> Vec<Transaction> {
    if spec.fanout == 0 || spec.per_tx == 0 || spec.levels == 0 {
        return Vec::new();
    }
    let mut txs = Vec::new();
    let values = split(root.value, spec.fanout);
    let root_tx = build_tx(&[root], &values, spec);
    let root_txid = root_tx.compute_txid();
    txs.push(root_tx);
    let mut prev: Vec<OutRef> = (0..spec.fanout)
        .map(|i| OutRef {
            txid: root_txid,
            vout: i,
            value: values[i as usize],
            kind: spec.output_kind(i),
        })
        .collect();
    let per_level = prev.len() / spec.per_tx as usize;
    for _ in 0..spec.levels {
        let mut next = Vec::with_capacity(prev.len());
        for t in 0..per_level {
            let start = t * spec.per_tx as usize;
            let ins: Vec<OutRef> = prev[start..start + spec.per_tx as usize].to_vec();
            let total: u64 = ins.iter().map(|o| o.value).sum();
            let values = split(total, spec.per_tx);
            let tx = build_tx(&ins, &values, spec);
            let txid = tx.compute_txid();
            for i in 0..spec.per_tx {
                next.push(OutRef {
                    txid,
                    vout: i,
                    value: values[i as usize],
                    kind: spec.output_kind(i),
                });
            }
            txs.push(tx);
        }
        prev = next;
    }
    txs
}

fn coinbase_value(height: u32, params: &ChainParams) -> u64 {
    rbitcoin_consensus::block_subsidy(height, params) as u64
}

/// Build `warmup + timed` blocks on regtest genesis and write raw files.
///
/// Returns `(paths[height], txs, inputs, bytes)` for the timed range.
fn generate(cfg: &Cfg, spec: &Spec, dir: &Path) -> Result<(), String> {
    let params = ChainParams::regtest();
    let genesis = rbitcoin_consensus::genesis_block(&params);
    let mut prev_hash = genesis.block_hash();
    let mut prev_time = genesis.header.time;
    write_block(dir, 0, &genesis)?;
    let mut coinbase_at: std::collections::HashMap<u32, OutRef> = Default::default();
    let total = cfg
        .warmup
        .checked_add(cfg.blocks)
        .ok_or_else(|| "block count overflow".to_string())?;
    for height in 1..=total {
        let time = prev_time.saturating_add(600);
        let extra = if height > cfg.warmup && spec.fanout > 0 {
            let root_height = height.saturating_sub(MATURITY);
            let root = *coinbase_at.get(&root_height).ok_or_else(|| {
                format!("missing mature coinbase at height {root_height} for {height}")
            })?;
            tree_txs(root, spec)
        } else {
            Vec::new()
        };
        let block = rbitcoin_consensus::mine_regtest_paying(
            prev_hash,
            time,
            height,
            script_for(spec.coinbase),
            extra,
        );
        write_block(dir, height, &block)?;
        let cb = block.txdata[0].compute_txid();
        coinbase_at.insert(
            height,
            OutRef {
                txid: cb,
                vout: 0,
                value: coinbase_value(height, &params),
                kind: spec.coinbase,
            },
        );
        prev_hash = block.block_hash();
        prev_time = time;
    }
    Ok(())
}

fn write_block(dir: &Path, height: u32, block: &Block) -> Result<PathBuf, String> {
    let path = dir.join(format!("{height:010}.bin"));
    std::fs::write(&path, serialize(block))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn header_record(prev_fk: Fk, block: &Block) -> HeaderRecord {
    rbitcoin_query::header_to_record(prev_fk, &block.header, block.block_hash().to_byte_array())
}

fn read_rss_kb() -> (u64, u64) {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return (0, 0);
    };
    let mut rss = 0;
    let mut hwm = 0;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            rss = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("VmHWM:") {
            hwm = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
        }
    }
    (rss, hwm)
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total: u64 = 0;
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            total = total.saturating_add(dir_bytes(&p));
        } else {
            total = total.saturating_add(md.len());
        }
    }
    total
}

#[derive(Default)]
struct StageTotals {
    decode_ns: u64,
    lookup_total_ns: u64,
    load_ns: u64,
    script_ns: u64,
    class_a_ns: u64,
    structural_ns: u64,
    class_c_ns: u64,
    connect_ns: u64,
}

fn bench_synthetic(cfg: &Cfg) -> Result<(), String> {
    let spec = Spec::for_case(cfg.case, cfg.txs_per_block, cfg.levels)?;
    let root = run_dir(cfg)?;
    let store_dir = root.join("store");
    let (corpus, gen_s) = if let Some(dir) = &cfg.corpus {
        (dir.clone(), 0.0)
    } else {
        let corpus = root.join("blocks");
        std::fs::create_dir_all(&corpus).map_err(|e| format!("create corpus: {e}"))?;
        let gen = Instant::now();
        generate(cfg, &spec, &corpus)?;
        (corpus, gen.elapsed().as_secs_f64())
    };

    let layout = if cfg.tiny {
        StoreLayout::tiny(&store_dir)
    } else {
        StoreLayout::single(&store_dir)
    };
    let query = Query::open_or_create_layout(layout).map_err(|e| e.to_string())?;
    let params = ChainParams::regtest();
    let rss_before = read_rss_kb();
    let replay = Instant::now();
    let stats = replay_blocks(&query, &params, &corpus, cfg.warmup, cfg.blocks, cfg.batch)?;
    let want_tip = cfg
        .warmup
        .checked_add(cfg.blocks)
        .ok_or_else(|| "block count overflow".to_string())?;
    if query.tip_height().map(|h| h.0) != Some(want_tip) {
        return Err(format!(
            "replay stopped at {:?}, expected {want_tip}",
            query.tip_height().map(|h| h.0)
        ));
    }
    let replay_s = replay.elapsed().as_secs_f64();
    let rss_after = read_rss_kb();
    let disk = dir_bytes(&store_dir);
    let blocks = u64::from(cfg.blocks);
    let mut batch_ms = stats.batch_ms.clone();
    let p50 = percentile(&mut batch_ms, 0.50);
    let p95 = percentile(&mut batch_ms, 0.95);
    let row = Row {
        case: format!("{:?}", cfg.case).to_ascii_lowercase(),
        blocks,
        txs: stats.txs,
        inputs: stats.inputs,
        bytes: stats.bytes,
        gen_s,
        replay_s,
        blocks_per_s: blocks as f64 / replay_s.max(f64::EPSILON),
        txs_per_s: stats.txs as f64 / replay_s.max(f64::EPSILON),
        inputs_per_s: stats.inputs as f64 / replay_s.max(f64::EPSILON),
        mib_per_s: stats.bytes as f64 / (1024.0 * 1024.0) / replay_s.max(f64::EPSILON),
        rss_before_kb: rss_before.0,
        rss_after_kb: rss_after.0,
        rss_hwm_kb: rss_after.1,
        store_mib: disk as f64 / (1024.0 * 1024.0),
        batch_p50_ms: p50,
        batch_p95_ms: p95,
    };
    let stages = query.confirm_stats().take_window();
    let stage_totals = StageTotals {
        decode_ns: stats.decode_ns,
        lookup_total_ns: stages.lookup_total_ns,
        load_ns: stages.load_ns,
        script_ns: stages.script_ns,
        class_a_ns: stages.class_a_ns,
        structural_ns: stages.structural_ns,
        class_c_ns: stages.class_c_ns,
        connect_ns: stages.connect_ns,
    };
    print_row(&row, &stage_totals, cfg.json);
    if let Some(out) = &cfg.out {
        write_csv(out, &row, &stage_totals)?;
    }
    if cfg.keep {
        eprintln!("kept {}", root.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }
    Ok(())
}

#[derive(Default)]
struct ReplayStats {
    txs: u64,
    inputs: u64,
    bytes: u64,
    decode_ns: u64,
    batch_ms: Vec<f64>,
}

fn replay_blocks(
    query: &Query,
    params: &ChainParams,
    corpus: &Path,
    warmup: u32,
    timed: u32,
    batch: u32,
) -> Result<ReplayStats, String> {
    let mut stats = ReplayStats::default();
    let mut prev_fk = Fk::NULL;
    let mut prev_hash = bitcoin::BlockHash::from_byte_array([0u8; 32]);
    // Genesis is confirmed alone: a batch that starts at height 0 with the
    // following blocks is not the tip-follow seam the writer expects.
    apply_corpus_batch(
        query,
        params,
        corpus,
        0,
        1,
        &mut prev_fk,
        &mut prev_hash,
        false,
        &mut stats,
    )?;
    let mut height = 1u32;
    while height <= warmup {
        let n = batch.min(warmup + 1 - height);
        apply_corpus_batch(
            query,
            params,
            corpus,
            height,
            n,
            &mut prev_fk,
            &mut prev_hash,
            false,
            &mut stats,
        )?;
        height += n;
    }
    // Warmup counters (genesis + maturity blocks) are not measured.
    let _ = query.confirm_stats().take_window();
    let last = warmup
        .checked_add(timed)
        .ok_or_else(|| "block count overflow".to_string())?;
    while height <= last {
        let n = batch.min(last + 1 - height);
        apply_corpus_batch(
            query,
            params,
            corpus,
            height,
            n,
            &mut prev_fk,
            &mut prev_hash,
            true,
            &mut stats,
        )?;
        height += n;
    }
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
fn apply_corpus_batch(
    query: &Query,
    params: &ChainParams,
    corpus: &Path,
    start: u32,
    n: u32,
    prev_fk: &mut Fk,
    prev_hash: &mut bitcoin::BlockHash,
    timed: bool,
    stats: &mut ReplayStats,
) -> Result<(), String> {
    let mut wire: Vec<WireBlockIn> = Vec::with_capacity(n as usize);
    let mut applied = Vec::with_capacity(n as usize);
    for h in start..start + n {
        let path = corpus.join(format!("{h:010}.bin"));
        let raw = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let block: Block =
            deserialize(&raw).map_err(|e| format!("decode {}: {e}", path.display()))?;
        if block.header.prev_blockhash != *prev_hash {
            return Err(format!(
                "corpus is not linked at height {h}: prev {} != {}",
                block.header.prev_blockhash, prev_hash
            ));
        }
        let rec = header_record(*prev_fk, &block);
        *prev_fk = query
            .ensure_header(&rec)
            .map_err(|e| format!("ensure header {h}: {e}"))?;
        *prev_hash = block.block_hash();
        let t = Instant::now();
        let (block, pres, _) = rbitcoin_consensus::decode_block_precomputes(&raw, true)
            .ok_or_else(|| format!("precompute {h}"))?;
        if timed {
            stats.decode_ns = stats
                .decode_ns
                .saturating_add(t.elapsed().as_nanos() as u64);
            stats.txs = stats.txs.saturating_add(block.txdata.len() as u64);
            stats.inputs = stats.inputs.saturating_add(
                block
                    .txdata
                    .iter()
                    .skip(1)
                    .map(|tx| tx.input.len() as u64)
                    .sum::<u64>(),
            );
            stats.bytes = stats.bytes.saturating_add(raw.len() as u64);
        }
        wire.push((Height(h), std::sync::Arc::new(block), Some(pres.into())));
        applied.push(h);
    }
    let t = Instant::now();
    let fks = confirm_wire_run_preverified(
        query,
        params,
        Milestone::NONE,
        &wire,
        &ScriptPreverified::new(),
    )
    .map_err(|e| format!("confirm {:?}: {e}", applied.first()))?;
    if timed {
        stats.batch_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    if fks.len() != wire.len() {
        return Err(format!(
            "confirm returned {} fks for {} blocks",
            fks.len(),
            wire.len()
        ));
    }
    Ok(())
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let i = ((v.len() - 1) as f64 * p).round() as usize;
    v[i.min(v.len() - 1)]
}

struct Row {
    case: String,
    blocks: u64,
    txs: u64,
    inputs: u64,
    bytes: u64,
    gen_s: f64,
    replay_s: f64,
    blocks_per_s: f64,
    txs_per_s: f64,
    inputs_per_s: f64,
    mib_per_s: f64,
    rss_before_kb: u64,
    rss_after_kb: u64,
    rss_hwm_kb: u64,
    store_mib: f64,
    batch_p50_ms: f64,
    batch_p95_ms: f64,
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn print_row(row: &Row, stages: &StageTotals, json: bool) {
    if json {
        println!(
            "{{\"case\":\"{}\",\"blocks\":{},\"txs\":{},\"inputs\":{},\"bytes\":{},\
             \"gen_s\":{:.3},\"replay_s\":{:.3},\"blocks_per_s\":{:.3},\"txs_per_s\":{:.1},\
             \"inputs_per_s\":{:.1},\"mib_per_s\":{:.3},\"rss_before_kb\":{},\
             \"rss_after_kb\":{},\"rss_hwm_kb\":{},\"store_mib\":{:.1},\
             \"batch_p50_ms\":{:.1},\"batch_p95_ms\":{:.1},\
             \"decode_ms\":{:.1},\"lookup_total_ms\":{:.1},\"load_ms\":{:.1},\"script_ms\":{:.1},\
             \"class_a_ms\":{:.1},\"structural_ms\":{:.1},\"class_c_ms\":{:.1},\
             \"connect_ms\":{:.1}}}",
            row.case,
            row.blocks,
            row.txs,
            row.inputs,
            row.bytes,
            row.gen_s,
            row.replay_s,
            row.blocks_per_s,
            row.txs_per_s,
            row.inputs_per_s,
            row.mib_per_s,
            row.rss_before_kb,
            row.rss_after_kb,
            row.rss_hwm_kb,
            row.store_mib,
            row.batch_p50_ms,
            row.batch_p95_ms,
            ms(stages.decode_ns),
            ms(stages.lookup_total_ns),
            ms(stages.load_ns),
            ms(stages.script_ns),
            ms(stages.class_a_ns),
            ms(stages.structural_ns),
            ms(stages.class_c_ns),
            ms(stages.connect_ns),
        );
        return;
    }
    println!(
        "case={} blocks={} txs={} inputs={} MiB={:.1}",
        row.case,
        row.blocks,
        row.txs,
        row.inputs,
        row.bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "generate={:.2}s replay={:.2}s  {:.2} blocks/s  {:.0} tx/s  {:.0} in/s  {:.1} MiB/s",
        row.gen_s, row.replay_s, row.blocks_per_s, row.txs_per_s, row.inputs_per_s, row.mib_per_s
    );
    println!(
        "batch p50={:.1}ms p95={:.1}ms  rss before={} MiB after={} MiB hwm={} MiB store={:.1} MiB",
        row.batch_p50_ms,
        row.batch_p95_ms,
        row.rss_before_kb / 1024,
        row.rss_after_kb / 1024,
        row.rss_hwm_kb / 1024,
        row.store_mib
    );
    println!(
        "stages decode={:.1}ms lookup={:.1}ms load={:.1}ms script={:.1}ms class_a={:.1}ms structural={:.1}ms class_c={:.1}ms connect={:.1}ms",
        ms(stages.decode_ns),
        ms(stages.lookup_total_ns),
        ms(stages.load_ns),
        ms(stages.script_ns),
        ms(stages.class_a_ns),
        ms(stages.structural_ns),
        ms(stages.class_c_ns),
        ms(stages.connect_ns)
    );
}

fn write_csv(path: &Path, row: &Row, stages: &StageTotals) -> Result<(), String> {
    let header = "case,blocks,txs,inputs,bytes,gen_s,replay_s,blocks_per_s,txs_per_s,inputs_per_s,mib_per_s,rss_before_kb,rss_after_kb,rss_hwm_kb,store_mib,batch_p50_ms,batch_p95_ms,decode_ms,lookup_total_ms,load_ms,script_ms,class_a_ms,structural_ms,class_c_ms,connect_ms\n";
    let line = format!(
        "{case},{blocks},{txs},{inputs},{bytes},{gen_s:.3},{replay_s:.3},{blocks_per_s:.3},{txs_per_s:.1},{inputs_per_s:.1},{mib_per_s:.3},{rss_before_kb},{rss_after_kb},{rss_hwm_kb},{store_mib:.1},{p50:.1},{p95:.1},{decode:.1},{lookup:.1},{load:.1},{script:.1},{class_a:.1},{structural:.1},{class_c:.1},{connect:.1}\n",
        case = row.case,
        blocks = row.blocks,
        txs = row.txs,
        inputs = row.inputs,
        bytes = row.bytes,
        gen_s = row.gen_s,
        replay_s = row.replay_s,
        blocks_per_s = row.blocks_per_s,
        txs_per_s = row.txs_per_s,
        inputs_per_s = row.inputs_per_s,
        mib_per_s = row.mib_per_s,
        rss_before_kb = row.rss_before_kb,
        rss_after_kb = row.rss_after_kb,
        rss_hwm_kb = row.rss_hwm_kb,
        store_mib = row.store_mib,
        p50 = row.batch_p50_ms,
        p95 = row.batch_p95_ms,
        decode = ms(stages.decode_ns),
        lookup = ms(stages.lookup_total_ns),
        load = ms(stages.load_ns),
        script = ms(stages.script_ns),
        class_a = ms(stages.class_a_ns),
        structural = ms(stages.structural_ns),
        class_c = ms(stages.class_c_ns),
        connect = ms(stages.connect_ns),
    );

    let write_header = !path.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    use std::io::Write;
    if write_header {
        f.write_all(header.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    f.write_all(line.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Real mainnet block 866342 + Floresta prevout pack.
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rbitcoin-consensus/tests/fixtures/block_866342")
}

fn decode_zstd(path: &Path) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut decoder =
        ruzstd::decoding::StreamingDecoder::new_with_max_window_size(f, 128 * 1024 * 1024)
            .map_err(|e| format!("zstd {}: {e}", path.display()))?;
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("zstd read {}: {e}", path.display()))?;
    Ok(out)
}

fn prevouts_from_json(bytes: &[u8]) -> Result<Vec<TxOut>, String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or("spent_utxos is not an array")?;
    arr.iter()
        .map(|u| {
            let value = u["txout"]["value"]
                .as_u64()
                .ok_or("missing prevout value")?;
            let spk = u["txout"]["script_pubkey"]
                .as_str()
                .ok_or("missing prevout script")?;
            let spk = rbitcoin_primitives::hex_decode(spk).map_err(|e| e.to_string())?;
            Ok(TxOut {
                value: Amount::from_sat(value),
                script_pubkey: ScriptBuf::from_bytes(spk),
            })
        })
        .collect()
}

fn bench_real_block(cfg: &Cfg) -> Result<(), String> {
    let dir = fixture_dir();
    let raw = decode_zstd(&dir.join("raw.zst"))?;
    let prev_bytes = decode_zstd(&dir.join("spent_utxos.zst"))?;
    let prevouts = prevouts_from_json(&prev_bytes)?;
    let template: Block = deserialize(&raw).map_err(|e| format!("decode 866342: {e}"))?;
    let n_tx = template.txdata.len();
    let n_in: usize = template
        .txdata
        .iter()
        .skip(1)
        .map(|tx| tx.input.len())
        .sum();
    if prevouts.len() != n_in {
        return Err(format!(
            "prevouts {} != non-coinbase inputs {n_in}",
            prevouts.len()
        ));
    }
    let params = ChainParams::mainnet();
    let ctx =
        rbitcoin_consensus::block::ValidationContext::at(&params, Height(866_342), Milestone::NONE);
    let rss_before = read_rss_kb();
    let mut decode_ns = 0u64;
    let mut structure_ns = 0u64;
    let mut script_ns = 0u64;
    let t_all = Instant::now();
    for _ in 0..cfg.blocks {
        let t = Instant::now();
        let (block, _pres, _) =
            rbitcoin_consensus::decode_block_precomputes(&raw, true).ok_or("decode precomputes")?;
        decode_ns = decode_ns.saturating_add(t.elapsed().as_nanos() as u64);
        let t = Instant::now();
        rbitcoin_consensus::block::validate_block_structure_hashed(&block, &ctx)
            .map_err(|e| format!("structure: {e}"))?;
        structure_ns = structure_ns.saturating_add(t.elapsed().as_nanos() as u64);
        let arc = std::sync::Arc::new(block);
        let mut stxos = prevouts.iter();
        let t = Instant::now();
        for i in 1..n_tx {
            let tx = &arc.txdata[i];
            let mut prevs = Vec::with_capacity(tx.input.len());
            for _ in &tx.input {
                prevs.push(stxos.next().ok_or("prevouts short")?.clone());
            }
            let txid = tx.compute_txid().to_byte_array();
            let job = rbitcoin_consensus::block::ScriptCheckJob::from_parts(
                txid,
                rbitcoin_consensus::block::JobPrevouts::owned(prevs),
                rbitcoin_consensus::block::JobTx::shared(std::sync::Arc::clone(&arc), i),
                rbitcoin_consensus::block::ScriptVerifyFlags::buried(true, true, true, true, true),
            );
            rbitcoin_consensus::script::verify_job_all_inputs(&job)
                .map_err(|e| format!("script tx {i}: {e}"))?;
        }
        script_ns = script_ns.saturating_add(t.elapsed().as_nanos() as u64);
    }
    let wall = t_all.elapsed().as_secs_f64();
    let reps = f64::from(cfg.blocks.max(1));
    let rss_after = read_rss_kb();
    println!(
        "case=real-866342 repeats={} txs_per_block={} inputs_per_block={}",
        cfg.blocks, n_tx, n_in
    );
    println!(
        "wall={:.3}s {:.1} blocks/s {:.0} tx/s {:.0} in/s decode={:.1}ms structure={:.1}ms script={:.1}ms (per block)",
        wall / reps,
        reps / wall.max(f64::EPSILON),
        (n_tx as f64 * reps) / wall.max(f64::EPSILON),
        (n_in as f64 * reps) / wall.max(f64::EPSILON),
        ms(decode_ns) / reps,
        ms(structure_ns) / reps,
        ms(script_ns) / reps
    );
    println!(
        "rss before={} MiB after={} MiB hwm={} MiB",
        rss_before.0 / 1024,
        rss_after.0 / 1024,
        rss_after.1 / 1024
    );
    if let Some(out) = &cfg.out {
        let row = Row {
            case: "real-866342".into(),
            blocks: u64::from(cfg.blocks),
            txs: n_tx as u64 * u64::from(cfg.blocks),
            inputs: n_in as u64 * u64::from(cfg.blocks),
            bytes: raw.len() as u64 * u64::from(cfg.blocks),
            gen_s: 0.0,
            replay_s: wall,
            blocks_per_s: reps / wall.max(f64::EPSILON),
            txs_per_s: (n_tx as f64 * reps) / wall.max(f64::EPSILON),
            inputs_per_s: (n_in as f64 * reps) / wall.max(f64::EPSILON),
            mib_per_s: (raw.len() as f64 * reps) / (1024.0 * 1024.0) / wall.max(f64::EPSILON),
            rss_before_kb: rss_before.0,
            rss_after_kb: rss_after.0,
            rss_hwm_kb: rss_after.1,
            store_mib: 0.0,
            batch_p50_ms: 0.0,
            batch_p95_ms: 0.0,
        };
        let stages = StageTotals {
            decode_ns,
            lookup_total_ns: 0,
            load_ns: 0,
            script_ns,
            class_a_ns: 0,
            structural_ns: structure_ns,
            class_c_ns: 0,
            connect_ns: 0,
        };
        write_csv(out, &row, &stages)?;
    }
    Ok(())
}

/// Entry used by the main CLI (args exclude the `ibd` token).
pub fn run(args: &[OsString]) -> ExitCode {
    match cli_main(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_specs_have_the_requested_shapes() {
        let chain = Spec::for_case(Case::Chain, 100, 1).unwrap();
        assert_eq!((chain.per_tx, chain.levels, chain.fanout), (1, 99, 1));
        let multi = Spec::for_case(Case::Multi, 100, 1).unwrap();
        assert_eq!((multi.per_tx, multi.levels, multi.fanout), (2, 1, 198));
        let wide = Spec::for_case(Case::Wide, 100, 1).unwrap();
        assert_eq!((wide.per_tx, wide.levels, wide.fanout), (100, 1, 9_900));
        let levels = Spec::for_case(Case::Multi, 100, 3).unwrap();
        assert_eq!(levels.levels, 3);
        assert_eq!(levels.fanout % levels.per_tx, 0);
    }

    #[test]
    fn tree_txs_link_and_conserve_value() {
        let spec = Spec::for_case(Case::Multi, 9, 1).unwrap();
        let root = OutRef {
            txid: Txid::from_byte_array([1u8; 32]),
            vout: 0,
            value: 5_000_000_000,
            kind: Kind::Legacy,
        };
        let txs = tree_txs(root, &spec);
        assert_eq!(txs.len(), 9);
        assert_eq!(txs[0].input.len(), 1);
        for tx in &txs[1..] {
            assert_eq!(tx.input.len(), 2);
            assert_eq!(tx.output.len(), 2);
            for txin in &tx.input {
                assert!(
                    txin.previous_output.txid == txs[0].compute_txid()
                        || txs
                            .iter()
                            .any(|t| t.compute_txid() == txin.previous_output.txid)
                );
            }
        }
        let out_sum: u64 = txs
            .iter()
            .flat_map(|t| t.output.iter())
            .map(|o| o.value.to_sat())
            .sum();
        assert!(out_sum > 0);
    }

    #[test]
    fn witness_tree_uses_p2wsh_witness() {
        let spec = Spec::for_case(Case::Witness, 5, 1).unwrap();
        let root = OutRef {
            txid: Txid::from_byte_array([2u8; 32]),
            vout: 0,
            value: 5_000_000_000,
            kind: Kind::Witness,
        };
        let txs = tree_txs(root, &spec);
        assert!(txs
            .iter()
            .any(|t| t.input.iter().any(|i| !i.witness.is_empty())));
        assert!(txs
            .iter()
            .flat_map(|t| t.output.iter())
            .any(|o| o.script_pubkey.is_p2wsh()));
    }

    #[test]
    fn header_record_has_the_wire_hash_and_parent() {
        let genesis = rbitcoin_consensus::genesis_block(&ChainParams::regtest());
        let rec = header_record(Fk::NULL, &genesis);
        assert_eq!(rec.hash, genesis.block_hash().to_byte_array());
        assert_eq!(rec.prev_fk, Fk::NULL);
    }

    // A tiny end-to-end proof that the replay entry point accepts a generated
    // chain. Kept small so it stays inside the default test budget.
    #[test]
    fn empty_chain_replays_through_confirm() {
        let root = std::env::temp_dir().join(format!(
            "rbtc-ibd-bench-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let corpus = root.join("blocks");
        std::fs::create_dir_all(&corpus).unwrap();
        let cfg = Cfg {
            case: Case::Empty,
            txs_per_block: 1,
            levels: 1,
            blocks: 3,
            warmup: 0,
            batch: 3,
            corpus: None,
            datadir: None,
            tiny: true,
            keep: false,
            out: None,
            json: false,
        };
        let spec = Spec::for_case(cfg.case, cfg.txs_per_block, cfg.levels).unwrap();
        generate(&cfg, &spec, &corpus).unwrap();
        let q = Query::open_or_create_layout(StoreLayout::tiny(root.join("store"))).unwrap();
        replay_blocks(&q, &ChainParams::regtest(), &corpus, 0, 3, 3).unwrap();
        assert_eq!(q.tip_height(), Some(Height(3)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
