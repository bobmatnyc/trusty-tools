//! Shared "ensure daemon running" helper for trusty-* CLI commands.
//!
//! Why: trusty-search, trusty-memory, and trusty-analyze all have a CLI
//! daemon_guard module that probes a health endpoint, optionally spawns a
//! detached daemon process, then polls with a spinner until the daemon is
//! ready or a timeout is exceeded. The spin/probe/timeout logic was identical
//! across all three crates. This module is the single shared implementation;
//! each crate's daemon_guard.rs is reduced to a thin shim that fills in the
//! service-specific knobs (health path, timeout, spawn args) and delegates
//! here. See issue #985.
//!
//! What: `DaemonGuardConfig` carries all service-specific parameters;
//! `probe_once` and `spin_until_ready` together implement the full guard loop.
//! `spawn_current_exe` is the shared process-spawn helper.
//!
//! STDOUT hygiene: like `mcp::daemon_bridge`, this module NEVER writes to
//! stdout. All user-visible output (spinner, ready/timeout messages) goes to
//! stderr so stdout stays clean for JSON piping and MCP framing.
//!
//! Test: `probe_once_returns_false_for_refused_port` and
//! `spin_until_ready_returns_ok_for_live_server` exercise the core paths
//! without requiring a real daemon binary.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use colored::Colorize;

/// Per-probe HTTP timeout.
///
/// Why: a hung or half-started daemon must not exhaust the spinner budget on a
/// single stalled TCP connect. 750 ms matches the value used by all three
/// daemon_guard copies.
/// What: connect + read deadline applied to each `probe_once` call.
/// Test: probe tests assert completion within 6s (generous for filtered ports).
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Polling interval between health probes during the spinner loop.
///
/// Why: 500 ms keeps the spinner feeling responsive without hammering the
/// daemon during its own boot sequence.
/// What: sleep duration in `spin_until_ready` between probe attempts.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default hard-error budget for the daemon to become ready after spawning.
///
/// Why: 30s is the value used by both trusty-memory and trusty-analyze; the
/// search crate historically used 60s but that was for ONNX model loading
/// which is no longer on the critical-start path. Callers can override via
/// `DaemonGuardConfig::startup_timeout`.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Spinner animation frames cycled while waiting for the daemon.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Configuration for a service's CLI daemon-guard startup check.
///
/// Why: each service has its own health path, timeout, and error hint. Encoding
/// those differences in a config struct lets `spin_until_ready` be a single
/// tested function rather than three near-identical copies.
/// What: holds the full health URL (so the caller can handle dynamic-port
/// resolution), the ready budget, and the error hint shown on timeout.
/// Test: `spin_until_ready_returns_ok_for_live_server` constructs one and
/// exercises the happy path.
pub struct DaemonGuardConfig {
    /// Full `http://host:port/path` URL to probe for health.
    pub health_url: String,
    /// Human-readable service name used in spinner messages.
    pub service_name: String,
    /// Wall-clock budget before the guard hard-errors.
    pub startup_timeout: Duration,
    /// Polling interval between probes.
    pub poll_interval: Duration,
    /// One-line hint appended to the timeout error message.
    pub timeout_hint: String,
}

impl DaemonGuardConfig {
    /// Build a `DaemonGuardConfig` with `DEFAULT_STARTUP_TIMEOUT` and
    /// `DEFAULT_POLL_INTERVAL`.
    ///
    /// Why: the three call sites that replace their inline guards only need to
    /// specify the service-specific parts (URL, name, hint); sensible defaults
    /// handle the rest.
    /// What: fills `startup_timeout` and `poll_interval` with the module
    /// defaults; callers can override those fields afterwards if needed.
    /// Test: exercised by every test that constructs a `DaemonGuardConfig`.
    pub fn new(
        health_url: impl Into<String>,
        service_name: impl Into<String>,
        timeout_hint: impl Into<String>,
    ) -> Self {
        Self {
            health_url: health_url.into(),
            service_name: service_name.into(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            timeout_hint: timeout_hint.into(),
        }
    }
}

/// Probe the given health URL once; returns `true` on any 2xx HTTP response.
///
/// Why: a fresh `reqwest::Client` per probe avoids carrying connection-pool
/// state from a failed probe to a later successful one, keeping the logic
/// simple and predictable across cold/warm starts.
/// What: builds a one-shot reqwest client with `PROBE_TIMEOUT`, issues a GET,
/// returns `true` on any 2xx status and `false` on any error or non-2xx.
/// Test: `probe_once_returns_false_for_refused_port` (async unit test below).
pub async fn probe_once(health_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(
        client.get(health_url).send().await,
        Ok(r) if r.status().is_success()
    )
}

/// Spawn `current_exe()` with the given arguments as a detached background
/// process (all stdio fds null-ed).
///
/// Why: every daemon_guard copy spawns `<current_exe> <args>` with stdin,
/// stdout, and stderr redirected to null so the daemon outlives the parent
/// terminal / shell and does not pollute the user's output. Using
/// `current_exe()` ensures a `cargo run` session boots its own debug daemon
/// and a production install boots the production binary.
/// What: resolves `current_exe()`, spawns it with the provided args and all
/// stdio null-ed, returns the child PID.
/// Test: compile-only (spawning a real process in unit tests risks port/FS
/// side-effects; the live path is exercised by integration tests).
pub fn spawn_current_exe(args: &[&str]) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    let child = std::process::Command::new(&exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow!(
                "could not spawn `{} {}`: {e}",
                exe.display(),
                args.join(" "),
            )
        })?;
    Ok(child.id())
}

