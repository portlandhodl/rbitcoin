/// Header-only chain of `n` blocks on `parent`, labeled from `first` so
/// sibling forks never share a hash. Returns the headers in order.
fn put_header_fork(
    q: &Query,
    parent: (Fk, [u8; 32]),
    first: u32,
    n: u32,
) -> Vec<HeaderRecord> {
    let (mut prev_fk, mut prev_hash) = parent;
    (first..first + n)
        .map(|label| {
            let (h, _) = coinbase_block(label, prev_fk, Some(prev_hash));
            prev_fk = q.put_header(&h).unwrap();
            prev_hash = h.hash;
            h
        })
        .collect()
}

fn path_hashes(path: &[ResumeWorkEntry]) -> Vec<[u8; 32]> {
    path.iter().map(|e| e.hash).collect()
}

/// Resume after a restart walks the most-work header lineage from the
/// confirmed tip: a heavier header-only sibling beats a loser tip that has
/// a body, the ancestor walk reaches forks under the grandparent and takes
/// the heaviest, `exclude` falls back to the next child, a deep band ahead
/// of the tip does not overflow the stack, and a `prev_fk` cycle ends.
#[test]
fn resume_most_work_header_path() {
    let (dir, q) = temp_query("resume-most-work-journey");
    let (g, tg) = coinbase_block(0, Fk::NULL, None);
    let gfk = q.connect_block(Height(0), &g, &[tg]).unwrap();
    let (p, tp) = coinbase_block(1, gfk, Some(g.hash));
    let pfk = q.connect_block(Height(1), &p, &[tp]).unwrap();
    let (lose, tl) = coinbase_block(2, pfk, Some(p.hash));
    q.connect_block(Height(2), &lose, &[tl]).unwrap();
    assert_eq!(q.tip_height(), Some(Height(2)));

    let w = put_header_fork(&q, (pfk, p.hash), 21, 2);
    let path = q.resume_work_path_after_tip(lose.hash, 2, 8).unwrap();
    assert_eq!(
        path_hashes(&path)[..2],
        [w[0].hash, w[1].hash],
        "from the loser tip, the heavier sibling at tip height"
    );
    assert_eq!(path[0].height, 2);
    assert!(!path[0].has_body, "the winner's first hop has no body yet");
    let from_parent = q.resume_work_path_after_tip(p.hash, 1, 8).unwrap();
    assert_eq!(
        from_parent[0].hash, w[0].hash,
        "more work beats the loser's Class A body"
    );
    let excluded = q
        .resume_work_path_after_tip_excluding(p.hash, 1, 8, &[w[0].hash])
        .unwrap();
    assert_eq!(excluded[0].hash, lose.hash, "exclude falls back to the loser");
    assert!(excluded[0].has_body);

    let y = put_header_fork(&q, (gfk, g.hash), 31, 3);
    let x = put_header_fork(&q, (gfk, g.hash), 41, 4);
    let path = q.resume_work_path_after_tip(lose.hash, 2, 8).unwrap();
    assert_eq!(
        path[0].hash, w[0].hash,
        "the nearest better sibling wins before the ancestor walk climbs"
    );
    let without_w = |max| {
        q.resume_work_path_after_tip_excluding(lose.hash, 2, max, &[w[0].hash])
            .unwrap()
    };
    let path = without_w(8);
    assert_eq!(
        path_hashes(&path)[..2],
        [x[0].hash, x[1].hash],
        "the ancestor walk takes the heaviest fork under the grandparent"
    );
    assert_eq!(path[0].height, 1);
    assert_ne!(path[0].hash, y[0].hash);

    let x_tip = x.last().unwrap();
    let x_tip_fk = q.get_header_by_hash(&x_tip.hash).unwrap().unwrap().0;
    // Longer than the 32-cap. Scored on a stack a recursive walk of this
    // band would overflow; the production walk is a heap stack.
    const DEEP: u32 = 256;
    put_header_fork(&q, (x_tip_fk, x_tip.hash), 1_000, DEEP);
    let path = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 * 1024)
            .spawn_scoped(scope, || without_w(32))
            .expect("spawn")
            .join()
            .expect("deep resume walk")
    });
    assert_eq!(path.len(), 32, "capped walk length");
    assert_eq!((path[0].height, path[31].height), (1, 32));

    let mut children: rbitcoin_query::U64Map<Vec<(Fk, [u8; 32])>> = rbitcoin_query::U64Map::default();
    children.insert(gfk.0, vec![(pfk, p.hash)]);
    children.insert(pfk.0, vec![(gfk, g.hash)]);
    let n = q.store().header_count() as usize;
    let index = rbitcoin_query::ResumeHeaderIndex {
        children,
        bits: vec![0x1d00ffff; n],
        prevs: vec![0; n],
    };
    let mut memo = rbitcoin_query::U64Map::default();
    let (_w, d) = rbitcoin_query::Query::resume_subtree_score(&index, gfk, &mut memo)
        .expect("a prev_fk cycle must not hang");
    assert!(memo.contains_key(&gfk.0) && memo.contains_key(&pfk.0));
    assert!(d >= 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// One hard header beats a longer run of easy headers.
#[test]
fn resume_shorter_heavier_header_beats_longer_easy_fork() {
    use bitcoin::{CompactTarget, Target};
    let (dir, q) = temp_query("resume-short-heavy");
    let (g, tg) = coinbase_block(0, Fk::NULL, None);
    let gfk = q.connect_block(Height(0), &g, &[tg]).unwrap();
    let (p, tp) = coinbase_block(1, gfk, Some(g.hash));
    let pfk = q.connect_block(Height(1), &p, &[tp]).unwrap();
    let (tip, tt) = coinbase_block(2, pfk, Some(p.hash));
    q.connect_block(Height(2), &tip, &[tt]).unwrap();

    let easy_bits = 0x207f_ffffu32;
    let hard_bits = 0x1d00_ffffu32;
    let easy = Target::from_compact(CompactTarget::from_consensus(easy_bits)).to_work();
    let hard = Target::from_compact(CompactTarget::from_consensus(hard_bits)).to_work();
    let mut easy_chain = easy;
    for _ in 0..7 {
        easy_chain = easy_chain + easy;
    }
    assert!(hard > easy_chain);

    let long = put_header_fork(&q, (gfk, g.hash), 50, 8);
    let (mut short, _) = coinbase_block(70, gfk, Some(g.hash));
    short.bits = hard_bits;
    rehash_header(&mut short, &g.hash);
    q.put_header(&short).unwrap();

    let path = q
        .resume_work_path_after_tip_excluding(tip.hash, 2, 8, &[p.hash])
        .unwrap();
    assert_eq!(path[0].hash, short.hash);
    assert_ne!(path[0].hash, long[0].hash);
    let _ = std::fs::remove_dir_all(dir);
}
