use binary_sv2::{Seq064K, Str0255, B016M, B064K};
use bitcoin::hashes::Hash;
use bitcoin::{block, Block, BlockHash, CompactTarget, Transaction, TxMerkleNode, Witness};
use common_messages_sv2::{
    Protocol, SetupConnection, SetupConnectionError, SetupConnectionSuccess,
    ERROR_CODE_SETUP_CONNECTION_PROTOCOL_VERSION_MISMATCH,
    ERROR_CODE_SETUP_CONNECTION_UNSUPPORTED_FEATURE_FLAGS,
    ERROR_CODE_SETUP_CONNECTION_UNSUPPORTED_PROTOCOL, MESSAGE_TYPE_SETUP_CONNECTION,
    MESSAGE_TYPE_SETUP_CONNECTION_ERROR, MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use noise_sv2::Responder;
use rbitcoin_net::{BlockingRegion, ChainHub};
use rbitcoin_sv2::session::{constraints_rebuild_at, Templates};
use rbitcoin_sv2::template;
use rbitcoin_sv2::transport::{Frame, NoiseConn, NoiseWriter};
use rbitcoin_sv2::Sv2TpStats;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use template_distribution_sv2::{
    CoinbaseOutputConstraints, RequestTransactionData, RequestTransactionDataError,
    RequestTransactionDataSuccess, SubmitSolution,
    ERROR_CODE_REQUEST_TRANSACTION_DATA_STALE_TEMPLATE_ID,
    ERROR_CODE_REQUEST_TRANSACTION_DATA_TEMPLATE_ID_NOT_FOUND,
    MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_NEW_TEMPLATE,
    MESSAGE_TYPE_REQUEST_TRANSACTION_DATA, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR,
    MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS, MESSAGE_TYPE_SET_NEW_PREV_HASH,
    MESSAGE_TYPE_SUBMIT_SOLUTION,
};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;

#[test]
fn constraints_rebuild_at_uses_the_one_second_cooldown() {
    let built_at = Instant::now();
    let soon = built_at + Duration::from_millis(999);
    assert_eq!(
        constraints_rebuild_at(built_at, soon),
        Some(built_at + Duration::from_secs(1))
    );
    let due = built_at + Duration::from_secs(1);
    assert_eq!(constraints_rebuild_at(built_at, due), None);
}

#[test]
fn tip_event_rebuilds_a_template_built_before_it() {
    let (old, new) = ([1; 32], [2; 32]);
    let mut t = Templates::default();
    assert!(t.on_tip(old), "first tip");
    t.current_prev = Some(old);
    assert!(!t.on_tip(old), "repeat event with no build since");
    // A build between the store tip move and its event (constraints, or
    // the previous event's rebuild) already sits on the new prev hash.
    t.current_prev = Some(new);
    t.built_since_tip = true;
    assert!(t.on_tip(new), "template may hold the block's confirmed txs");
    assert!(!t.on_tip(new), "no build since the event");
    // A failed reorg disconnects `new` without an event, then its rollback
    // reconnects `new`; a build in that window precedes the second event.
    t.built_since_tip = true;
    assert!(t.on_tip(new), "reconnect of the same hash after a build");
}
