//! Auto-start the trusty-analyze daemon when a CLI command needs it.
//!
//! Why: the `dashboard` command (and any other command that requires a running
//! daemon) previously failed with a static "is not running" error. This guard
//! probes `/health` on the configured port, spawns the daemon in the background
//! when absent, then polls until ready. Users get a single informational spinner
//! line and the command they typed Just Works.
//!
//! What: thin shim over `trusty_common::daemon_guard` (issue #985).
//! `ensure_daemon_running` delegates the spinner/probe/timeout loop to the
//! shared implementation; only the trusty-analyze–specific knobs (port-based
//! health URL construction, spawn args `serve --port <port>`, PID-file check)
//! live here.
//!
//! Test: `probe_health_returns_false_on_connection_refused`,
//! `ensure_daemon_running_returns_ok_when_already_healthy`, and
//! `probe_health_returns_false_quickly_for_free_port` cover the shim layer;
//! `trusty_common::daemon_guard` tests cover the shared spin loop.
//!
//! Note: only call this from commands that *require* the daemon. Commands like
//! `start`, `stop`, `serve`, `service`, and `completions` deliberately do not
//! call this guard.

use std::time::Duration;

use anyhow::{anyhow, Result};
use colored::Colorize;
use trusty_common::daemon_guard::{probe_once, spin_until_ready, DaemonGuardConfig};

/// Probe `GET http://127.0.0.1:{port}/health`. Returns `true` on any 2xx response.
///
/// Why: caller code across trusty-analyze uses the port-based signature; this
/// wrapper keeps that API stable while delegating to the shared
/// `trusty_common::daemon_guard::probe_once`.
/// What: constructs the full URL then calls `probe_once`.
/// Test: `probe_health_returns_false_on_connection_refused` below.
pub async fn probe_health(port: u16) -> bool {
    probe_once(&format!("http://127.0.0.1:{port}/health")).await
}

/// Spawn the daemon in the background on `port`, returning the child PID.
///
/// Why: invokes `<current_exe> serve --port <port>` with all stdio null-ed so
/// the daemon outlives the parent process and does not pollute the terminal.
/// What: delegates to `trusty_common::daemon_guard::spawn_current_exe`.
/// Test: `handle_start` in `daemon.rs` exercises the same spawn pattern.
fn spawn_daemon(port: u16) -> Result<u32> {
    let port_str = port.to_string();
    trusty_common::daemon_guard::spawn_current_exe(&["serve", "--port", &port_str])
        .map_err(|e| anyhow!("trusty-analyze daemon spawn failed: {e}"))
}

/// Ensure the trusty-analyze daemon is running on `port`.
///
/// Why: gives any daemon-requiring command a single shared "boot if absent"
/// path so the user never has to run `trusty-analyze start` first.
/// What: fast-path probes `/health`; on miss, checks the PID file to avoid
/// double-spawning a booting daemon, then spawns (or just waits), and
/// delegates the spinner/poll/timeout loop to
/// `trusty_common::daemon_guard::spin_until_ready` (30s budget).
/// Test: `ensure_daemon_running_returns_ok_when_already_healthy` below.
pub async fn ensure_daemon_running(port: u16) -> Result<()> {
    // Fast path: daemon is already up.
    if probe_health(port).await {
        return Ok(());
    }

    // Check for a stale-but-booting daemon via the PID file before spawning
    // a duplicate.
    let already_running = super::daemon::pid_file_path()
        .ok()
        .and_then(|p| {
            let raw = std::fs::read_to_string(&p).ok()?;
            raw.trim().parse::<u32>().ok()
        })
        .is_some();

    if already_running {
        eprint!(
            "{} trusty-analyze daemon already starting, waiting for it to become ready…",
            "◉".cyan()
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());
    } else {
        eprintln!("{} Starting trusty-analyze daemon…", "◉".cyan());
        spawn_daemon(port)?;
    }

    let cfg = DaemonGuardConfig {
        health_url: format!("http://127.0.0.1:{port}/health"),
        service_name: "trusty-analyze".to_string(),
        startup_timeout: Duration::from_secs(30),
        poll_interval: Duration::from_millis(500),
        timeout_hint: format!("try `trusty-analyze serve --port {port}` manually to see the error"),
    };
    spin_until_ready(&cfg).await
}

/// Ensure the trusty-analyze daemon is reachable for the MCP stdio bridge.
///
/// Why: the `mcp` subcommand acts as a stdio bridge that forwards every tool
/// call to the daemon's REST API. If the daemon is down, every tool call
/// fails with a connection error. Auto-starting matches the pattern
/// established by trusty-memory and trusty-search (issue #1078).
/// What: uses the shared `trusty_common::mcp::DaemonBridgeConfig` to probe
/// the health endpoint derived from `analyzer_url`. On miss, spawns
/// `<current_exe> serve --port <port>` detached and polls until ready (30s
/// budget). Returns the live base URL so the caller can construct the
/// `AnalyzerMcpServer` with the confirmed-reachable address.
/// Test: covered by the `trusty_common::mcp::daemon_bridge` unit tests; the
/// live path is exercised by `cargo run -- mcp` with no daemon running.
pub async fn ensure_mcp_daemon_up(analyzer_url: &str) -> anyhow::Result<String> {
    use trusty_common::mcp::DaemonBridgeConfig;

    let base_url = analyzer_url.to_string();
    let base_url_clone = base_url.clone();
    let config = DaemonBridgeConfig {
        service_name: "trusty-analyze".to_string(),
        spawn_args: {
            let port = analyzer_url
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .rsplit(':')
                .next()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(trusty_analyze::service::DEFAULT_PORT);
            vec!["serve".to_string(), "--port".to_string(), port.to_string()]
        },
        health_path: "/health".to_string(),
        base_url_fn: Box::new(
            move || match trusty_common::read_daemon_addr("trusty-analyze") {
                Ok(Some(addr)) if !addr.is_empty() => {
                    if addr.starts_with("http://") || addr.starts_with("https://") {
                        addr
                    } else {
                        format!("http://{addr}")
                    }
                }
                _ => base_url_clone.clone(),
            },
        ),
        startup_timeout: None,
        poll_interval: None,
    };
    trusty_common::mcp::ensure_daemon_up(&config).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Why: a down port must not return true — if it did, callers would skip
    /// the spawn and poll loop.
    /// What: picks an ephemeral port known to be free by binding+dropping,
    /// then asserts `probe_health` returns false.
    /// Test: this function.
    #[tokio::test]
    async fn probe_health_returns_false_on_connection_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let started = Instant::now();
        let ok = probe_health(port).await;
        assert!(!ok, "probe should fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why: already-healthy path must return early without spawning anything.
    /// What: binds a real TCP listener that answers "HTTP/1.1 200" to simulate
    /// the daemon's `/health`, then calls `ensure_daemon_running`.
    /// Test: `ensure_daemon_running` returns `Ok(())` quickly.
    #[tokio::test]
    async fn ensure_daemon_running_returns_ok_when_already_healthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(response).await;
                    });
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let result = ensure_daemon_running(port).await;
        assert!(
            result.is_ok(),
            "should succeed when daemon is already healthy"
        );
    }

    /// Why: `probe_health` must return false quickly for a definitely-free port.
    /// What: asserts `probe_health` returns false for port 1 (reserved).
    /// Test: this function.
    #[tokio::test]
    async fn probe_health_returns_false_quickly_for_free_port() {
        let started = Instant::now();
        let ok = probe_health(1).await;
        assert!(!ok);
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe should be fast: {:?}",
            started.elapsed()
        );
    }
}
