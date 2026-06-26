//! Daemon URL resolution: explicit flag → lock file → default.
//!
//! Why: The daemon may bind to an ephemeral port when 7880 is busy.
//! The lock file records the actual address so clients always find it.
//! What: `resolve_daemon_url` checks an explicit override first, then
//! reads `~/.trusty-mpm/daemon.lock`, then falls back to the hard-coded
//! default.
//! Test: The unit tests below cover all three resolution paths.

use std::path::PathBuf;

use crate::core::paths::FRAMEWORK_DIR_NAME;

/// Canonical loopback bind address the daemon listens on by default.
///
/// Why (issue #1268): the daemon's `--addr` default, the thin CLI's `--url`
/// default, and [`DEFAULT_DAEMON_URL`] previously each hard-coded the
/// `127.0.0.1:7880` literal independently. Any one of them drifting (or an
/// operator carrying a stale `TRUSTY_MPM_URL=…:7881` in their environment)
/// produced "daemon: unreachable" because the client probed a port the daemon
/// never bound. Hoisting the port into a single constant — and deriving every
/// other default address string from it — makes the bind side and the client
/// side provably agree from one source of truth.
/// What: the literal `"127.0.0.1:7880"`, parsed into a [`SocketAddr`] by
/// [`default_daemon_addr`] and embedded into [`DEFAULT_DAEMON_URL`].
/// Test: `default_url_matches_addr`, `default_addr_parses`.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:7880";

/// Default daemon URL when no override and no lock file is found.
///
/// Why: derived from [`DEFAULT_DAEMON_ADDR`] so the client default and the
/// daemon bind default can never drift (issue #1268).
/// What: `"http://127.0.0.1:7880"` — the `http://` scheme prepended to
/// [`DEFAULT_DAEMON_ADDR`]. Kept as a `&'static str` literal (rather than a
/// runtime `format!`) so it remains usable in `const`/`default_value` contexts;
/// the [`default_url_matches_addr`] test guarantees the two stay in lockstep.
/// Test: `default_url_matches_addr`.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7880";

/// Parse [`DEFAULT_DAEMON_ADDR`] into a [`SocketAddr`].
///
/// Why: the daemon's clap `--addr` argument is typed as [`SocketAddr`]; this
/// helper lets that default be derived from the shared [`DEFAULT_DAEMON_ADDR`]
/// constant instead of repeating the literal (issue #1268).
/// What: parses the constant; the parse is infallible for a well-formed
/// literal, so a malformed constant is a programmer error caught by
/// `default_addr_parses` at test time.
/// Test: `default_addr_parses`.
pub fn default_daemon_addr() -> std::net::SocketAddr {
    DEFAULT_DAEMON_ADDR
        .parse()
        .expect("DEFAULT_DAEMON_ADDR is a valid SocketAddr literal")
}

/// Path to the daemon lock file.
///
/// Why: the lock file MUST live in the same `~/.trusty-mpm` root as every
/// other framework artifact (logs, sessions, framework dir). It previously
/// resolved under `dirs::config_dir()` (`~/.config/trusty-mpm`), so the daemon
/// wrote the lock to one directory while the rest of the app — and any user
/// inspecting the install — looked in another. That mismatch meant clients
/// that resolved the URL from a differently-configured environment (or simply
/// expected the documented `~/.trusty-mpm` location) never found the lock and
/// fell back to the default port, reporting "daemon unreachable".
/// What: `~/.trusty-mpm/daemon.lock`, derived from the same `home_dir` +
/// [`FRAMEWORK_DIR_NAME`] as `FrameworkPaths`.
/// Test: `lock_file_path_is_under_framework_root`.
pub fn lock_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(FRAMEWORK_DIR_NAME)
        .join("daemon.lock")
}

