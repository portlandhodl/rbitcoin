use rbitcoin_rpc::auth::{
    default_socket_path, default_token_path, read_cookie_file, RpcAuth, RpcCookiePerms,
};
use rbitcoin_rpc::methods::{
    difficulty_rpc_f64, gbt_template, submit_received_block, HubRegtest, RpcActive, RpcContext,
    RpcRegtest, SubmitBlockOutcome,
};
use rbitcoin_rpc::server::{
    run_rpc, RpcConfig, RpcHandle, DEFAULT_RPC_WORK_QUEUE, RPC_MAX_HTTP_BODY,
};
#[test]
fn node_rpc_path_is_root() {
    assert_eq!(rbitcoin_rpc::node_rpc_path(), "/");
}
