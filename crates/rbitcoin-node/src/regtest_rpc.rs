//! Regtest generate / `submitblock`: mine locally; `submitblock` uses the
//! same [`ChainHub::accept_received_block`] path as P2P `block` messages.

pub use rbitcoin_rpc::HubRegtest;
