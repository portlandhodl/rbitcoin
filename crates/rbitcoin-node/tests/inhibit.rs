use rbitcoin_log::{info, warn};
use rbitcoin_node::inhibit::*;
use std::process::{Child, Command, Stdio};

#[test]
fn try_start_and_drop_is_safe() {
    // On hosts without systemd-inhibit this returns None; with it, Drop reaps.
    let g = SuspendInhibit::try_start("rbitcoin unit test");
    drop(g);
    // Second call still safe.
    let _ = SuspendInhibit::try_start("rbitcoin unit test 2");
}
