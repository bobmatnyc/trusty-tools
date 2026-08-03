//! Auto-start the trusty-memory daemon when a CLI command requires it.
//!
//! Why: `trusty-memory monitor web` and any future human-facing commands
//! that open the UI silently fail or emit a confusing connection error when
//! the daemon isn't running. This guard probes `/health`; if the
//! daemon is down it spawns a detached `<exe> serve --foreground` child and
//! polls until ready or a 30-second budget is exhausted. Users see a single
//! informational line and the command Just Works.
//!
//! What: thin shim over `trusty_common::daemon_guard` (issue #985).
//! `ensure_daemon_running` delegates the spinner/probe/timeout loop to the
//! shared implementation; only the trusty-memory–specific knobs (health path
//! `/health`, spawn args, base-URL resolver) live here.
//!
//! Test: `probe_health_returns_false_on_connection_refused`,
//! `probe_health_returns_false_on_bad_url`, and
//! `daemon_base_url_always_has_http_scheme` cover the shim layer;
//! `trusty_common::daemon_guard` tests cover the shared spin loop.
//!
//! Note: only call this from commands that *require* the daemon (e.g.
//! `monitor web`). Commands like `start`, `stop`, `serve`, `service`, and
//! `setup` deliberately do not call this guard.

use anyhow::{anyhow, Result};
use colored::Colorize;
use trusty_common::daemon_guard::{probe_once, spin_until_ready, DaemonGuardConfig};

/// Build the daemon's health-probe URL for `base`.
///
/// Why: the probe path has to agree with what the router actually registers —
/// `GET /health` (`web::router`). Deriving it in one place keeps `probe_health`
/// and `ensure_daemon_running` from drifting apart, which is exactly how #4635
/// happened.
/// What: appends the `/health` route to an already-scheme-qualified base URL.
/// Test: `health_url_targets_the_registered_health_route`.
// #4635: probe /health — /api/v1/health is not a registered route.
fn health_url(base: &str) -> String {
    format!("{base}/health")
}

/// Probe `GET {base}/health`. Returns `true` on any 2xx response.
///
/// Why: trusty-memory's health endpoint lives at `/health` — callers must use
/// this helper rather than constructing the URL themselves.
/// What: delegates to `trusty_common::daemon_guard::probe_once`.
/// Test: `probe_health_succeeds_against_the_registered_health_route`,
/// `probe_health_returns_false_on_connection_refused`,
/// `probe_health_returns_false_on_bad_url`.
pub async fn probe_health(base: &str) -> bool {
    probe_once(&health_url(base)).await
}

/// Spawn `<this exe> serve --foreground` as a detached background process.
///
/// Why: we want the daemon to outlive this CLI invocation. `--foreground`
/// prevents recursive self-spawning. Stdio is fully null-ed so the daemon
/// survives terminal close / SIGHUP.
/// What: delegates to `trusty_common::daemon_guard::spawn_current_exe`.
/// Test: covered indirectly by `ensure_daemon_running` live test path.
fn spawn_daemon() -> Result<u32> {
    trusty_common::daemon_guard::spawn_current_exe(&["serve", "--foreground"])
        .map_err(|e| anyhow!("trusty-memory daemon spawn failed: {e}"))
}

/// Resolve the trusty-memory daemon's base URL from the address-discovery file.
///
/// Why: trusty-memory selects a dynamic port from 7070–7079 and writes it to
/// `{data_dir}/http_addr`. Reading that file is the canonical discovery path.
/// What: delegates to `trusty_common::read_daemon_addr("trusty-memory")` and
/// normalises the bare `host:port` string to a full `http://` URL; falls back
/// to `http://127.0.0.1:7070` when no file is found.
/// Test: `daemon_base_url_always_has_http_scheme`.
pub fn daemon_base_url() -> String {
    match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) if !addr.is_empty() => {
            if addr.starts_with("http://") || addr.starts_with("https://") {
                addr
            } else {
                format!("http://{addr}")
            }
        }
        _ => "http://127.0.0.1:7070".to_string(),
    }
}

