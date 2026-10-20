use rbitcoin_store::head_resolve_pick::LeftoverMissOn;
use rbitcoin_store::head_resolve_stats::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Pure Sample math (no process atomics — safe under parallel cargo test).
#[test]
fn sample_age_cdf_and_compact() {
    let mut s = Sample::default();
    s.age_hit[0] = 2;
    s.age_hit[3] = 1;
    s.age_hit[AGE_CAP - 1] = 1;
    assert_eq!(s.age_hit_n(), 4);
    assert!((s.age_cdf(0) - 0.5).abs() < 1e-9);
    assert!((s.age_cdf(3) - 0.75).abs() < 1e-9);
    assert!((s.age_cdf(47) - 1.0).abs() < 1e-9);
    assert_eq!(s.age_cdf_pct(3), 75);
    assert!(s.age_hit_compact().starts_with("2:0:0:1:"));
    s.hit_rank_sum = 4;
    s.hit_rank_n = 2;
    assert!((s.hit_rank_avg() - 2.0).abs() < 1e-9);
    s.probe_ns = 10;
    s.idx_ns = 20;
    s.body_ns = 30;
    assert_eq!(s.sum_ns(), 60);
    let empty = Sample::default();
    assert_eq!(empty.age_cdf(3), 0.0);
    assert_eq!(empty.age_hit_n(), 0);
}

#[test]
fn sealed_age_for_fk_binary_search() {
    // segs: [1, 100, 200] → ages from tip: si0 age2, si1 age1, si2 age0
    let first = [1u64, 100, 200];
    assert_eq!(sealed_age_for_fk(&first, 0), None);
    assert_eq!(sealed_age_for_fk(&first, 1), Some(2));
    assert_eq!(sealed_age_for_fk(&first, 99), Some(2));
    assert_eq!(sealed_age_for_fk(&first, 100), Some(1));
    assert_eq!(sealed_age_for_fk(&first, 199), Some(1));
    assert_eq!(sealed_age_for_fk(&first, 200), Some(0));
    assert_eq!(sealed_age_for_fk(&first, 999_999), Some(0));
    assert_eq!(sealed_age_for_fk(&[], 1), None);
    assert_eq!(sealed_age_from_index(5, 6), 0);
    assert_eq!(sealed_age_from_index(0, 6), 5);
    assert_eq!(sealed_age_from_index(2, 6), 3);
    assert_eq!(sealed_age_from_index(0, 0), 0);
    assert_eq!(age_bucket(0), 0);
    assert_eq!(age_bucket(3), 3);
    assert_eq!(age_bucket(100), AGE_CAP - 1);
}

#[test]
fn note_local_hit_age_buckets() {
    let first = [1u64, 50, 100];
    let mut local = [0u64; AGE_CAP];
    note_local_hit_age(&mut local, &first, 10); // age 2
    note_local_hit_age(&mut local, &first, 60); // age 1
    note_local_hit_age(&mut local, &first, 100); // age 0
    note_local_hit_age(&mut local, &first, 100); // age 0
    note_local_hit_age(&mut local, &first, 0); // ignore
    assert_eq!(local[0], 2);
    assert_eq!(local[1], 1);
    assert_eq!(local[2], 1);
    assert_eq!(local.iter().sum::<u64>(), 4);
    // Flush path is best-effort under parallel tests; still exercise the API.
    add_hit_ages(&local);
    add_hit_age(7);
    let _ = sample_and_reset();
}

fn dummy_probe_diag(txid: [u8; 32]) -> LeftoverProbeDiag {
    LeftoverProbeDiag {
        txid,
        mixed_prefix: [0; 8],
        page_base: 0,
        bits: 16,
        file_id: 0,
        first_fk: 1,
        sealed_age: None,
        hit_empty: true,
        depth_end: 0,
        empty_local: 0,
        page_occupied: 0,
        hop_equal_second: true,
        cands: Vec::new(),
    }
}

/// Next resolve batch (`clear_leftover_miss`) must not drop a leftover hop-dump.
/// Parallel `get_fk_by_txid_batch` used to wipe `diag=1` before the pin.
#[test]
fn clear_leftover_miss_keeps_probe_diag() {
    let ghost = [0xEEu8; 32];
    note_leftover_probe_diag(dummy_probe_diag(ghost));
    note_leftover_miss(LeftoverMissOn::Head, 3);
    clear_leftover_miss();
    assert!(
        take_leftover_miss().is_none(),
        "miss class is per resolve batch"
    );
    assert!(
        leftover_probe_diag_recorded(&ghost),
        "hop-dump txid must survive the next resolve clear"
    );
}