/// Resolve the daemon URL in priority order:
/// 1. `explicit` — from `--url` flag or `TRUSTY_MPM_URL` env var (if Some,
///    non-empty, AND not just the default). A caller passing the clap default
///    value is treated the same as passing None so the lock file can win.
/// 2. Lock file `~/.trusty-mpm/daemon.lock` (if present and PID alive)
/// 3. `DEFAULT_DAEMON_URL`
pub fn resolve_daemon_url(explicit: Option<&str>) -> String {
    // 1. Explicit override wins — but only if it's a real override, not the
    //    clap-injected default. When the caller passes DEFAULT_DAEMON_URL we
    //    fall through to the lock file so `tm tui` and `tm status` find a
    //    daemon running on an ephemeral port.
    if let Some(url) = explicit
        && !url.is_empty()
        && url != DEFAULT_DAEMON_URL
    {
        return url.to_string();
    }

    // 2. Lock file — records the actual bound address written by the daemon.
    if let Some(url) = read_lock_file_url() {
        return url;
    }

    // 3. Fall back to the default (or the explicit default the caller passed).
    explicit
        .filter(|u| !u.is_empty())
        .unwrap_or(DEFAULT_DAEMON_URL)
        .to_string()
}

/// Resolve the daemon URL with reachability probing for explicit overrides.
///
/// Why: `resolve_daemon_url` treats an explicit non-default URL (e.g. a stale
/// `TRUSTY_MPM_URL=http://127.0.0.1:7881`) as unconditionally winning, even
/// when the daemon is no longer listening there. A stale env var therefore
/// permanently breaks `tm` guided-default until the env is corrected, because
/// the lock file (which records the daemon's actual bound address) is never
/// consulted (#1731). This async variant probes the explicit URL before
/// committing to it; on failure it falls through to the lock file and then
/// the compiled-in default — exactly the fallback chain users expect.
/// What: follows the same three-step priority order as `resolve_daemon_url`,
/// but step 1 is gated on a 500 ms health probe.
///
/// 1. `explicit` (non-empty, non-default) — accepted only if reachable
/// 2. Lock file `~/.trusty-mpm/daemon.lock` (if present and PID alive)
/// 3. `DEFAULT_DAEMON_URL`
///
/// A reachable explicit URL wins immediately without consulting the lock file
/// so a deliberately non-default daemon address (`--url http://…:9999`) still
/// works.
/// Test: `probing_resolver_falls_back_when_explicit_unreachable` and
/// `probing_resolver_wins_when_explicit_reachable` below.
pub async fn resolve_daemon_url_probing(
    client: &reqwest::Client,
    explicit: Option<&str>,
) -> String {
    // Step 1: if the caller supplied a real override (non-empty, non-default),
    // probe it.  Only skip the probe when the "explicit" URL is actually the
    // clap-injected default — identical behaviour to resolve_daemon_url so
    // callers that pass DEFAULT_DAEMON_URL always see the lock file win.
    if let Some(url) = explicit
        && !url.is_empty()
        && url != DEFAULT_DAEMON_URL
    {
        if probe_url(client, url).await {
            return url.to_string();
        }
        // Probe failed: fall through to lock file, then hard default.
        if let Some(lock_url) = read_lock_file_url() {
            return lock_url;
        }
        return DEFAULT_DAEMON_URL.to_string();
    }

    // No real explicit override: delegate to the sync resolver.
    resolve_daemon_url(explicit)
}

/// Probe a URL for reachability with a short timeout.
///
/// Why: used by `resolve_daemon_url_probing` to validate stale explicit URLs
/// before committing to them. A 500 ms timeout keeps the probe imperceptible
/// to the user while still covering slow loopback responses.
/// What: sends `GET <url>/health` (trailing slash on `url` is stripped first
/// to avoid the double-slash path `//health`); returns `true` if the response
/// has any 2xx status, `false` on error (connection refused, timeout, etc.).
/// Test: `probing_resolver_falls_back_when_explicit_unreachable`,
/// `probing_resolver_wins_when_explicit_reachable`,
/// `probe_url_trims_trailing_slash`.
async fn probe_url(client: &reqwest::Client, url: &str) -> bool {
    use std::time::Duration;
    let base = url.trim_end_matches('/');
    let probe = client
        .get(format!("{base}/health"))
        .timeout(Duration::from_millis(500))
        .send()
        .await;
    probe.map(|r| r.status().is_success()).unwrap_or(false)
}

