use bitcoin_hashes::{sha256, Hash, HashEngine};
use rbitcoin_primitives::merkle::*;

#[test]
fn merkle_root_single_and_pair() {
    let a = [1u8; 32];
    assert_eq!(merkle_root_from_txids(&[a]), a);
    let b = [2u8; 32];
    let root = merkle_root_from_txids(&[a, b]);
    assert_eq!(root, hash256_concat(&a, &b));
}

/// CVE-2012-2459: a repeated tail at any level keeps the root and is
/// flagged. The shorter list and an unpaired repeat are not.
#[test]
fn merkle_mutation_flags_a_repeated_tail_at_any_level() {
    let l: Vec<[u8; 32]> = (1..=6u8).map(|n| [n; 32]).collect();
    let (root3, mutated) = merkle_root_mutated(&l[..3]);
    assert!(!mutated);
    assert_eq!(
        merkle_root_mutated(&[l[0], l[1], l[2], l[2]]),
        (root3, true)
    );
    assert!(!merkle_root_mutated(&[l[0], l[1], l[1]]).1);

    let (root6, mutated) = merkle_root_mutated(&l);
    assert!(!mutated);
    let mut tail = l.clone();
    tail.extend_from_slice(&l[4..]);
    assert_eq!(merkle_root_mutated(&tail), (root6, true));
    assert_eq!(merkle_root_mutated(&[]), ([0u8; 32], false));
}

#[test]
fn merkle_path_folds_to_root_at_every_index() {
    let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    assert!(merkle_branch(&[a], 0).is_empty());
    assert_eq!(merkle_branch(&[a, b], 0), vec![b]);
    assert_eq!(
        merkle_branch(&[a, b, c], 0),
        vec![b, hash256_concat(&c, &c)]
    );
    assert_eq!(
        merkle_branch(&[a, b, c], 2),
        vec![c, hash256_concat(&a, &b)]
    );
    for n in 1..=9u8 {
        let leaves: Vec<[u8; 32]> = (0..n).map(|i| [i + 10; 32]).collect();
        let root = merkle_root_from_txids(&leaves);
        for (index, leaf) in leaves.iter().enumerate() {
            let branch = merkle_branch(&leaves, index);
            assert_eq!(
                merkle_root_from_branch(*leaf, &branch, index),
                root,
                "n={n} index={index}"
            );
        }
    }
}
