use rbitcoin_primitives::Fk;
use rbitcoin_store::head_resolve_pick::*;
use rbitcoin_store::height_fence::FenceRun;
use rbitcoin_store::height_fence::HeightFence;
use rbitcoin_store::int_map::{U64Map, U64Set};

fn fence_on(fks: &[u64]) -> HeightFence {
    let runs: Vec<FenceRun> = fks
        .iter()
        .map(|&id| FenceRun {
            first_fk: id,
            count: 1,
            height: 0,
        })
        .collect();
    HeightFence::from_runs(runs)
}

fn ids(pairs: &[(u64, [u8; 32])]) -> U64Map<[u8; 32]> {
    pairs.iter().copied().collect()
}

fn walked(
    cands: &[Fk],
    filled: usize,
    want: &[u8; 32],
    id_of: &U64Map<[u8; 32]>,
    heights: Option<&HeightFence>,
) -> PrefixWalk {
    walk_id_prefix(cands, filled, want, id_of, heights, false)
}

#[test]
fn id_idx_stop_after_connected_skips_older_cand() {
    let want = [0xAAu8; 32];
    let other = [0xBBu8; 32];
    let cands = [Fk(10), Fk(20), Fk(30)];
    let ht = fence_on(&[20]);
    // Newest unconnected match is not a fence winner.
    let map1 = ids(&[(10, want)]);
    assert!(walked(&cands, 1, &want, &map1, Some(&ht)).winner.is_none());
    // Connected match at rank 2 wins; older Fk(30) is irrelevant.
    let map2 = ids(&[(10, want), (20, want)]);
    let (fk, rank) = walked(&cands, 2, &want, &map2, Some(&ht)).winner.unwrap();
    assert_eq!(fk, Fk(20));
    assert_eq!(rank, 2);
    let _ = other;
}

#[test]
fn id_idx_stop_after_connected_unconnected_only_peeks_all() {
    let want = [0xAAu8; 32];
    let cands = [Fk(10), Fk(20)];
    let ht = fence_on(&[99]);
    let map = ids(&[(10, want), (20, want)]);
    let w = walked(&cands, 2, &want, &map, Some(&ht));
    assert!(w.winner.is_none());
    assert!(w.had_body);
    assert_eq!(w.miss, 0);
    let fb = walk_id_prefix(&cands, 2, &want, &map, Some(&ht), true);
    assert_eq!(fb.winner, Some((Fk(10), 1)));
    assert_eq!(fb.miss, 0);
}

#[test]
fn no_fence_stops_at_first_txid_match() {
    let want = [0xAAu8; 32];
    let cands = [Fk(1), Fk(2)];
    let map = ids(&[(1, want)]);
    let (fk, rank) = walked(&cands, 1, &want, &map, None).winner.unwrap();
    assert_eq!((fk, rank), (Fk(1), 1));
}

/// Full cand list + complete id_map: newest unconnected body match does
/// not win when a shallower cand is fence-connected.
#[test]
fn full_map_connected_beats_newer_unconnected() {
    let want = [0xAAu8; 32];
    let cands = [Fk(10), Fk(20)];
    let ht = fence_on(&[20]);
    let map = ids(&[(10, want), (20, want)]);
    let (fk, rank) = walked(&cands, cands.len(), &want, &map, Some(&ht))
        .winner
        .unwrap();
    assert_eq!(fk, Fk(20));
    assert_eq!(rank, 2);
}

/// A cand with no `id_map` entry is not a match (same as wrong body).
#[test]
fn missing_id_map_entry_is_not_a_match() {
    let want = [0xAAu8; 32];
    let cands = [Fk(10), Fk(20)];
    let ht = fence_on(&[10, 20]);
    let map = ids(&[(20, want)]);
    let (fk, rank) = walked(&cands, cands.len(), &want, &map, Some(&ht))
        .winner
        .unwrap();
    assert_eq!(fk, Fk(20));
    assert_eq!(rank, 2);
    let empty = ids(&[]);
    assert!(walked(&cands, cands.len(), &want, &empty, Some(&ht))
        .winner
        .is_none());
}

