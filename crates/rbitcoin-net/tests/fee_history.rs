use rbitcoin_mempool::CONFIDENCE_FAR;
use rbitcoin_mempool::{AnalogHistory, CONFIDENCE_NEAR, HISTORICAL_CONFIDENCE_FAR};
use rbitcoin_net::fee_history::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

fn block(rate: Option<u64>, bytes: u64) -> HistoricalFeeBlock {
    HistoricalFeeBlock {
        p10_sat_kvb: rate,
        txstat_bytes: bytes,
    }
}

fn calm(i: u32) -> u64 {
    1_000 + (u64::from(i) * 7_919) % 200
}

#[test]
fn farther_targets_use_the_interpolated_95_percent() {
    let targets = [1u32, 2, 10];
    let mut history = FeeHistory::new(u64::MAX, &targets);
    // Twelve higher hurdles in each hundred. Short windows clear the
    // 95% on that run; a 10-block window does so only about 3% of the
    // time, under 95% and over 99%.
    for height in 0..2_500u32 {
        let hurdle = if height % 100 < 12 { 1_200 } else { 1_000 };
        history.insert(height, block(Some(hurdle), 8), None);
    }
    let rates = history.rates();
    let short = rates[&2].unwrap();
    let long = rates[&10].unwrap();
    assert!(short > long, "published 2-block {short} vs 10-block {long}");
    assert_eq!(rates[&10], history.analog.rate_sat_kvb(10, 0.95));
    assert_ne!(rates[&10], history.analog.rate_sat_kvb(10, CONFIDENCE_FAR));
}