/// Ensure the trusty-memory daemon is running and healthy.
///
/// Why: `monitor web` should never tell the user "daemon not running — start
/// it yourself". Auto-starting matches the UX in `trusty-search` and
/// `trusty-analyze`.
/// What: fast-path probes `/health`; on miss, spawns
/// `<exe> serve --foreground`, then delegates the spinner/poll/timeout loop
/// to `trusty_common::daemon_guard::spin_until_ready` (30s budget).
/// Test: `probe_health_returns_false_on_connection_refused` covers the probe;
/// the full auto-start path is exercised manually via
/// `trusty-memory monitor web` with no daemon running.
pub async fn ensure_daemon_running(base: &str) -> Result<()> {
    if probe_health(base).await {
        return Ok(());
    }

    eprintln!("{} Starting trusty-memory daemon…", "◉".cyan());
    spawn_daemon()?;

    let cfg = DaemonGuardConfig {
        // #4635: probe /health — /api/v1/health is not a registered route.
        health_url: health_url(base),
        service_name: "trusty-memory".to_string(),
        startup_timeout: std::time::Duration::from_secs(30),
        poll_interval: std::time::Duration::from_millis(500),
        timeout_hint: "try `trusty-memory start` manually to see the error".to_string(),
    };
    spin_until_ready(&cfg).await
}

/// Open the trusty-memory web dashboard in the default system browser.
///
/// Why: operators and agents should never have to run `trusty-memory start`
/// before `trusty-memory monitor web`. This is the single entry point for
/// the `monitor web` subcommand, mirroring the UX in `trusty-analyze` and
/// `trusty-search`.
/// What: resolves the daemon base URL from the discovery file; calls
/// `ensure_daemon_running` which probes `/health` and auto-spawns as
/// needed; re-resolves the live URL after boot; opens `http://<addr>/ui`.
/// Browser-open failure degrades to printing the URL.
/// Test: `cargo run -p trusty-memory -- monitor web` with no daemon running.
pub async fn open_web_dashboard() -> Result<()> {
    let base = daemon_base_url();
    ensure_daemon_running(&base).await?;
    let live_base = daemon_base_url();
    let url = format!("{live_base}/ui");
    eprintln!("{} Opening {} …", "◉".green(), url.cyan());
    if let Err(e) = open::that(&url) {
        eprintln!(
            "{} could not launch browser ({e}). Open this URL manually: {}",
            "⚠".yellow(),
            url
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Why: #4635 — the guard probed `/api/v1/health`, which `web::router` never
    /// registers, so `monitor web` reported a healthy daemon as down. Pin the
    /// path against the route the router actually serves.
    /// What: asserts the probe URL is `{base}/health`.
    /// Test: this test.
    #[test]
    fn health_url_targets_the_registered_health_route() {
        assert_eq!(
            health_url("http://127.0.0.1:7070"),
            "http://127.0.0.1:7070/health"
        );
    }

    /// Why: the string assertion above cannot catch a router/probe disagreement
    /// on its own — #4635 needed a wire-level check that the path the guard
    /// requests is the path a trusty-memory daemon answers.
    /// What: binds a stub server that mirrors `web::router`'s health surface —
    /// 200 on `GET /health`, 404 on everything else, including `/api/v1/health`
    /// — then asserts `probe_health` resolves it as up. Fails against the
    /// pre-#4635 `/api/v1/health` probe path.
    /// Test: this test.
    #[tokio::test]
    async fn probe_health_succeeds_against_the_registered_health_route() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub health server");
        let addr = listener.local_addr().expect("stub server local addr");

        tokio::spawn(async move {
            let (mut sock, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => return,
            };
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            // Only `/health` is a registered route; anything else 404s, exactly
            // as the live daemon does for `/api/v1/health`.
            let resp = if req.starts_with("GET /health ") {
                "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot found"
            };
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });

        let base = format!("http://{addr}");
        assert!(
            probe_health(&base).await,
            "probe_health must resolve a daemon serving GET /health as up"
        );
    }

    /// Why: `probe_health` against an unbound localhost port must return `false`
    /// (connection refused) within a reasonable wall-clock bound, never panic.
    /// What: probes port 65535 which is never bound in the test environment;
    /// asserts the result is false and the call completes within 6 seconds.
    /// Test: this test.
    #[tokio::test]
    async fn probe_health_returns_false_on_connection_refused() {
        let base = "http://127.0.0.1:65535";
        let started = Instant::now();
        let ok = probe_health(base).await;
        assert!(!ok, "probe should fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why: a malformed URL must not panic — `reqwest` returns an error and
    /// `probe_health` must convert it to `false`.
    /// What: passes a non-URL string; asserts `false` is returned.
    /// Test: this test.
    #[tokio::test]
    async fn probe_health_returns_false_on_bad_url() {
        let ok = probe_health("not-a-valid-url").await;
        assert!(!ok);
    }

    /// Why: `daemon_base_url()` must always return a string starting with
    /// `http://` so callers can append paths without scheme detection.
    /// What: calls `daemon_base_url()` and asserts the scheme prefix.
    /// Test: this test.
    #[test]
    fn daemon_base_url_always_has_http_scheme() {
        let url = daemon_base_url();
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "daemon_base_url must start with http(s)://; got: {url}"
        );
    }
}
