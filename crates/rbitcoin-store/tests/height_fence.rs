use rbitcoin_primitives::{Fk, Height};
use rbitcoin_store::chain::{ConfirmedTable, HeaderTxsTable};
use rbitcoin_store::error::StoreError;
use rbitcoin_store::height_fence::*;
use std::sync::Arc;

fn run(first: u64, count: u32, height: u32) -> FenceRun {
    FenceRun {
        first_fk: first,
        count,
        height,
    }
}

#[test]
fn mtp_ring_sequential_shift_and_tip_only() {
    let mut r = MtpRing::empty();
    for h in 0..12u32 {
        assert!(r.try_push(h, 1_000 + h * 10), "push {h}");
    }
    let (n, buf) = r.window_at(11).expect("tip window");
    assert_eq!(n, 11);
    assert_eq!(
        &buf[..11],
        &[1010, 1020, 1030, 1040, 1050, 1060, 1070, 1080, 1090, 1100, 1110]
    );
    assert!(
        r.window_at(10).is_none(),
        "historical height is not the ring"
    );
    assert!(!r.try_push(13, 9), "hole is not sequential");
    assert!(r.window_at(11).is_some());
    r.clear();
    assert!(r.window_at(11).is_none());
    assert!(!r.try_push(5, 1), "cold ring only accepts genesis");
    assert!(r.try_push(0, 42));
    let (n, buf) = r.window_at(0).unwrap();
    assert_eq!(n, 1);
    assert_eq!(buf[0], 42);
}

#[test]
fn height_fence_adjacent_runs_and_edges() {
    let f = HeightFence::from_runs(vec![run(1, 2, 0), run(3, 3, 1)]);
    assert_eq!(f.height_of(Fk(1)), Some(0));
    assert_eq!(f.height_of(Fk(2)), Some(0));
    assert_eq!(f.height_of(Fk(3)), Some(1));
    assert_eq!(f.height_of(Fk(5)), Some(1));
    assert_eq!(f.height_of(Fk(6)), None);
    assert_eq!(f.height_of(Fk::NULL), None);
    assert_eq!(
        f.get_batch(&[Fk(2), Fk(4), Fk(9)]),
        vec![Some(0), Some(1), None]
    );
}

#[test]
fn height_fence_reorg_hole_is_unconnected() {
    // Confirmed h=0 fks 1..=2, then a discarded block used 3..=5, then
    // confirmed h=1 is a new archive at 6..=8.
    let f = HeightFence::from_runs(vec![run(1, 2, 0), run(6, 3, 1)]);
    assert_eq!(f.height_of(Fk(2)), Some(0));
    assert_eq!(
        f.height_of(Fk(3)),
        None,
        "orphaned fk must not take neighbor height"
    );
    assert_eq!(f.height_of(Fk(5)), None);
    assert_eq!(f.height_of(Fk(6)), Some(1));
    assert_eq!(f.height_of(Fk(8)), Some(1));
}

#[test]
fn height_fence_extend_and_pop_tip() {
    let mut f = HeightFence::empty();
    f.extend(0, Fk(1), 2);
    f.extend(1, Fk(3), 1);
    assert_eq!(f.height_of(Fk(3)), Some(1));
    f.pop_height(1);
    assert_eq!(f.height_of(Fk(3)), None);
    assert_eq!(f.height_of(Fk(1)), Some(0));
    f.pop_height(0);
    assert!(f.is_empty());
}

#[test]
fn height_fence_clone_shares_runs_until_extend() {
    let mut f = HeightFence::from_runs(vec![run(1, 2, 0)]);
    let snap = f.clone();
    assert!(f.shares_runs(&snap), "snapshot must be Arc, not a vec copy");
    assert_eq!(snap.height_of(Fk(1)), Some(0));
    f.extend(1, Fk(3), 1);
    assert!(
        !f.shares_runs(&snap),
        "extend COWs away from live snapshots"
    );
    assert_eq!(snap.height_of(Fk(3)), None);
    assert_eq!(f.height_of(Fk(3)), Some(1));
    assert_eq!(snap.len(), 1);
    assert_eq!(f.len(), 2);
}

