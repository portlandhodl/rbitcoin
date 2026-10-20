use bitcoin::block::{Block, Header};
use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_net::cache::*;
use std::collections::HashMap;
use std::sync::RwLock;

#[test]
fn default_body_depth_is_compact_serve_window() {
    assert_eq!(DEFAULT_BODY_DEPTH, 16);
}
