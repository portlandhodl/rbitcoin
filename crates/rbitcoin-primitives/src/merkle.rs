//! Bitcoin block merkle root from leaf txids (internal byte order).
//!
//! The mutation flag is Core's `ComputeMerkleRoot` check (CVE-2012-2459):
//! some level held two equal hashes in one pair before odd padding, so a
//! shorter tx list has the same root.

use bitcoin_hashes::{sha256, Hash, HashEngine};

/// Bitcoin block merkle root from leaf txids (internal byte order).
pub fn merkle_root_from_txids(leaves: &[[u8; 32]]) -> [u8; 32] {
    merkle_root_mutated(leaves).0
}

/// [`merkle_root_from_txids`] plus Core's `ComputeMerkleRoot` mutation flag
/// (CVE-2012-2459).
pub fn merkle_root_mutated(leaves: &[[u8; 32]]) -> ([u8; 32], bool) {
    if leaves.is_empty() {
        return ([0u8; 32], false);
    }
    let mut mutated = false;
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        mutated |= level.chunks_exact(2).any(|pair| pair[0] == pair[1]);
        level = merkle_parent_level(level);
    }
    (level[0], mutated)
}

/// Sibling hashes from `leaves[index]` up to the root, deepest first. Empty for
/// one leaf. Folding the leaf with them (left when the index bit is 0)
/// reproduces [`merkle_root_from_txids`]. The leaf's own value feeds no entry.
pub fn merkle_branch(leaves: &[[u8; 32]], mut index: usize) -> Vec<[u8; 32]> {
    let mut branch = Vec::new();
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let sibling = (index ^ 1).min(level.len() - 1);
        branch.push(level[sibling]);
        level = merkle_parent_level(level);
        index /= 2;
    }
    branch
}

/// Root reached by folding `leaf` at `index` with its [`merkle_branch`]. The
/// leaf is the left child when the index bit at that depth is 0.
pub fn merkle_root_from_branch(leaf: [u8; 32], branch: &[[u8; 32]], index: usize) -> [u8; 32] {
    branch
        .iter()
        .enumerate()
        .fold(leaf, |acc, (depth, sibling)| {
            if (index >> depth) & 1 == 0 {
                hash256_concat(&acc, sibling)
            } else {
                hash256_concat(sibling, &acc)
            }
        })
}

#[doc(hidden)]
pub fn merkle_parent_level(mut level: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
    if level.len() % 2 == 1 {
        level.push(level[level.len() - 1]);
    }
    level
        .chunks_exact(2)
        .map(|pair| hash256_concat(&pair[0], &pair[1]))
        .collect()
}

#[doc(hidden)]
pub fn hash256_concat(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut eng = sha256::HashEngine::default();
    eng.input(a);
    eng.input(b);
    let mid = sha256::Hash::from_engine(eng);
    let mut eng2 = sha256::HashEngine::default();
    eng2.input(mid.as_byte_array());
    sha256::Hash::from_engine(eng2).to_byte_array()
}
