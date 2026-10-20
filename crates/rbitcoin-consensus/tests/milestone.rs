use bitcoin::hashes::Hash;
use rbitcoin_consensus::milestone::*;

fn anchor(hash: [u8; 32], min_work_be: [u8; 32]) -> Milestone {
    Milestone {
        height: 100,
        anchor: Some(MilestoneAnchor {
            hash: bitcoin::BlockHash::from_byte_array(hash),
            min_work_be,
        }),
    }
}

#[test]
fn none_and_default_never_skip() {
    assert_eq!(Milestone::default(), Milestone::NONE);
    assert!(!Milestone::NONE.skips_scripts_at(0));
    assert!(!Milestone::NONE.skips_scripts_at(1_000_000));
    assert!(!Milestone::NONE.skips_scripts(0, &[0; 32], |_| None));
    assert!(!Milestone::NONE.skips_scripts(1, &[0; 32], |_| None));
}

#[test]
fn height_gate_skips_at_and_below() {
    let m = Milestone::height(100);
    assert!(m.skips_scripts_at(1));
    assert!(m.skips_scripts_at(100));
    assert!(!m.skips_scripts_at(101));
    assert!(m.skips_scripts(50, &[9; 32], |_| None));
}

#[test]
fn low_work_fork_does_not_skip_even_at_the_milestone_height() {
    let anchor_hash = [0x11u8; 32];
    let fork = [0x22u8; 32];
    let honest = [0x33u8; 32];
    let mut min = [0u8; 32];
    min[1] = 0x10;
    let m = anchor(anchor_hash, min);
    assert!(!m.skips_scripts_at(50), "anchor is not a bare height skip");

    let mut headers = std::collections::HashMap::new();
    headers.insert(50u32, fork);
    assert!(!m.skips_scripts(50, &fork, |h| headers.get(&h).copied()));
    headers.insert(100, anchor_hash);
    headers.insert(50, honest);
    assert!(
        !m.skips_scripts(50, &fork, |h| headers.get(&h).copied()),
        "a different hash at the same height is not the anchor's ancestor"
    );
    assert!(
        m.skips_scripts(50, &honest, |h| headers.get(&h).copied()),
        "the milestone hash commits to this ancestor before chain work meets the floor"
    );
    assert!(
        m.skips_scripts(100, &anchor_hash, |h| headers.get(&h).copied()),
        "the anchor block itself is buried"
    );
    let above = [0x44u8; 32];
    headers.insert(101, above);
    assert!(!m.skips_scripts(101, &above, |h| headers.get(&h).copied()));
}
