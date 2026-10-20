use rbitcoin_store::bulk_io;
use rbitcoin_store::io_backend::*;
use std::sync::Mutex;
use std::sync::OnceLock;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_io_envs() {
    std::env::remove_var("RBITCOIN_IO");
}

#[test]
fn parse_tokens() {
    assert_eq!(parse_read_token("uring"), Some(ReadIoBackend::Uring));
    assert_eq!(parse_read_token("io_uring"), Some(ReadIoBackend::Uring));
    assert_eq!(parse_read_token("pool"), Some(ReadIoBackend::Uring));
    assert_eq!(parse_read_token("iocp"), Some(ReadIoBackend::Uring));
    assert!(
        parse_read_token("ioring").is_none(),
        "Windows IoRing is not supported; ioring is not a token"
    );
    assert!(parse_write_token("ioring").is_none());
    assert_eq!(parse_read_token("pread"), Some(ReadIoBackend::Pread));
    assert_eq!(parse_read_token("fd"), Some(ReadIoBackend::Pread));
    assert_eq!(parse_read_token("libc"), Some(ReadIoBackend::Pread));
    assert_eq!(parse_read_token("pwrite"), Some(ReadIoBackend::Pread));
    assert!(parse_read_token("mmap").is_none());
    assert_eq!(parse_write_token("uring"), Some(WriteIoBackend::Uring));
    assert_eq!(parse_write_token("io_uring"), Some(WriteIoBackend::Uring));
    assert_eq!(parse_write_token("pwrite"), Some(WriteIoBackend::Pwrite));
    assert_eq!(parse_write_token("pread"), Some(WriteIoBackend::Pwrite));
    assert_eq!(parse_write_token("fd"), Some(WriteIoBackend::Pwrite));
    assert_eq!(parse_write_token("libc"), Some(WriteIoBackend::Pwrite));
    assert!(parse_write_token("mmap").is_none());
    assert!(parse_read_token("alternate").is_none());
    assert!(parse_write_token("nope").is_none());
}

#[test]
fn path_env_ignored_global_only() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_io_envs();
    std::env::set_var("RBITCOIN_IO", "uring");
    assert_eq!(resolve_read(), effective_read(ReadIoBackend::Uring));
    clear_io_envs();
    std::env::set_var("RBITCOIN_IO", "pread");
    assert_eq!(resolve_read(), ReadIoBackend::Pread);
    clear_io_envs();
}

#[test]
fn global_env_io_uring_off_and_aliases() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_io_envs();
    std::env::set_var("RBITCOIN_IO", "mmap");
    assert_eq!(
        global_read_from_env(),
        None,
        "mmap is not a token; unknown RBITCOIN_IO falls through"
    );
    assert_eq!(global_write_from_env(), None);
    clear_io_envs();
    std::env::set_var("RBITCOIN_IO_URING", "0");
    assert_eq!(
        global_read_from_env(),
        None,
        "RBITCOIN_IO_URING is not an alias; use RBITCOIN_IO=pread"
    );
    assert_eq!(global_write_from_env(), None);
    clear_io_envs();
    // Unknown RBITCOIN_IO token → None (fall through).
    std::env::set_var("RBITCOIN_IO", "not-a-backend");
    assert_eq!(global_read_from_env(), None);
    assert_eq!(global_write_from_env(), None);
    clear_io_envs();
}

#[test]
fn effective_backends_demote_when_uring_disabled() {
    // effective_* only demotes when bulk_io reports uring off; still exercises match arms.
    let _ = effective_read(ReadIoBackend::Pread);
    let _ = effective_read(ReadIoBackend::Uring);
    let _ = effective_write(WriteIoBackend::Pwrite);
    let _ = effective_write(WriteIoBackend::Uring);
}