#[test]
fn leftover_miss_classifies_head_body_fence() {
    assert_eq!(classify_leftover_miss(0, false), LeftoverMissOn::Head);
    assert_eq!(classify_leftover_miss(3, false), LeftoverMissOn::Body);
    assert_eq!(classify_leftover_miss(3, true), LeftoverMissOn::Fence);
    assert_eq!(LeftoverMissOn::Head.as_str(), "head");
    assert_eq!(LeftoverMissOn::Body.as_str(), "body");
    assert_eq!(LeftoverMissOn::Fence.as_str(), "fence");
}

#[test]
fn miss_peeks_counts_wrong_identity_only() {
    let want = [0xAAu8; 32];
    let cands = [Fk(1), Fk(2)];
    let map = ids(&[(1, [0x00; 32]), (2, want)]);
    let w = walked(&cands, 2, &want, &map, None);
    assert_eq!(w.miss, 1);
    assert_eq!(w.winner, Some((Fk(2), 2)));
    let after = [Fk(1), Fk(2), Fk(3)];
    let map_after = ids(&[(1, want), (2, [0x00; 32]), (3, [0x11; 32])]);
    let w_after = walked(&after, 3, &want, &map_after, None);
    assert_eq!(w_after.winner, Some((Fk(1), 1)));
    assert_eq!(w_after.miss, 2, "entries after the winner still miss");
}

#[test]
fn next_id_shot_first_chunk_then_rest() {
    let cands = vec![vec![Fk(10), Fk(20), Fk(30), Fk(40), Fk(50), Fk(60)]];
    let filled = vec![0usize];
    let skip = vec![false];
    let a = next_id_shot(&cands, &filled, &skip, ID_FILL_CHUNK);
    assert_eq!(a, vec![Fk(10), Fk(20), Fk(30), Fk(40)]);
    let filled = vec![ID_FILL_CHUNK];
    let b = next_id_shot(&cands, &filled, &skip, usize::MAX);
    assert_eq!(b, vec![Fk(50), Fk(60)]);
}

#[test]
fn two_shot_skips_tail_after_connected_in_chunk() {
    let want = [0xAAu8; 32];
    let cands = vec![vec![Fk(10), Fk(20), Fk(30), Fk(40), Fk(50)]];
    let mut filled = vec![0usize];
    let mut skip = vec![false];
    let shot_a = next_id_shot(&cands, &filled, &skip, ID_FILL_CHUNK);
    assert_eq!(shot_a.len(), ID_FILL_CHUNK);
    filled[0] = ID_FILL_CHUNK;
    let map = ids(&[(10, want), (20, want)]);
    let ht = fence_on(&[20]);
    assert!(walked(&cands[0], filled[0], &want, &map, Some(&ht))
        .winner
        .is_some());
    skip[0] = true;
    let shot_b = next_id_shot(&cands, &filled, &skip, usize::MAX);
    assert!(
        shot_b.is_empty(),
        "connected in shot A must not fetch the tail"
    );
}

#[test]
fn two_shot_unconnected_body_match_still_takes_rest() {
    let want = [0xAAu8; 32];
    let cands = vec![vec![Fk(10), Fk(20), Fk(30), Fk(40), Fk(50)]];
    let mut filled = vec![0usize];
    let skip = vec![false];
    let _shot_a = next_id_shot(&cands, &filled, &skip, ID_FILL_CHUNK);
    filled[0] = ID_FILL_CHUNK;
    let map = ids(&[(10, want)]);
    let ht = fence_on(&[50]);
    assert!(
        walked(&cands[0], filled[0], &want, &map, Some(&ht))
            .winner
            .is_none(),
        "unconnected match in the chunk is not a fence win"
    );
    let shot_b = next_id_shot(&cands, &filled, &skip, usize::MAX);
    assert_eq!(shot_b, vec![Fk(50)]);
}
