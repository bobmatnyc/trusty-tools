//! Shared "ensure daemon up" helper for MCP stdio bridge processes.
//!
//! Why: trusty-memory, trusty-search, and trusty-analyze all run an MCP stdio
//! bridge that must guarantee the corresponding HTTP daemon is reachable before
//! entering the JSON-RPC dispatch loop.  Each service had (or needed) the same
//! probe-spawn-poll pattern implemented locally.  This module centralises that
//! pattern so the three services share one tested implementation instead of
//! three diverging copies.
//!
//! What: `DaemonBridgeConfig` carries all service-specific knobs; `ensure_daemon_up`
//! probes the daemon's health endpoint, auto-starts it when absent, and polls
//! until the 30-second budget is exhausted or the daemon becomes ready.
//!
//! STDOUT hygiene: this module NEVER writes to stdout — stdout is the JSON-RPC
//! channel in all callers. All diagnostic output goes to stderr.
//!
//! Test: `daemon_bridge_config_health_url` validates URL construction; async tests
//! cover the fast-path (daemon already up) and the error path (refused port).

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

/// Per-probe HTTP timeout for the health check inside `ensure_daemon_up`.
///
/// Why: a hung or half-started daemon must not block the stdio bridge indefinitely
/// on a single TCP connect.  750 ms is short enough to keep the bridge snappy
/// while being long enough for a busy machine to accept the connection.
/// Test: `fast_path_returns_quickly_for_live_listener` verifies the bound holds.
const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Default polling interval between health probes while waiting for the daemon.
///
/// Why: 500 ms keeps the bridge startup latency low while not hammering the
/// daemon with connection attempts during its own boot sequence.
pub const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default hard-error budget for the daemon to become ready after being spawned.
///
/// Why: 30 s gives a cold-start daemon (first-run model download, redb open,
/// port selection) generous headroom while capping the worst-case wait at a
/// user-perceptible but finite interval.
pub const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for a service's MCP daemon-bridge startup guard.
///
/// Why: each service (trusty-memory, trusty-search, trusty-analyze) has its own
/// daemon binary, spawn arguments, and health path.  `DaemonBridgeConfig` captures
/// those differences in a single struct so `ensure_daemon_up` can be a single
/// parameterised function rather than three near-identical functions.
/// What: holds the service name (for diagnostics), the arguments appended to
/// `current_exe()` when spawning the daemon, the path for health probing (e.g.
/// `/health` or `/api/v1/health`), a URL-resolver closure, and optional timeout
/// overrides.
/// Test: `daemon_bridge_config_health_url` unit test.
pub struct DaemonBridgeConfig {
    /// Human-readable service name, used in diagnostic messages.
    pub service_name: String,
    /// Arguments passed to `current_exe()` when the daemon is not running.
    /// Example: `&["serve", "--foreground", "--http", "127.0.0.1:0"]`.
    pub spawn_args: Vec<String>,
    /// HTTP health endpoint path (including leading `/`).
    /// Example: `/health` or `/api/v1/health`.
    pub health_path: String,
    /// Closure that resolves the daemon's current base URL on each poll
    /// iteration.  Re-evaluated every iteration so a dynamic-port daemon (port
    /// 0) is discovered as soon as it writes its address file.
    pub base_url_fn: Box<dyn Fn() -> String + Send + Sync>,
    /// How long to wait for the daemon to become ready after spawning.
    /// Defaults to `DAEMON_START_TIMEOUT` when `None`.
    pub startup_timeout: Option<Duration>,
    /// Polling interval between health probes.
    /// Defaults to `DAEMON_POLL_INTERVAL` when `None`.
    pub poll_interval: Option<Duration>,
}

impl DaemonBridgeConfig {
    /// Build the health-probe URL from the current base URL and `health_path`.
    ///
    /// Why: `ensure_daemon_up` calls this on each iteration to produce the full
    /// probe URL without knowing the base URL ahead of time.
    /// What: concatenates `(self.base_url_fn)()` and `self.health_path`.
    /// Test: `daemon_bridge_config_health_url`.
    pub fn health_url(&self) -> String {
        format!("{}{}", (self.base_url_fn)(), self.health_path)
    }
}

