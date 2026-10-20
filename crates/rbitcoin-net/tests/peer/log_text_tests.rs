use super::non_version_before_handshake_log;
use super::unsupported_before_verack_log;
use bitcoin::bip152::{BlockTransactions, BlockTransactionsRequest, HeaderAndShortIds};
use bitcoin::hashes::Hash;
use bitcoin::p2p::address::{AddrV2Message, Address};
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_blockdata::{GetBlocksMessage, GetHeadersMessage, Inventory};
use bitcoin::p2p::message_compact_blocks::{BlockTxn, CmpctBlock, GetBlockTxn, SendCmpct};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::{Magic, ServiceFlags, PROTOCOL_VERSION};
use bitcoin::{Block, BlockHash, Transaction};
use rbitcoin_net::cache::BlockCache;
use rbitcoin_net::chain::{
    accept_block_header_nodos_log, accept_prev_not_found_log, ignoring_low_work_chain_log,
    received_getdata_wtx_log, received_tx_log, synchronizing_blockheaders_log, AcceptOutcome,
    ChainHub,
};
use rbitcoin_net::codec::{
    FramedMessage, MAX_HEADERS_RESULTS, MAX_INV_SIZE, MAX_LOCATOR_SZ, TX_INV_BATCH,
};
use rbitcoin_net::error::NetError;
use rbitcoin_net::msg_decode::decode_framed_offload;
use rbitcoin_net::peer_dos::{PeerRateLimiter, OVERSIZE_BAN_SCORE, RATE_LIMIT_BAN_SCORE};
use rbitcoin_net::peers::{CappedSet, PeerOut, PingAction};
use rbitcoin_net::v2::{
    open_v2, open_v2_with_wire, read_v2_contents, read_v2_frame, read_v2_frame_with_progress,
    write_v2_contents, write_v2_msg, write_v2_msg_offload, V2Reader, V2Writer,
};
use rbitcoin_primitives::Height;
use rbitcoin_query::Query;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};

#[test]
fn peer_command_logs_have_no_raw_newline() {
    let line = non_version_before_handshake_log("inv\nX", 7);
    assert!(!line.contains('\n'), "{line:?}");
    assert!(!line.contains('\r'), "{line:?}");
    let line = unsupported_before_verack_log("ping\r\n", 3);
    assert!(!line.contains('\n'), "{line:?}");
    assert!(!line.contains('\r'), "{line:?}");
}
