//! Node lifecycle, configuration, and process orchestration.

#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod health;
#[doc(hidden)]
pub mod inhibit;
#[doc(hidden)]
pub mod lock;
#[doc(hidden)]
pub mod regtest_rpc;
#[doc(hidden)]
pub mod run;
#[doc(hidden)]
pub mod tor_control;

pub use cli::cli_main;
pub use config::{
    DatadirOpts, ListenOpts, MempoolOpts, NodeConfig, P2pListen, RpcOpts, Sv2AuthoritySecret,
    TorControlOpts,
};
pub use error::NodeError;
pub use run::{run_node, run_p2p, NodeHandle};