#[test]
fn height_fence_max_connected_fk_is_last_run_end() {
    assert_eq!(HeightFence::empty().max_connected_fk(), 0);
    let f = HeightFence::from_runs(vec![run(1, 2, 0), run(10, 3, 2)]);
    // last connected run is 10..=12, not 2 — holes must not use last_tip_fk.
    assert_eq!(f.max_connected_fk(), 12);
    assert_eq!(f.height_of(Fk(12)), Some(2));
    assert_eq!(f.height_of(Fk(3)), None);
}

#[test]
fn height_fence_fk_before_first_and_empty() {
    let f = HeightFence::from_runs(vec![run(10, 2, 4)]);
    assert_eq!(f.height_of(Fk(9)), None);
    assert_eq!(f.height_of(Fk(10)), Some(4));
    assert_eq!(f.height_of(Fk(12)), None);
    assert_eq!(HeightFence::empty().height_of(Fk(1)), None);
}

#[test]
fn height_fence_max_height_is_highest_run_not_last_fk() {
    assert_eq!(HeightFence::empty().max_height(), None);
    let f = HeightFence::from_runs(vec![run(1, 2, 0), run(10, 3, 2)]);
    assert_eq!(f.max_height(), Some(2));
    assert_eq!(f.max_connected_fk(), 12);
}

/// Mainnet 258870: fence tip 40, drain still on height 2 → ready height 2.
#[test]
fn drain_and_fence_hi_is_min_of_drain_height_and_fence_tip() {
    assert_eq!(HeightFence::empty().drain_and_fence_hi(1), None);
    let f = HeightFence::from_runs(vec![run(1, 50, 2), run(51, 50, 40)]);
    assert_eq!(f.drain_and_fence_hi(0), None, "no drain: keep all inflight");
    assert_eq!(
        f.drain_and_fence_hi(40),
        Some(2),
        "drain on height 2 while fence tip is 40"
    );
    assert_eq!(f.max_height(), Some(40));
    assert_eq!(f.drain_and_fence_hi(90), Some(40), "drain on fence tip");
    assert_eq!(
        f.drain_and_fence_hi(200),
        None,
        "drain fk not on fence (drain ahead of fence)"
    );
}

#[test]
fn height_fence_covers_fk_span() {
    assert!(!HeightFence::empty().covers_fk_span(1, 1));
    let f = HeightFence::from_runs(vec![run(1, 2, 0), run(6, 3, 1)]);
    assert!(f.covers_fk_span(1, 2));
    assert!(f.covers_fk_span(6, 8));
    assert!(!f.covers_fk_span(3, 5), "hole is not leftover-visible");
    assert!(
        !f.covers_fk_span(1, 8),
        "span that includes a hole is not covered"
    );
    assert!(!f.covers_fk_span(8, 10), "past last run end");
    assert!(f.covers_fk_span(7, 7));
}

#[test]
fn height_fence_unconnected_ranges() {
    assert_eq!(
        HeightFence::empty().unconnected_ranges(0),
        Vec::<(u64, u64)>::new()
    );
    assert_eq!(
        HeightFence::empty().unconnected_ranges(10),
        vec![(1, 11)],
        "empty fence: the whole 1..=last_fk span is leftover"
    );
    let abut = HeightFence::from_runs(vec![run(1, 2, 0), run(3, 3, 1)]);
    assert_eq!(abut.unconnected_ranges(5), Vec::<(u64, u64)>::new());
    assert_eq!(abut.unconnected_ranges(8), vec![(6, 9)]);
    let hole = HeightFence::from_runs(vec![run(1, 2, 0), run(6, 3, 1)]);
    assert_eq!(hole.unconnected_ranges(8), vec![(3, 6)]);
    assert_eq!(hole.unconnected_ranges(10), vec![(3, 6), (9, 11)]);
    let late = HeightFence::from_runs(vec![run(10, 2, 4)]);
    assert_eq!(late.unconnected_ranges(12), vec![(1, 10), (12, 13)]);
    assert_eq!(late.unconnected_ranges(9), vec![(1, 10)]);
}
