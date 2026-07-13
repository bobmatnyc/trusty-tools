//! Daemon URL resolution: explicit flag → lock file → default.
//!
//! Why: The daemon may bind to an ephemeral port when 7880 is busy.
//! The lock file records the actual address so clients always find it.
//! What: `resolve_daemon_url` checks an explicit override first, then
//! reads `~/.trusty-mpm/daemon.lock`, then falls back to the hard-coded
//! default. `resolve_daemon_url_via_gateway` additionally tries the
//! trusty-console gateway first so all traffic can flow through the
//! unified web UI when it is running.
//!
//! Issue #2487: every resolver here takes `explicit: Option<&str>` and MUST
//! be fed by a caller that only produces `Some` when the operator actually
//! supplied `--url` / `TRUSTY_MPM_URL` — never by a clap `default_value`
//! string. Prior to #2487 the resolvers tried to reconstruct that distinction
//! themselves by comparing the value against [`DEFAULT_DAEMON_URL`], which
//! silently broke whenever an operator's real override happened to equal the
//! default (`TRUSTY_MPM_URL=http://127.0.0.1:7880`, itself the documented
//! default) — the value looked identical to "nothing was passed", so
//! `resolve_daemon_url_via_gateway` fell through to the trusty-console
//! gateway probe and rerouted traffic through the console proxy (port 7788)
//! instead of the daemon (port 7880). The fix: every CLI-facing `url` field
//! is `Option<String>` with no `default_value` (clap only fills `Some` on a
//! real flag/env hit), and [`explicit_url_from_env`] is the single shared
//! helper for the few call sites that read `TRUSTY_MPM_URL` before clap has
//! even parsed (`tm projects` bare-TUI interception). No resolver in this
//! module compares `explicit` against `DEFAULT_DAEMON_URL` anymore.
//! Test: The unit tests below cover all resolution paths, including the
//! gateway probe, explicit-override bypass (including a value equal to the
//! default), and direct fallback.

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

/// Default trusty-console HTTP address (host:port) used when the console's
/// discovery file is absent or unreadable.
///
/// Why: trusty-console defaults to 127.0.0.1:7788; mirroring that here lets
/// the gateway resolver fall back to the expected address without importing
/// trusty-console (which would create a circular dependency between crates).
/// What: the literal `"127.0.0.1:7788"`, matching `trusty_console::DEFAULT_HTTP`.
/// Test: `resolve_via_gateway_falls_back_to_direct_on_probe_failure`.
pub const DEFAULT_CONSOLE_ADDR: &str = "127.0.0.1:7788";

/// Path at which the trusty-console proxies the MPM daemon.
///
/// Why: a single constant prevents the gateway path `/api/mpm` from being
/// hard-coded in both the resolver and any caller that needs to detect whether
/// a resolved URL is routing via the gateway (e.g. `tm health` display).
/// What: the literal `"/api/mpm"` — appended to the console base URL to form
/// the gateway base, e.g. `http://127.0.0.1:7788/api/mpm`.
/// Test: `resolve_via_gateway_uses_gateway_when_probe_succeeds`.
pub const GATEWAY_PATH: &str = "/api/mpm";

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