#[test]
fn txstat_byte_budget_keeps_recent_heights_and_tracks_reorgs() {
    let mut history = FeeHistory::new(16, &[1]);
    for height in 1..=3 {
        history.insert(height, block(Some(u64::from(height) * 100), 8), None);
    }
    assert_eq!(
        history.entries().iter().map(|e| e.0).collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(history.txstat_bytes, 16);

    history.insert(3, block(Some(350), 4), None);
    assert_eq!(history.txstat_bytes, 12);
    history.insert(2, block(Some(225), 8), None);
    assert_eq!(history.entries(), [(2, block(Some(225), 8))]);
    assert_eq!(history.txstat_bytes, 8);
}

#[test]
fn fee_history_gaps_cache_and_reorg_share_one_history() {
    let targets = [1, 2];
    let mut history = FeeHistory::new(u64::MAX, &targets);
    let mut hurdles_only = FeeHistory::new(u64::MAX, &targets);
    let mut next = 0u32;
    for height in 0..2_600u32 {
        let mut hash = [0u8; 32];
        hash[..4].copy_from_slice(&height.to_le_bytes());
        if height % 10 == 9 {
            history.insert(height, block(None, 8), Some(hash));
        } else {
            history.insert(height, block(Some(calm(height)), 8), Some(hash));
            hurdles_only.insert(next, block(Some(calm(height)), 8), None);
            next += 1;
        }
        if height == 199 {
            let hashes = history.recent_hashes();
            assert_eq!(hashes.len(), RECENT_HASHES);
            assert_eq!(hashes[0].0, 200 - RECENT_HASHES as u32);
            // A later cut, after the window has slid past this height,
            // drops every retained hash. The reorg has to land while 149
            // is still inside the window. The replacement keeps the same
            // hurdle so the observation sequence does not change.
            history.insert(150, block(Some(calm(150)), 8), None);
            assert_eq!(history.recent_hashes().last().map(|h| h.0), Some(149));
            assert_eq!(history.entries().len(), 151);
            for replay in 151u32..=199 {
                let mut hash = [0u8; 32];
                hash[..4].copy_from_slice(&replay.to_le_bytes());
                if replay % 10 == 9 {
                    history.insert(replay, block(None, 8), Some(hash));
                } else {
                    history.insert(replay, block(Some(calm(replay)), 8), Some(hash));
                }
            }
        }
    }
    assert_eq!(history.analog.pairs(2), hurdles_only.analog.pairs(2));
    assert_eq!(history.rates(), hurdles_only.rates());
    let near = history.analog.rate_sat_kvb(1, CONFIDENCE_NEAR);
    let far = history.analog.rate_sat_kvb(1, CONFIDENCE_FAR);
    assert_eq!(history.rates()[&1], near);
    assert_ne!(
        near, far,
        "one-block confidence is nearer than a farther target"
    );
    assert!(history.rates()[&1].is_some());
    assert!(history.rates()[&2].is_some());
    assert!(history.rates.is_some(), "cached");
    history.insert_if_absent(5, block(Some(1), 8), None);
    assert!(history.rates.is_some(), "held height changes nothing");
    history.insert(2_600, block(Some(1_000), 8), None);
    assert!(history.rates.is_none(), "a connect clears the cache");

    let hashes = history.recent_hashes();
    assert_eq!(hashes.len(), RECENT_HASHES);
    assert_eq!(hashes[0].0, 2_600 - RECENT_HASHES as u32);
}

#[test]
fn every_update_path_matches_a_fresh_history() {
    let mut book = FeeHistory::new(u64::MAX, &[1]);
    book.insert_if_absent(10, block(Some(100), 1), None);
    for h in 11..14u32 {
        book.insert_if_absent(h, block(Some(100 + u64::from(h)), 1), None);
    }
    let after_back = book.analog.pairs(1);
    assert!(
        after_back >= 1,
        "an empty history and heights above the last extend the analog"
    );
    assert!(!book.analog_stale);
    book.insert_if_absent(1, block(Some(50), 1), None);
    assert!(
        book.analog.pairs(1) > after_back,
        "a height below the first extends the analog"
    );
    assert!(!book.analog_stale);

    let mut replaced = FeeHistory::new(u64::MAX, &[1]);
    for h in 1..=4u32 {
        replaced.insert(h, block(Some(100), 1), None);
    }
    let before_replace = replaced.analog.pairs(1);
    replaced.insert(5, block(None, 1), None);
    replaced.insert(5, block(Some(200), 1), None);
    assert_eq!(replaced.analog.pairs(1), before_replace + 1);
    assert!(!replaced.analog_stale);

    let mut evicted = FeeHistory::new(10, &[1]);
    evicted.insert(1, block(None, 10), None);
    for h in 2..=6u32 {
        evicted.insert(h, block(Some(100 + u64::from(h)), 1), None);
    }
    assert!(!evicted.analog_stale);
    assert_eq!(
        evicted.analog.pairs(1),
        2,
        "evicting a hurdle-less height keeps the five later observations"
    );
    evicted.insert(7, block(Some(300), 10), None);
    assert!(
        evicted.analog.pairs(1) < 2,
        "evicting a hurdle drops it from the analog"
    );

    let targets = [1, 2, 6];
    let mut history = FeeHistory::new(20_000, &targets);
    // preload walks down from 3000; connects arrive above it
    for height in (1_000..=3_000u32).rev() {
        history.insert_if_absent(height, block(Some(calm(height)), 8), None);
    }
    history.insert(3_001, block(Some(5_000), 8), None);
    history.insert(3_002, block(None, 8), None);
    // reorg: 3001 replaced, 3002 gone
    history.insert(3_001, block(Some(700), 8), None);
    // a gap fill between held heights marks the analog stale
    history.truncate_above(2_990);
    history.insert(2_995, block(Some(900), 8), None);
    history.insert_if_absent(2_993, block(Some(800), 8), None);
    assert!(history.analog_stale);
    // budget eviction pops the oldest
    for height in 2_996..3_600u32 {
        history.insert(height, block(Some(calm(height)), 8), None);
    }

    let mut fresh = FeeHistory::new(20_000, &targets);
    for (height, b) in history.entries() {
        fresh.insert(height, b, None);
    }
    assert_eq!(history.rates(), fresh.rates());
    assert!(!history.analog_stale);
    let mut rebuilt = AnalogHistory::new(&targets);
    rebuilt.rebuild(history.entries().iter().filter_map(|(_, b)| b.p10_sat_kvb));
    for n in targets {
        assert_eq!(history.analog.pairs(n), rebuilt.pairs(n), "N={n}");
    }
}
