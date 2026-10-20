use rbitcoin_node::cli_main;
use std::process::ExitCode;

#[global_allocator]
#[doc(hidden)]
pub static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[doc(hidden)]
pub fn main() -> ExitCode {
    cli_main(std::env::args_os())
}
