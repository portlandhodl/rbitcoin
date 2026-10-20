//! Multi-peer initial block download. Registers onto [`rbitcoin_net::P2PNode`]
//! and the chain hub; `rbitcoin-net` does not depend on this crate.

#[doc(hidden)]
pub mod ibd;

pub use ibd::*;

/// IBD / catch-up across `peers`. Tip-follow stays on [`rbitcoin_net::P2PNode`].
pub async fn sync(
    node: &rbitcoin_net::P2PNode,
    peers: &[rbitcoin_net::NetAddr],
    cfg: ibd::IbdConfig,
) -> Result<u32, rbitcoin_net::NetError> {
    sync_cancellable(node, peers, cfg, None).await
}

/// IBD with optional cooperative cancel flag (SIGINT / SIGTERM path).
pub async fn sync_cancellable(
    node: &rbitcoin_net::P2PNode,
    peers: &[rbitcoin_net::NetAddr],
    cfg: ibd::IbdConfig,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<u32, rbitcoin_net::NetError> {
    ibd::ibd_cancellable(
        std::sync::Arc::clone(&node.hub),
        node.magic,
        node.local_addr,
        peers,
        cfg,
        cancel,
        Some(std::sync::Arc::clone(&node.peers)),
    )
    .await
}

/// IBD with default window (1024 concurrent getdata, 16/peer).
pub async fn sync_default(
    node: &rbitcoin_net::P2PNode,
    peers: &[rbitcoin_net::NetAddr],
) -> Result<u32, rbitcoin_net::NetError> {
    sync(node, peers, ibd::IbdConfig::default()).await
}
