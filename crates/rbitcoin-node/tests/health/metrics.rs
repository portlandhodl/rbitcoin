use rbitcoin_log::progress::Snapshot;
use rbitcoin_net::ChainHub;
use rbitcoin_node::health::metrics::*;
use rbitcoin_node::health::{readiness, NodeStatus, Phase};
use std::fmt::{Display, Write};
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

fn stage(stage: &'static str, done: u64) -> Snapshot {
    Snapshot {
        stage,
        done,
        total: 10,
        base: 0,
        elapsed: Duration::ZERO,
        started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
    }
}

/// Two running stages of one name (two nodes in one process) export one
/// series, the newest; each family's HELP/TYPE appears once; idle exports
/// nothing.
#[test]
fn progress_gauges_one_series_per_stage_name() {
    let mut out = Exposition::default();
    progress_gauges(&mut out, &[]);
    assert_eq!(out.0, "");

    let mut out = Exposition::default();
    progress_gauges(&mut out, &[stage("x", 1), stage("y", 5), stage("x", 2)]);
    let lines: Vec<&str> = out.0.lines().collect();
    let count = |prefix: &str| lines.iter().filter(|l| l.starts_with(prefix)).count();
    assert_eq!(count("rbitcoin_progress_done{stage=\"x\"}"), 1, "{}", out.0);
    assert!(
        lines.contains(&"rbitcoin_progress_done{stage=\"x\"} 2"),
        "{}",
        out.0
    );
    assert!(
        lines.contains(&"rbitcoin_progress_done{stage=\"y\"} 5"),
        "{}",
        out.0
    );
    assert!(lines.contains(&"rbitcoin_progress_target{stage=\"x\"} 10"));
    assert!(lines.contains(&"rbitcoin_progress_start_time_seconds{stage=\"x\"} 1000"));
    for family in [
        "rbitcoin_progress_done",
        "rbitcoin_progress_target",
        "rbitcoin_progress_start_time_seconds",
    ] {
        assert_eq!(count(&format!("# TYPE {family} gauge")), 1, "{}", out.0);
        assert_eq!(count(&format!("# HELP {family} ")), 1, "{}", out.0);
    }
}