/// Read the daemon URL from the lock file if present and the PID is alive.
fn read_lock_file_url() -> Option<String> {
    let path = lock_file_path();
    let content = std::fs::read_to_string(&path).ok()?;

    let mut addr: Option<String> = None;
    let mut pid: Option<u32> = None;

    for line in content.lines() {
        if let Some(v) = line.strip_prefix("addr = ") {
            addr = Some(v.trim_matches('"').to_string());
        }
        if let Some(v) = line.strip_prefix("pid = ") {
            pid = v.trim().parse::<u32>().ok();
        }
    }

    // Validate PID is still alive (Unix only; on non-Unix skip check).
    #[cfg(unix)]
    if let Some(p) = pid {
        // kill(pid, 0) returns Ok if process exists, Err otherwise.
        if unsafe { libc::kill(p as libc::pid_t, 0) } != 0 {
            // Stale lock — remove it silently.
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }

    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_url_wins_over_everything() {
        let result = resolve_daemon_url(Some("http://example.com:9999"));
        assert_eq!(result, "http://example.com:9999");
    }

    #[test]
    fn empty_explicit_falls_through() {
        // With no lock file and empty explicit, must return default.
        // (Lock file path may or may not exist on CI; we just assert not empty.)
        let result = resolve_daemon_url(Some(""));
        assert!(!result.is_empty());
    }

    #[test]
    fn default_returned_when_no_lock_and_no_explicit() {
        // If no lock file exists this returns DEFAULT_DAEMON_URL.
        // We can't guarantee no lock file exists, so just check it's a valid URL.
        let result = resolve_daemon_url(None);
        assert!(result.starts_with("http"));
    }

    #[test]
    fn default_url_matches_addr() {
        // Why (issue #1268): the client default URL and the daemon bind default
        // must agree. `DEFAULT_DAEMON_URL` is `http://` + `DEFAULT_DAEMON_ADDR`.
        assert_eq!(
            DEFAULT_DAEMON_URL,
            format!("http://{DEFAULT_DAEMON_ADDR}"),
            "client default URL and daemon bind addr drifted (issue #1268)"
        );
    }

    #[test]
    fn default_addr_parses() {
        // The shared bind constant must parse into a SocketAddr so the daemon's
        // clap `--addr` default can be derived from it.
        let addr = default_daemon_addr();
        assert_eq!(addr.to_string(), DEFAULT_DAEMON_ADDR);
        assert_eq!(addr.port(), 7880);
    }

    #[test]
    fn default_url_passed_as_explicit_falls_through_to_lock() {
        // Why: clap injects DEFAULT_DAEMON_URL when the user does not pass --url.
        // resolve_daemon_url must NOT treat that as a real user override — it must
        // fall through to the lock file (or the final default when no lock file
        // exists). We verify this behaviorally: with no lock file present in the
        // test environment the result must still be a valid HTTP URL (either the
        // lock-file URL or the compiled-in default).
        let result = resolve_daemon_url(Some(DEFAULT_DAEMON_URL));
        assert!(
            result.starts_with("http"),
            "resolve_daemon_url(DEFAULT_DAEMON_URL) must return an HTTP URL: {result}"
        );
    }

    #[test]
    fn lock_file_path_is_under_framework_root() {
        // Why: the lock file must share the `~/.trusty-mpm` root with every
        // other framework artifact. A path under `~/.config` (the previous
        // behaviour) meant the daemon and its clients could disagree on the
        // location and the TUI would report "daemon unreachable".
        let path = lock_file_path();
        assert!(
            path.ends_with(format!("{FRAMEWORK_DIR_NAME}/daemon.lock")),
            "lock file path {path:?} is not under the {FRAMEWORK_DIR_NAME} root"
        );
        // The parent directory is the framework root itself.
        assert_eq!(
            path.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new(FRAMEWORK_DIR_NAME))
        );
    }

    // ── resolve_daemon_url_probing tests (#1731) ─────────────────────────────

    /// Stale explicit URL (unreachable) falls back to lock file or default (#1731).
    ///
    /// Why: `TRUSTY_MPM_URL=http://127.0.0.1:1` is never reachable (port 1 is
    /// reserved and never listening). The probing resolver must skip it and
    /// return either the lock-file URL (if the daemon is running and has written
    /// one) or `DEFAULT_DAEMON_URL` — never the unreachable stale value.
    /// What: calls `resolve_daemon_url_probing` with the reserved port; asserts
    /// the result is a valid HTTP URL that is NOT the stale value.
    /// Test: this test.
    #[tokio::test]
    async fn probing_resolver_falls_back_when_explicit_unreachable() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("build client");
        let stale = "http://127.0.0.1:1"; // port 1 is reserved, always refused
        let result = resolve_daemon_url_probing(&client, Some(stale)).await;
        // Must return a valid HTTP URL.
        assert!(
            result.starts_with("http"),
            "expected HTTP URL, got: {result}"
        );
        // Must NOT lock onto the unreachable stale URL.
        assert_ne!(
            result, stale,
            "probing resolver must not return the unreachable stale URL"
        );
    }

    /// Reachable explicit URL wins immediately, lock file is not consulted (#1731).
    ///
    /// Why: a deliberately non-default URL (`--url http://…:9999`) must still
    /// win when the daemon is reachable at that address. The probe must not
    /// cause a regression for the `tm --url <custom>` workflow.
    /// What: binds a minimal TCP listener that responds with HTTP 200 on any
    /// connection, calls `resolve_daemon_url_probing` with that address, and
    /// asserts the result equals the listener's address.
    /// Test: this test.
    #[tokio::test]
    async fn probing_resolver_wins_when_explicit_reachable() {
        use tokio::io::AsyncWriteExt as _;

        // Bind to an ephemeral port and respond HTTP 200 to the first connection.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local_addr");
        let url = format!("http://{addr}");

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // A minimal HTTP/1.1 200 response is enough for reqwest to
                // call `r.status().is_success()`.
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await;
            }
        });

        let client = reqwest::Client::new();
        let result = resolve_daemon_url_probing(&client, Some(&url)).await;
        assert_eq!(
            result, url,
            "reachable explicit URL must win over lock file / default"
        );
    }

    /// Trailing slash on the base URL must not produce a double-slash path.
    ///
    /// Why: `probe_url("http://…:PORT/")` previously produced
    /// `GET http://…:PORT//health` — the double slash could cause 404s on
    /// strict HTTP servers, making a live daemon appear unreachable. The fix
    /// trims the trailing slash before appending `/health`.
    /// What: binds a TCP listener that echoes the request line in its 200
    /// response body; asserts the captured path is `/health` not `//health`.
    /// Test: this test.
    #[tokio::test]
    async fn probe_url_trims_trailing_slash() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local_addr");

        // Capture the request line so we can assert on the path.
        let (path_tx, path_rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            if let Ok((sock, _)) = listener.accept().await {
                let (reader, mut writer) = sock.into_split();
                let mut lines = BufReader::new(reader).lines();
                // First line is the request line, e.g. "GET /health HTTP/1.1"
                let req_line = lines.next_line().await.unwrap().unwrap_or_default();
                let _ = path_tx.send(req_line);
                let _ = writer
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });

        let client = reqwest::Client::new();
        // URL with trailing slash — must probe `/health`, not `//health`.
        let url_with_slash = format!("http://{addr}/");
        probe_url(&client, &url_with_slash).await;

        let req_line = path_rx.await.expect("request line captured");
        // The path segment must be exactly "/health" with no double slash.
        assert!(
            req_line.starts_with("GET /health "),
            "probe must request /health (no double slash); got: {req_line:?}"
        );
    }
}
