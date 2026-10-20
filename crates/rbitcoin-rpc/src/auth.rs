//! RPC authentication: Bearer token plus optional Core cookie credentials.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Bearer token accepted by the RPC server.
#[derive(Clone, PartialEq, Eq)]
pub struct RpcAuth {
    pub token: String,
}

impl std::fmt::Debug for RpcAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RpcAuth(<redacted>)")
    }
}

impl RpcAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    pub fn matches_token(&self, token: &str) -> bool {
        ct_eq(self.token.as_bytes(), token.as_bytes())
    }
}

/// Core-format `username:password` cookie accepted by an opted-in TCP listener.
#[derive(Clone, PartialEq, Eq)]
pub struct RpcCookie {
    pub credentials: String,
}

impl RpcCookie {
    pub fn matches_credentials(&self, credentials: &str) -> bool {
        ct_eq(self.credentials.as_bytes(), credentials.as_bytes())
    }
}

/// Compare two byte strings without returning on the first mismatch.
#[doc(hidden)]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= usize::from(*x ^ *y);
    }
    diff == 0
}

pub fn default_token_path(datadir: &Path) -> PathBuf {
    datadir.join("rpc.token")
}

pub fn default_socket_path(datadir: &Path) -> PathBuf {
    datadir.join("rpc.sock")
}

/// Widest mode an operator declared for `--rpc-cookie-file`.
///
/// The node never creates or chmods the cookie. `owner` is mode 0600.
/// `group` is at most 0640 (group read). `all` is at most 0644 (world read).
/// The check is Unix mode bits. Windows ignores the flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RpcCookiePerms {
    #[default]
    Owner,
    Group,
    All,
}

impl RpcCookiePerms {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "owner" => Ok(Self::Owner),
            "group" => Ok(Self::Group),
            "all" => Ok(Self::All),
            _ => Err(format!(
                "rpc_cookie_perms must be owner, group, or all (got {raw:?})"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Group => "group",
            Self::All => "all",
        }
    }

    /// Unix permission bits no wider than this declaration.
    #[cfg(unix)]
    #[doc(hidden)]
    pub fn allows_unix_mode(self, mode: u32) -> bool {
        let mode = mode & 0o777;
        match self {
            Self::Owner => mode & 0o077 == 0,
            // Group write/exec and every world bit. 0640 stays.
            Self::Group => mode & 0o037 == 0,
            // Group and world write/exec. 0644 stays.
            Self::All => mode & 0o033 == 0,
        }
    }
}

/// Read a Core-format RPC cookie. The configured file is never created or logged.
///
/// The file must be exactly `username:password` with no line ending, as Core
/// writes it. bitcoin-cli reads the cookie with `getline`, but mempool sends the
/// raw file bytes as Basic credentials, so a trailing newline would make the two
/// clients disagree. Refusing it here turns mempool's endless 401 into a startup
/// error.
pub fn read_cookie_file(path: &Path, perms: RpcCookiePerms) -> Result<RpcCookie, String> {
    // Mode bits exist only on Unix. The argument stays so every caller passes it.
    #[cfg(not(unix))]
    let _ = perms;
    let mut file =
        fs::File::open(path).map_err(|e| format!("read RPC cookie {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| format!("stat RPC cookie {}: {e}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if !perms.allows_unix_mode(mode) {
            return Err(format!(
                "RPC cookie {}: mode {mode:o} is wider than --rpc-cookie-perms {} \
                 (owner is 0600, group is at most 0640, all is at most 0644)",
                path.display(),
                perms.as_str()
            ));
        }
    }
    let mut credentials = String::new();
    file.read_to_string(&mut credentials)
        .map_err(|e| format!("read RPC cookie {}: {e}", path.display()))?;
    if credentials.ends_with(['\n', '\r']) {
        return Err(format!(
            "RPC cookie {}: ends with a line ending; write it without one (Core format, e.g. printf not echo)",
            path.display()
        ));
    }
    if credentials.is_empty()
        || credentials.contains(['\n', '\r'])
        || !matches!(credentials.split_once(':'), Some((user, password)) if !user.is_empty() && !password.is_empty())
    {
        return Err(format!(
            "RPC cookie {}: expected username:password",
            path.display()
        ));
    }
    Ok(RpcCookie { credentials })
}

/// Read an existing token or write a new CSPRNG token (mode 0600).
pub fn resolve_rpc_auth(
    datadir: &Path,
    token_path: Option<&Path>,
) -> Result<(RpcAuth, PathBuf), String> {
    let path = token_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_token_path(datadir));
    if path.is_file() {
        let auth = read_token_file(&path)?;
        return Ok((auth, path));
    }
    let auth = write_token_file(&path)?;
    Ok((auth, path))
}

pub fn read_token_file(path: &Path) -> Result<RpcAuth, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("read token {}: {e}", path.display()))?;
    // The node creates this file mode 0600. An existing token must stay
    // owner-only. Windows has no mode bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| format!("stat token {}: {e}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "RPC token {}: mode {mode:o} is wider than 0600. Delete the file and start again so the node mints a new token. chmod 0600 would keep this secret, and it may already have been readable",
                path.display()
            ));
        }
    }
    let mut line = String::new();
    file.read_to_string(&mut line)
        .map_err(|e| format!("read token {}: {e}", path.display()))?;
    let token = line.trim();
    if token.is_empty() {
        return Err(format!("token file {}: empty", path.display()));
    }
    Ok(RpcAuth::new(token))
}

pub fn write_token_file(path: &Path) -> Result<RpcAuth, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("token parent: {e}"))?;
    }
    let auth = RpcAuth::new(random_token());
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("token create: {e}"))?;
    f.write_all(auth.token.as_bytes())
        .map_err(|e| format!("token write: {e}"))?;
    f.sync_all().map_err(|e| format!("token sync: {e}"))?;
    Ok(auth)
}

/// Parse `Authorization: Bearer …`.
pub fn parse_bearer_auth(header: &str) -> Option<&str> {
    let header = header.trim();
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Decode `Authorization: Basic …` into Core cookie credentials.
pub fn parse_basic_auth(header: &str) -> Option<String> {
    use base64::Engine;

    let encoded = header
        .trim()
        .strip_prefix("Basic ")
        .or_else(|| header.trim().strip_prefix("basic "))?
        .trim();
    if encoded.is_empty() {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    String::from_utf8(decoded).ok()
}

#[doc(hidden)]
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("CSPRNG for RPC token");
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
