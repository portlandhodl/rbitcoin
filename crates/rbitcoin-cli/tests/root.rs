use rbitcoin_cli::*;
use rbitcoin_primitives::Network;
use serde_json::Value;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use std::net::TcpListener;

use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn exit_ok(c: ExitCode) -> bool {
    format!("{c:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn tmp_datadir() -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("rbitcoin-cli-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn spawn_rpc_mock(token: &str, result_json: &str) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let expect = format!("Bearer {token}");
    let result = result_json.to_string();
    let h = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut tmp = [0u8; 1024];
        while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&tmp[..n]);
        }
        let req = String::from_utf8_lossy(&raw);
        let authorized = req.lines().any(|line| {
            line.trim()
                .strip_prefix("Authorization:")
                .map(|r| r.trim().eq_ignore_ascii_case(&expect))
                .unwrap_or(false)
        });
        let body = if authorized {
            format!("{{\"jsonrpc\":\"1.0\",\"id\":\"1\",\"result\":{result},\"error\":null}}\n")
        } else {
            String::new()
        };
        if authorized {
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        } else {
            s.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"jsonrpc\"\r\nContent-Length: 13\r\nConnection: close\r\n\r\nUnauthorized\n").unwrap();
        }
    });
    (port, h)
}

#[test]
fn param_tokens_json_or_string() {
    assert_eq!(param_value("0"), serde_json::json!(0));
    assert_eq!(param_value("true"), serde_json::json!(true));
    assert_eq!(param_value("abc"), serde_json::json!("abc"));
}

#[test]
fn equals_form_datadir_is_accepted() {
    let dir = tmp_datadir();
    std::fs::write(dir.join("rpc.token"), "s3cret").unwrap();
    let (port, h) = spawn_rpc_mock("s3cret", "0");
    let flag = format!("--datadir={}", dir.display());
    let code = cli_main([
        "rbitcoin-cli",
        flag.as_str(),
        &format!("--rpc-url=http://127.0.0.1:{port}"),
        "getblockcount",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(exit_ok(code), "--foo=bar form must parse, got {code:?}");
    let _ = h.join();
}

#[test]
fn usage_describes_every_option() {
    let u = usage();
    for needle in [
        "--datadir PATH",
        "--network NET",
        "--rpc-url URL",
        "--rpc-token-file PATH",
        "-h, --help",
        "-V, --version",
    ] {
        assert!(u.contains(needle), "usage must list {needle}");
    }
    assert!(
        !u.contains("--rpcuser") && !u.contains("--rpcport") && !u.contains("--rpcconnect"),
        "dropped Core names must not be advertised: {u}"
    );
    assert!(
        u.contains("-V, --version") && u.contains("print version"),
        "version must have a description"
    );
}

#[test]
fn getblockcount_token_against_mock() {
    let dir = tmp_datadir();
    std::fs::write(dir.join("rpc.token"), "s3cret").unwrap();
    let (port, h) = spawn_rpc_mock("s3cret", "0");
    let code = cli_main([
        "rbitcoin-cli",
        "--datadir",
        dir.to_str().unwrap(),
        "--rpc-url",
        &format!("http://127.0.0.1:{port}"),
        "getblockcount",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        exit_ok(code),
        "token getblockcount must succeed against a live RPC mock, got {code:?}"
    );
    let _ = h.join();
}

#[test]
fn missing_auth_is_unauthorized() {
    let (port, _h) = spawn_rpc_mock("alice", "0");
    let dir = tmp_datadir();
    let code = cli_main([
        "rbitcoin-cli",
        "--datadir",
        dir.to_str().unwrap(),
        "--rpc-url",
        &format!("http://127.0.0.1:{port}"),
        "getblockcount",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!exit_ok(code));
}

#[test]
fn default_rpc_port_follows_network() {
    assert_eq!(Network::Mainnet.default_rpc_port(), 8332);
    assert_eq!(Network::Regtest.default_rpc_port(), 18443);
}

/// Answer one JSON-RPC request on `listener` with `"result": 7`.
#[cfg(unix)]
fn serve_one_rpc(listener: std::os::unix::net::UnixListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut tmp = [0u8; 1024];
        while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&tmp[..n]);
        }
        let body = "{\"jsonrpc\":\"1.0\",\"id\":\"1\",\"result\":7,\"error\":null}\n".to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(resp.as_bytes()).unwrap();
    })
}

#[cfg(unix)]
#[test]
fn unix_socket_needs_no_token() {
    use std::os::unix::net::UnixListener;
    let dir = tmp_datadir();
    let sock = dir.join("rpc.sock");
    let h = serve_one_rpc(UnixListener::bind(&sock).unwrap());
    let code = cli_main([
        "rbitcoin-cli",
        "--datadir",
        dir.to_str().unwrap(),
        "getblockcount",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        exit_ok(code),
        "unix getblockcount must succeed, got {code:?}"
    );
    let _ = h.join();
}

#[cfg(unix)]
#[test]
fn rpc_socket_reaches_a_socket_outside_the_datadir() {
    use std::os::unix::net::UnixListener;
    let dir = tmp_datadir();
    let sock = dir.join("rpc-elsewhere.sock");
    let h = serve_one_rpc(UnixListener::bind(&sock).unwrap());
    let code = cli_main([
        "rbitcoin-cli",
        "--datadir",
        dir.join("no-such-datadir").to_str().unwrap(),
        "--rpc-socket",
        sock.to_str().unwrap(),
        "getblockcount",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        exit_ok(code),
        "--rpc-socket getblockcount must succeed, got {code:?}"
    );
    let _ = h.join();
}
