//! Optional JSONL API call log (`--api-log PATH`).
//!
//! Always emits a TRACE `api:` line. When [`init_api_log`] has been called,
//! also appends one JSON object per call so the operator can `tail -f` without
//! turning the main log up.

use crate::{format_timestamp, trace};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

#[doc(hidden)]
pub const PARAMS_MAX: usize = 384;

#[doc(hidden)]
pub static API_LOG: Mutex<Option<File>> = Mutex::new(None);

/// Open (append) `path` as the process API call log.
pub fn init_api_log(path: impl AsRef<Path>) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let mut g = API_LOG.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(f);
    Ok(())
}

/// Stop writing the API call file (tests).
pub fn close_api_log() {
    let mut g = API_LOG.lock().unwrap_or_else(|e| e.into_inner());
    *g = None;
}

/// Truncate a params blob for the log (UTF-8 safe).
#[doc(hidden)]
pub fn compact_params(params: &str) -> String {
    if params.len() <= PARAMS_MAX {
        return params.to_string();
    }
    format!("{}…", &params[..char_floor(params, PARAMS_MAX)])
}

/// Record one Electrum / Esplora / RPC call.
///
/// Extended private keys and silent-payment scan secrets that overlap the
/// logged prefix are stripped here, then the params blob is compacted.
/// A large body with no secret in that prefix is truncated without copying
/// the tail. `err` is `None` on success.
pub fn api_call(
    surface: &str,
    peer: &str,
    method: &str,
    params: &str,
    wall_ms: u64,
    err: Option<&str>,
) {
    let params = logged_params(method, params);
    match err {
        None => trace!("api: {surface} peer={peer} {method} {params} wall_ms={wall_ms} ok"),
        Some(e) => trace!("api: {surface} peer={peer} {method} {params} wall_ms={wall_ms} err={e}"),
    }

    let mut g = API_LOG.lock().unwrap_or_else(|e| e.into_inner());
    let Some(file) = g.as_mut() else {
        return;
    };
    let ts = format_timestamp(SystemTime::now());
    let ok = err.is_none();
    let err_json = match err {
        None => "null".to_string(),
        Some(e) => format!("\"{}\"", json_escape(e)),
    };
    let line = format!(
        "{{\"ts\":\"{ts}\",\"surface\":\"{}\",\"peer\":\"{}\",\"method\":\"{}\",\"params\":\"{}\",\"wall_ms\":{wall_ms},\"ok\":{ok},\"err\":{err_json}}}\n",
        json_escape(surface),
        json_escape(peer),
        json_escape(method),
        json_escape(&params),
    );
    let _ = file.write_all(line.as_bytes());
    let _ = file.flush();
}

#[doc(hidden)]
pub fn logged_params(method: &str, params: &str) -> String {
    let cap = char_floor(params, PARAMS_MAX);
    let Some(end) = secret_cover_end(method, params, cap) else {
        return compact_params(params);
    };
    let mut redacted = redact_ext_privkeys(&params[..end]);
    if method.contains("silentpayment") {
        redacted = redact_quoted_hex64(&redacted);
    }
    compact_params(&redacted)
}

#[doc(hidden)]
pub fn char_floor(s: &str, max: usize) -> usize {
    s.floor_char_boundary(max.min(s.len()))
}

/// Exclusive end of the slice to redact, covering each secret that starts
/// inside the logged prefix plus `PARAMS_MAX` bytes after it. `None` when
/// that prefix has no secret, so the caller truncates without a full copy.
#[doc(hidden)]
pub fn secret_cover_end(method: &str, params: &str, cap: usize) -> Option<usize> {
    let b = params.as_bytes();
    let scan_hex = method.contains("silentpayment");
    let mut cover = None;
    let mut i = 0;
    while i < cap && i < b.len() {
        if i + 4 <= b.len() {
            let tag = &b[i..i + 4];
            if matches!(tag, b"xprv" | b"tprv" | b"yprv" | b"zprv") {
                let mut j = i + 4;
                while j < b.len() && is_base58(b[j]) {
                    j += 1;
                }
                if j > i + 4 {
                    let need = j.saturating_add(PARAMS_MAX).min(b.len());
                    cover = Some(cover.unwrap_or(0).max(need));
                    i = j;
                    continue;
                }
            }
        }
        if scan_hex
            && b[i] == b'"'
            && i + 66 <= b.len()
            && b[i + 65] == b'"'
            && b[i + 1..i + 65].iter().copied().all(is_hex)
        {
            let need = (i + 66).saturating_add(PARAMS_MAX).min(b.len());
            cover = Some(cover.unwrap_or(0).max(need));
            i += 66;
            continue;
        }
        i += 1;
    }
    cover
}

#[doc(hidden)]
pub fn is_base58(b: u8) -> bool {
    matches!(
        b,
        b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z'
    )
}

#[doc(hidden)]
pub fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// `xprv` / `tprv` / `yprv` / `zprv` plus the following base58 key material.
#[doc(hidden)]
pub fn redact_ext_privkeys(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if i + 4 <= b.len() {
            let tag = &b[i..i + 4];
            if matches!(tag, b"xprv" | b"tprv" | b"yprv" | b"zprv") {
                let mut j = i + 4;
                while j < b.len() && is_base58(b[j]) {
                    j += 1;
                }
                if j > i + 4 {
                    out.push_str("<redacted>");
                    i = j;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// A quoted 64-hex string is a silent-payment scan secret on that method.
#[doc(hidden)]
pub fn redact_quoted_hex64(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"'
            && i + 65 < b.len()
            && b[i + 65] == b'"'
            && b[i + 1..i + 65].iter().copied().all(is_hex)
        {
            out.push_str("\"<redacted>\"");
            i += 66;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[doc(hidden)]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
