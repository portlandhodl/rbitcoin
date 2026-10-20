use rbitcoin_log::PARAMS_MAX;
use rbitcoin_log::*;
use rbitcoin_log::{format_timestamp, trace};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn api_log_file_records_json_line() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = std::env::temp_dir().join(format!(
        "rbitcoin-api-log-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&path);
    init_api_log(&path).unwrap();
    api_call(
        "electrum",
        "127.0.0.1:1",
        "blockchain.tweaks.subscribe",
        "[0,1,false]",
        12,
        None,
    );
    api_call(
        "electrum",
        "127.0.0.1:1",
        "no.such",
        "[]",
        1,
        Some("unknown method: no.such"),
    );
    // 383 ASCII bytes, then a 3-byte character that straddles byte 384.
    // The operator file keeps the char-safe prefix and drops the tail.
    let mut straddling = "a".repeat(383);
    straddling.push('€');
    straddling.push_str("zzz");
    api_call("rpc", "-", "long.params", &straddling, 2, None);
    api_call("rpc", "-", "odd.params", "a\"b\\c\n\r\t\u{0001}", 3, None);
    close_api_log();
    let sealed = std::fs::metadata(&path).unwrap().len();
    api_call("electrum", "127.0.0.1:1", "server.ping", "[]", 1, None);
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        sealed,
        "close stops the operator file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "api log must be owner-only");
    }
    let body = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(body.contains("\"method\":\"blockchain.tweaks.subscribe\""));
    assert!(body.contains("\"params\":\"[0,1,false]\""));
    assert!(body.contains("\"wall_ms\":12"));
    assert!(!body.contains("join_ms"));
    assert!(body.contains("\"ok\":true"));
    assert!(body.contains("\"ok\":false"));
    assert!(body.contains("unknown method"));
    assert!(body.contains(&format!("\"params\":\"{}…\"", "a".repeat(383))));
    assert!(!body.contains('€'), "{body}");
    assert!(!body.contains("zzz"), "{body}");
    assert!(body.contains(r#""params":"a\"b\\c\n\r\t\u0001""#), "{body}");
}

#[test]
fn init_api_log_creates_a_missing_parent() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!(
        "rbitcoin-api-parent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let path = root.join("nested").join("api.jsonl");
    let _ = std::fs::remove_dir_all(&root);
    init_api_log(&path).unwrap();
    close_api_log();
    assert!(path.is_file(), "operator path must exist");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn api_stderr_line_is_trace() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    rbitcoin_log::capture_logs(true);
    api_call("electrum", "127.0.0.1:1", "server.ping", "[]", 3, None);
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    let (level, msg) = logs.last().expect("api_call stderr");
    assert_eq!(*level, rbitcoin_log::Level::Trace, "{msg}");
    assert!(msg.contains("api: electrum"), "{msg}");
    assert!(msg.contains("server.ping"), "{msg}");
}

#[test]
fn api_call_redacts_scan_secrets_and_ext_privkeys() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let scan = "ab".repeat(32);
    let xprv = format!("xprv{}", "1".repeat(40));
    let tprv = format!("tprv{}", "A".repeat(20));
    let path = std::env::temp_dir().join(format!(
        "rbitcoin-api-redact-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&path);
    init_api_log(&path).unwrap();
    rbitcoin_log::capture_logs(true);
    api_call(
        "electrum",
        "127.0.0.1:1",
        "blockchain.silentpayments.unsubscribe",
        &format!("[\"{scan}\",\"02ff\",1]"),
        4,
        None,
    );
    api_call(
        "rpc",
        "-",
        "gettxout",
        &format!("[\"start\",[\"{xprv}\",\"{tprv}\"]]"),
        5,
        None,
    );
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    close_api_log();
    let body = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let trace = logs
        .iter()
        .map(|(_, msg)| msg.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let all = format!("{body}\n{trace}");
    assert!(!all.contains(&scan), "{all}");
    assert!(!all.contains(&xprv), "{all}");
    assert!(!all.contains(&tprv), "{all}");
    assert!(all.contains("<redacted>"), "{all}");
}

#[test]
fn logged_prefix_redacts_a_straddling_secret_and_drops_one_past_the_cap() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key = format!("xprv{}", "1".repeat(40));
    let hex_secret = "ab".repeat(32);
    let straddle = format!("{}{key}{}", "a".repeat(PARAMS_MAX - 10), "b".repeat(40));
    let far = format!("{}{key}", "c".repeat(PARAMS_MAX + 80));
    let hex_params = format!("{}\"{hex_secret}\"tail", "d".repeat(PARAMS_MAX - 8));
    rbitcoin_log::capture_logs(true);
    api_call("rpc", "-", "submitblock", &straddle, 1, None);
    api_call("rpc", "-", "submitblock", &far, 1, None);
    api_call(
        "electrum",
        "127.0.0.1:1",
        "blockchain.silentpayments.unsubscribe",
        &hex_params,
        1,
        None,
    );
    let logs = rbitcoin_log::take_logs();
    rbitcoin_log::capture_logs(false);
    let trace = logs
        .iter()
        .map(|(_, msg)| msg.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!trace.contains(&key), "{trace}");
    assert!(!trace.contains(&hex_secret), "{trace}");
    assert!(trace.contains("<redacted>"), "{trace}");
    let far_mark = "c".repeat(40);
    let far_line = logs
        .iter()
        .map(|(_, msg)| msg.as_str())
        .find(|msg| msg.contains(&far_mark))
        .expect("far submitblock line");
    assert!(
        !far_line.contains("<redacted>"),
        "a secret past the cap is not part of the logged prefix: {far_line}"
    );
    assert!(!far_line.contains("xprv"), "{far_line}");
}