/// Poll `config.health_url` until the daemon is ready, printing a spinner to
/// stderr. The daemon is assumed to have already been spawned (or been
/// confirmed already running) by the caller.
///
/// Why: the spinner loop was copy-pasted verbatim across the three daemon_guard
/// files. This function is the single tested implementation; see issue #985.
/// What: polls `probe_once(config.health_url)` every `config.poll_interval`,
/// renders a braille spinner and elapsed-second counter to stderr, clears the
/// line on success, and hard-errors with `config.timeout_hint` after
/// `config.startup_timeout`.
/// Test: `spin_until_ready_returns_ok_for_live_server` (async integration test).
pub async fn spin_until_ready(config: &DaemonGuardConfig) -> Result<()> {
    let deadline = Instant::now() + config.startup_timeout;
    let start = Instant::now();
    let mut frame = 0usize;
    loop {
        let elapsed = start.elapsed().as_secs();
        let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
        eprint!(
            "\r{} Waiting for {} to become ready… ({}s) ",
            glyph.cyan(),
            config.service_name,
            elapsed
        );
        let _ = std::io::stderr().flush();
        frame = frame.wrapping_add(1);

        tokio::time::sleep(config.poll_interval).await;
        if probe_once(&config.health_url).await {
            // Erase the spinner line so subsequent output starts fresh.
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            eprintln!(
                "{} {} ready ({}s)",
                "✓".green(),
                config.service_name,
                start.elapsed().as_secs()
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            return Err(anyhow!(
                "{} did not become ready within {}s — {}",
                config.service_name,
                config.startup_timeout.as_secs(),
                config.timeout_hint,
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
    use std::time::Instant;

    /// Why: `probe_once` against an unbound localhost port must return `false`
    /// without panicking, within a generous wall-clock bound.
    /// What: probes port 65535 (never bound in the test environment).
    /// Test: this test.
    #[tokio::test]
    async fn probe_once_returns_false_for_refused_port() {
        let started = Instant::now();
        let ok = probe_once("http://127.0.0.1:65535/health").await;
        assert!(!ok, "probe must fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why: a malformed URL must not panic — reqwest converts it to an error and
    /// `probe_once` must translate that to `false`.
    /// What: passes a non-URL string; asserts `false` is returned.
    /// Test: this test.
    #[tokio::test]
    async fn probe_once_returns_false_for_bad_url() {
        let ok = probe_once("not-a-valid-url").await;
        assert!(!ok);
    }

    /// Why: `spin_until_ready` must return `Ok(())` immediately when the
    /// health endpoint is already responsive.
    /// What: binds a real TCP listener that returns `HTTP/1.1 200 OK` on every
    /// connection, then calls `spin_until_ready`. Using a real listener avoids
    /// mocking the reqwest client while keeping the test hermetic.
    /// Test: this test.
    #[tokio::test]
    async fn spin_until_ready_returns_ok_for_live_server() {
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

        let cfg = DaemonGuardConfig {
            health_url: format!("http://127.0.0.1:{port}/health"),
            service_name: "test-daemon".to_string(),
            startup_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(50),
            timeout_hint: "run `test-daemon start` to debug".to_string(),
        };
        let result = spin_until_ready(&cfg).await;
        assert!(
            result.is_ok(),
            "spin_until_ready must succeed when daemon is up: {result:?}"
        );
    }

    /// Why: when the daemon never starts, `spin_until_ready` must return `Err`
    /// after the timeout rather than looping forever.
    /// What: uses a very short timeout and a definitely-free port.
    /// Test: this test.
    #[tokio::test]
    async fn spin_until_ready_times_out_for_down_daemon() {
        let cfg = DaemonGuardConfig {
            health_url: "http://127.0.0.1:1/health".to_string(),
            service_name: "test-daemon".to_string(),
            startup_timeout: Duration::from_millis(200),
            poll_interval: Duration::from_millis(50),
            timeout_hint: "run `test-daemon start` to debug".to_string(),
        };
        let result = spin_until_ready(&cfg).await;
        assert!(
            result.is_err(),
            "spin_until_ready must fail when daemon never starts"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("test-daemon"),
            "error must name the service; got: {msg}"
        );
    }
}