/// Probe `GET <health_url>` once; returns `true` on any 2xx HTTP response.
///
/// Why: a fresh `reqwest::Client` per probe avoids connection-pool state
/// carrying over from a failed probe to a later successful one.
/// What: builds a one-shot client with `DAEMON_PROBE_TIMEOUT`, issues a GET,
/// returns `true` on 2xx, `false` on any error or non-2xx.
/// Test: `probe_health_once_returns_false_on_refused` (async unit test).
pub(crate) async fn probe_health_once(health_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(DAEMON_PROBE_TIMEOUT)
        .connect_timeout(DAEMON_PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(
        client.get(health_url).send().await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Ensure the daemon is running and return its live base URL.
///
/// Why: every daemon-backed MCP stdio bridge must guarantee the daemon is
/// reachable before forwarding requests.  Centralising this guarantee in one
/// tested function prevents three services from independently re-implementing
/// (and diverging in) the probe-spawn-poll pattern.
/// What: (1) fast-path probes the current health URL; returns immediately when
/// the daemon is already up.  (2) On miss, spawns `current_exe() + spawn_args`
/// as a detached background process; all stdio fds are null-ed so the spawned
/// daemon outlives the MCP bridge process.  (3) Polls every `poll_interval`
/// (re-evaluating `base_url_fn` each iteration for dynamic-port support) until
/// the daemon responds on `/health` or `startup_timeout` is exceeded.  Hard-
/// errors on timeout — there is no silent fallback.  All output to stderr only.
/// Test: `ensure_daemon_up_returns_ok_when_already_healthy` (async integration
/// test); `probe_health_once_returns_false_on_refused` (unit).
pub async fn ensure_daemon_up(config: &DaemonBridgeConfig) -> Result<String> {
    let startup_timeout = config.startup_timeout.unwrap_or(DAEMON_START_TIMEOUT);
    let poll_interval = config.poll_interval.unwrap_or(DAEMON_POLL_INTERVAL);

    // Fast path: daemon already healthy.
    let initial_url = (config.base_url_fn)();
    if probe_health_once(&config.health_url()).await {
        return Ok(initial_url);
    }

    // Slow path: spawn the daemon detached.
    eprintln!("\u{25cf} Starting {} daemon\u{2026}", config.service_name);

    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    std::process::Command::new(&exe)
        .args(&config.spawn_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow!(
                "could not spawn `{} {}`: {e}",
                exe.display(),
                config.spawn_args.join(" "),
            )
        })?;

    // Poll until ready, re-reading the base URL each iteration so dynamic ports
    // are discovered as soon as the daemon writes its address file.
    let deadline = Instant::now() + startup_timeout;
    loop {
        tokio::time::sleep(poll_interval).await;
        let current_url = (config.base_url_fn)();
        let health_url = format!("{current_url}{}", config.health_path);
        if probe_health_once(&health_url).await {
            eprintln!("\u{2713} {} daemon ready.", config.service_name);
            return Ok(current_url);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "{} daemon did not become ready within {}s. \
                 Check `{} doctor` for details. \
                 The MCP stdio bridge cannot operate without a running daemon.",
                config.service_name,
                startup_timeout.as_secs(),
                config.service_name,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_config(base_url: &'static str, health_path: &str) -> DaemonBridgeConfig {
        DaemonBridgeConfig {
            service_name: "test-svc".to_string(),
            spawn_args: vec!["serve".to_string(), "--foreground".to_string()],
            health_path: health_path.to_string(),
            base_url_fn: Box::new(move || base_url.to_string()),
            startup_timeout: Some(Duration::from_millis(100)), // very short for tests
            poll_interval: Some(Duration::from_millis(20)),
        }
    }

    /// Why: `health_url()` must concatenate base URL and health path exactly,
    /// with no double-slash or missing slash.
    /// Test: this test.
    #[test]
    fn daemon_bridge_config_health_url() {
        let cfg = make_config("http://127.0.0.1:9999", "/health");
        assert_eq!(cfg.health_url(), "http://127.0.0.1:9999/health");

        let cfg2 = make_config("http://127.0.0.1:9999", "/api/v1/health");
        assert_eq!(cfg2.health_url(), "http://127.0.0.1:9999/api/v1/health");
    }

    /// Why: `probe_health_once` against a refused port must return `false`
    /// quickly without hanging.
    /// Test: this test.
    #[tokio::test]
    async fn probe_health_once_returns_false_on_refused() {
        let started = std::time::Instant::now();
        let result = probe_health_once("http://127.0.0.1:65534/health").await;
        assert!(!result, "probe must fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why: when a live server answers `/health` with 200, `ensure_daemon_up`
    /// must return `Ok` immediately without spawning anything.
    /// What: binds a minimal TCP listener that returns `HTTP/1.1 200 OK` on
    /// every connection, feeds that port into `DaemonBridgeConfig`, and asserts
    /// `ensure_daemon_up` returns `Ok` within a short wall-clock bound.
    /// Test: this test.
    #[tokio::test]
    async fn ensure_daemon_up_returns_ok_when_already_healthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    });
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let base = format!("http://127.0.0.1:{port}");
        let cfg = DaemonBridgeConfig {
            service_name: "test-svc".to_string(),
            spawn_args: vec![],
            health_path: "/health".to_string(),
            base_url_fn: Box::new(move || base.clone()),
            startup_timeout: Some(Duration::from_secs(5)),
            poll_interval: Some(Duration::from_millis(50)),
        };
        let result = ensure_daemon_up(&cfg).await;
        assert!(
            result.is_ok(),
            "must succeed when daemon is healthy: {result:?}"
        );
    }

    /// Why: when nothing starts within the budget, `ensure_daemon_up` must
    /// return `Err` rather than hanging forever.
    /// Test: this test.
    #[tokio::test]
    async fn ensure_daemon_up_errors_on_timeout() {
        // Port 1 is reserved/refused on all test hosts.
        let cfg = make_config("http://127.0.0.1:1", "/health");
        let result = ensure_daemon_up(&cfg).await;
        assert!(
            result.is_err(),
            "must fail when the daemon never becomes ready"
        );
    }
}