/// Read `TRUSTY_MPM_URL` directly from the process environment.
///
/// Why (#2487): clap's `env = "TRUSTY_MPM_URL"` attribute on an
/// `Option<String>` field is the normal way a `tm` subcommand picks up the
/// override, but a couple of call sites resolve the daemon URL BEFORE
/// `Cli::try_parse()` runs — namely `tm projects` bare-TUI interception
/// (`commands::projects::launch_bare_tui`), which must fire ahead of clap so
/// the exact clap "requires a subcommand" usage error is preserved for a
/// non-interactive bare invocation (see that function's doc). Those sites
/// need the identical "did the operator actually set this" semantics as
/// every clap-parsed field, so they call this helper instead of
/// hand-rolling their own `std::env::var` read (and risking a different
/// empty-string or trim rule than the rest of the resolvers).
/// What: `std::env::var("TRUSTY_MPM_URL")`, trimmed, with an empty result
/// treated the same as "unset" (`None`) — matching every `resolve_daemon_url*`
/// empty-string check.
/// Test: `explicit_url_from_env_reads_set_var`,
/// `explicit_url_from_env_treats_empty_as_unset`.
pub fn explicit_url_from_env() -> Option<String> {
    std::env::var("TRUSTY_MPM_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Resolve the daemon URL in priority order:
/// 1. `explicit` — from `--url` flag or `TRUSTY_MPM_URL` env var (`Some`,
///    non-empty). Every CLI-facing `url` field is `Option<String>` with NO
///    `default_value` (issue #2487) — clap only produces `Some` when the flag
///    or env var was actually supplied, so `Some` here always means the
///    operator made a real, deliberate choice, even when that choice happens
///    to equal [`DEFAULT_DAEMON_URL`] verbatim. There is no longer a
///    string-equality heuristic trying to guess intent from the value.
/// 2. Lock file `~/.trusty-mpm/daemon.lock` (if present and PID alive)
/// 3. `DEFAULT_DAEMON_URL`
pub fn resolve_daemon_url(explicit: Option<&str>) -> String {
    // 1. Explicit override always wins when present and non-empty — the
    //    caller (a CLI field or the shared env-read helper) already collapsed
    //    "not supplied" to `None`, so there is nothing left to disambiguate.
    if let Some(url) = explicit
        && !url.is_empty()
    {
        return url.to_string();
    }

    // 2. Lock file — records the actual bound address written by the daemon.
    if let Some(url) = read_lock_file_url() {
        return url;
    }

    // 3. Fall back to the compiled-in default.
    DEFAULT_DAEMON_URL.to_string()
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
/// 1. `explicit` (non-empty) — accepted only if reachable
/// 2. Lock file `~/.trusty-mpm/daemon.lock` (if present and PID alive)
/// 3. `DEFAULT_DAEMON_URL`
///
/// A reachable explicit URL wins immediately without consulting the lock file
/// so a deliberately non-default daemon address (`--url http://…:9999`) still
/// works. As of #2487, "explicit" is `Some` if and only if the operator
/// actually supplied `--url` or `TRUSTY_MPM_URL` — there is no equality check
/// against [`DEFAULT_DAEMON_URL`] to second-guess that.
/// Test: `probing_resolver_falls_back_when_explicit_unreachable` and
/// `probing_resolver_wins_when_explicit_reachable` below.
pub async fn resolve_daemon_url_probing(
    client: &reqwest::Client,
    explicit: Option<&str>,
) -> String {
    // Step 1: if the caller supplied a real override (non-empty), probe it.
    if let Some(url) = explicit
        && !url.is_empty()
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

    // No explicit override: delegate to the sync resolver (lock file → default).
    resolve_daemon_url(explicit)
}

/// Resolve the daemon URL routing through the trusty-console gateway when
/// possible, with seamless fallback to the direct daemon when the console is
/// not running.
///
/// Why: when the trusty-console is up it proxies `ANY /api/mpm/{path}` to the
/// MPM daemon, so all `tm` traffic can flow through the unified web UI — with
/// future benefits like audit logging and auth. Direct fallback (lock file →
/// default) ensures `tm` still works when the console is not running, with no
/// operator action required. The gateway-first approach is gated on a live
/// probe so a stale console discovery file never breaks the CLI.
///
/// Precedence:
///   a) Explicit user override (non-empty `--url` / `TRUSTY_MPM_URL`) → use it
///      directly; the operator chose a specific address, so the gateway is
///      bypassed entirely. As of #2487 this is a plain `Option::is_some`
///      check — a value that happens to equal [`DEFAULT_DAEMON_URL`] still
///      wins, because callers only pass `Some` when the flag/env var was
///      actually supplied (see [`resolve_daemon_url`]'s doc). Before #2487
///      this step also compared the value against `DEFAULT_DAEMON_URL` to
///      guess whether it was "real"; that heuristic is exactly what let
///      `TRUSTY_MPM_URL=http://127.0.0.1:7880` (the literal default) fall
///      through to the gateway probe below and get silently rerouted through
///      the trusty-console proxy on a different port.
///   b) Gateway probe: read the console address from its discovery file
///      (falling back to `DEFAULT_CONSOLE_ADDR`). Construct
///      `http://{console}/api/mpm` and probe `GET .../health` with a 500 ms
///      timeout. If it returns 200 → return the gateway base URL.
///   c) Direct fallback: delegate to `resolve_daemon_url_probing` (lock file →
///      `DEFAULT_DAEMON_URL`). This path is taken whenever the console is
///      absent or the gateway probe fails.
///
/// Test: `resolve_via_gateway_explicit_override_wins`,
///       `resolve_via_gateway_explicit_override_equal_to_default_still_wins`,
///       `resolve_via_gateway_uses_gateway_when_probe_succeeds`,
///       `resolve_via_gateway_falls_back_to_direct_on_probe_failure`.
pub async fn resolve_daemon_url_via_gateway(
    client: &reqwest::Client,
    explicit: Option<&str>,
) -> String {
    let console_addr = trusty_common::read_daemon_addr("trusty-console")
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_CONSOLE_ADDR.to_string());
    resolve_daemon_url_via_gateway_inner(client, explicit, &console_addr).await
}

/// Inner implementation of the gateway resolver with an injected console addr.
///
/// Why: separates the file-system read (`read_daemon_addr`) from the probe
/// logic so tests can supply a controlled address without touching disk or
/// manipulating `$HOME`.
/// What: implements the three-step precedence (explicit override → gateway
/// probe → direct fallback) using the caller-supplied `console_addr`.
/// Test: called directly by the `resolve_via_gateway_*` test cases.
async fn resolve_daemon_url_via_gateway_inner(
    client: &reqwest::Client,
    explicit: Option<&str>,
    console_addr: &str,
) -> String {
    // a) Explicit override — any non-empty `Some` bypasses the gateway
    //    entirely, whether or not the value happens to equal
    //    `DEFAULT_DAEMON_URL` (#2487). Callers only pass `Some` when the
    //    operator actually supplied `--url` / `TRUSTY_MPM_URL`.
    if let Some(url) = explicit
        && !url.is_empty()
    {
        return url.to_string();
    }

    // b) Gateway probe — build the gateway base URL and probe its /health.
    //    `probe_url` appends "/health", so the actual request is:
    //    `GET http://{console}/api/mpm/health`, which the console proxy
    //    forwards to the daemon's `GET /health`.
    //
    //    IMPORTANT: use a **dedicated** short-timeout client for this probe,
    //    independent of the caller's `client`. Even now that the caller's
    //    client is bounded (`client::http_client::default_client()`, issue
    //    #2517 — previously a bare `reqwest::Client::new()` in main.rs with
    //    no timeout at all), its bound (10s) is tuned for the local daemon,
    //    not this console-gateway hop. A slow or unreachable console must
    //    NEVER stall every `tm` command for up to 10 seconds — the gateway
    //    probe must fail fast (500ms) and fall through to the direct path.
    //    The per-request `.timeout()` in `probe_url` provides a second line of
    //    defense; the client-level timeout here is the primary guarantee.
    let probe_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        // Client::builder().timeout(...).build() is infallible without custom
        // TLS or certificate configuration — both are absent here.
        .expect("gateway probe client is infallible");

    let gateway_base = format!("http://{console_addr}{GATEWAY_PATH}");
    if probe_url(&probe_client, &gateway_base).await {
        return gateway_base;
    }

    // c) Direct fallback — console absent or gateway probe failed. Delegate to
    //    the direct-daemon resolver chain (lock file → DEFAULT_DAEMON_URL) so
    //    `tm` still works when only the daemon is running.
    resolve_daemon_url_probing(client, explicit).await
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
    fn explicit_value_equal_to_default_wins_immediately() {
        // Why (issue #2487): prior to the fix, `resolve_daemon_url` compared the
        // explicit value against `DEFAULT_DAEMON_URL` by string equality and, on a
        // match, treated it as "not a real override" — falling through to the lock
        // file. That heuristic could never distinguish "clap injected the default
        // because nothing was passed" from "the operator explicitly set
        // TRUSTY_MPM_URL to the literal default value", so a deliberate
        // `TRUSTY_MPM_URL=http://127.0.0.1:7880` silently lost to a running lock
        // file (or, one layer up in `resolve_daemon_url_via_gateway`, to the
        // trusty-console gateway probe — the exact #2487 symptom). Callers now only
        // ever pass `Some` when the flag/env var was truly supplied (every
        // CLI-facing `url` field is `Option<String>` with no `default_value`), so
        // this function no longer needs — or performs — that comparison: `Some`
        // always wins, deterministically, with no dependency on lock-file state.
        let result = resolve_daemon_url(Some(DEFAULT_DAEMON_URL));
        assert_eq!(
            result, DEFAULT_DAEMON_URL,
            "an explicit value equal to the default must win outright, not fall through to the lock file"
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

    // ── resolve_daemon_url_via_gateway tests (#1849 Phase 2) ─────────────────

    /// Explicit user-supplied URL bypasses the gateway entirely.
    ///
    /// Why: `--url http://…:9999` must still win when the operator deliberately
    /// overrides the address. The gateway resolver must not intercept it.
    /// What: calls `resolve_daemon_url_via_gateway_inner` with a non-default,
    /// non-empty explicit URL and a dummy console addr; asserts the explicit
    /// URL is returned unchanged.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_via_gateway_explicit_override_wins() {
        let client = reqwest::Client::new();
        let explicit = "http://127.0.0.1:9999";
        let result =
            resolve_daemon_url_via_gateway_inner(&client, Some(explicit), "127.0.0.1:1").await;
        assert_eq!(
            result, explicit,
            "explicit override must win over gateway probe"
        );
    }

    /// Regression test for issue #2487: an explicit `TRUSTY_MPM_URL` that
    /// happens to equal [`DEFAULT_DAEMON_URL`] must still bypass the gateway,
    /// even when a live console is reachable and would otherwise win.
    ///
    /// Why: this is the exact production symptom — `TRUSTY_MPM_URL=http://
    /// 127.0.0.1:7880` (7880 IS `DEFAULT_DAEMON_URL`) got silently rerouted
    /// through the trusty-console proxy on port 7788 because the pre-fix
    /// resolver's `url != DEFAULT_DAEMON_URL` check made it indistinguishable
    /// from "nothing was passed". This test binds a listener that WOULD win
    /// the gateway probe (responds 200 to `/api/mpm/health`) to prove the
    /// explicit value still wins outright and the listener is never even
    /// contacted.
    /// What: calls `resolve_daemon_url_via_gateway_inner` with
    /// `Some(DEFAULT_DAEMON_URL)` and a console address that answers every
    /// probe with HTTP 200; asserts the result is `DEFAULT_DAEMON_URL`
    /// verbatim (not the gateway base URL) and that the listener saw zero
    /// connections.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_via_gateway_explicit_override_equal_to_default_still_wins() {
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local_addr");
        let hit_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hit_count_clone = hit_count.clone();
        tokio::spawn(async move {
            // Accept-and-200 loop: if the gateway were (incorrectly) probed,
            // this would answer successfully and the bug would still be masked.
            while let Ok((mut sock, _)) = listener.accept().await {
                hit_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await;
            }
        });

        let client = reqwest::Client::new();
        let console_addr = addr.to_string();
        let result =
            resolve_daemon_url_via_gateway_inner(&client, Some(DEFAULT_DAEMON_URL), &console_addr)
                .await;

        assert_eq!(
            result, DEFAULT_DAEMON_URL,
            "TRUSTY_MPM_URL equal to the default must win outright, never the gateway URL"
        );
        // Give any (incorrect) in-flight probe a moment to land before asserting
        // the listener was never contacted.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            hit_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the gateway/console must never be probed when an explicit override is present"
        );
    }

    /// Gateway is used when the console probe returns 200, and the probe
    /// specifically targets `/api/mpm/health` — not the bare base URL.
    ///
    /// Why: the primary goal of gateway resolution — when the console is
    /// running and the daemon is reachable through it, all `tm` traffic should
    /// route via `http://{console}/api/mpm`. Confirming the probe path prevents
    /// a regression where a bare GET /api/mpm (empty suffix) gets routed
    /// differently from GET /api/mpm/health by the proxy, causing spurious
    /// fallback to direct even when the gateway is healthy.
    /// What: binds an ephemeral TCP listener that reads the request line and
    /// asserts it is `GET /api/mpm/health …` before responding HTTP 200; uses
    /// its address as the injected `console_addr`; calls
    /// `resolve_daemon_url_via_gateway_inner` with no explicit override; asserts
    /// the returned URL is the gateway base URL.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_via_gateway_uses_gateway_when_probe_succeeds() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local_addr");

        // Capture the request line to confirm the probe targets /api/mpm/health.
        let (path_tx, path_rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            if let Ok((sock, _)) = listener.accept().await {
                let (reader, mut writer) = sock.into_split();
                let mut lines = BufReader::new(reader).lines();
                let req_line = lines.next_line().await.unwrap().unwrap_or_default();
                let _ = path_tx.send(req_line);
                // Respond 200 only after capturing the line so the reqwest
                // future sees the status and probe_url returns true.
                let _ = writer
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await;
            }
        });

        let client = reqwest::Client::new();
        let console_addr = addr.to_string();
        let result = resolve_daemon_url_via_gateway_inner(&client, None, &console_addr).await;

        let expected_gateway = format!("http://{console_addr}{GATEWAY_PATH}");
        assert_eq!(
            result, expected_gateway,
            "gateway URL must be returned when console probe succeeds"
        );

        // Verify the probe hit /api/mpm/health, not the bare /api/mpm base.
        let req_line = path_rx.await.expect("request line must be captured");
        assert!(
            req_line.starts_with("GET /api/mpm/health "),
            "gateway probe must target /api/mpm/health; got: {req_line:?}"
        );
    }

    /// Direct fallback is used when the gateway probe fails.
    ///
    /// Why: when the console is not running (port 1 is always refused) the
    /// resolver must fall through to the direct daemon chain, not return the
    /// unreachable gateway URL.
    /// What: calls `resolve_daemon_url_via_gateway_inner` with port 1 as the
    /// console addr (always refused) and no explicit override; asserts the
    /// result is a valid HTTP URL and NOT the failed gateway URL.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_via_gateway_falls_back_to_direct_on_probe_failure() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("build client");
        // Port 1 is reserved and never listening — probe must fail immediately.
        let dead_console = "127.0.0.1:1";
        let result = resolve_daemon_url_via_gateway_inner(&client, None, dead_console).await;

        let gateway_url = format!("http://{dead_console}{GATEWAY_PATH}");
        assert!(
            result.starts_with("http"),
            "fallback must still return an HTTP URL, got: {result}"
        );
        assert_ne!(
            result, gateway_url,
            "must not return the unreachable gateway URL on probe failure"
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

    // ── explicit_url_from_env tests (#2487) ───────────────────────────────

    /// Serialises the `explicit_url_from_env` tests' `TRUSTY_MPM_URL`
    /// mutation against every other test in the crate that reads/writes
    /// process env vars — `std::env::set_var`/`remove_var` are not
    /// thread-safe across concurrently running tests (the known env-race
    /// flake class in this workspace). Delegates to the crate-wide
    /// [`crate::core::trusty_tools_config::env_test_lock`] so these tests are
    /// serialised against env tests in OTHER modules too, not just each
    /// other.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::core::trusty_tools_config::env_test_lock()
    }

    #[test]
    fn explicit_url_from_env_reads_set_var() {
        let _g = env_lock();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        unsafe { std::env::set_var("TRUSTY_MPM_URL", "http://127.0.0.1:7880") };
        let result = explicit_url_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("TRUSTY_MPM_URL", v) },
            None => unsafe { std::env::remove_var("TRUSTY_MPM_URL") },
        }
        assert_eq!(
            result.as_deref(),
            Some("http://127.0.0.1:7880"),
            "must read TRUSTY_MPM_URL verbatim, even when it equals the default"
        );
    }

    #[test]
    fn explicit_url_from_env_treats_empty_as_unset() {
        let _g = env_lock();
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        unsafe { std::env::set_var("TRUSTY_MPM_URL", "   ") };
        let result = explicit_url_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("TRUSTY_MPM_URL", v) },
            None => unsafe { std::env::remove_var("TRUSTY_MPM_URL") },
        }
        assert_eq!(
            result, None,
            "a whitespace-only TRUSTY_MPM_URL must resolve to None, same as unset"
        );
    }

    #[test]
    fn explicit_url_from_env_unset_is_none() {
        let _g = env_lock();
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        unsafe { std::env::remove_var("TRUSTY_MPM_URL") };
        let result = explicit_url_from_env();
        if let Some(v) = prior {
            unsafe { std::env::set_var("TRUSTY_MPM_URL", v) };
        }
        assert_eq!(result, None, "an unset TRUSTY_MPM_URL must resolve to None");
    }
}
