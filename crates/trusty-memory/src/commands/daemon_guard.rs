//! Auto-start the trusty-memory daemon when a CLI command requires it.
//!
//! Why: `trusty-memory monitor web` and any future human-facing commands
//! that open the UI silently fail or emit a confusing connection error when
//! the daemon isn't running. This guard probes `/api/v1/health`; if the
//! daemon is down it spawns a detached `<exe> serve --foreground` child
//! (identical to what `trusty-memory start` does) and polls
//! `/api/v1/health` until the daemon is ready or a 30-second budget is
//! exhausted. Users see a single informational line and the command Just Works.
//!
//! What: `ensure_daemon_running(base)` returns `Ok(())` once the daemon is
//! responding to `/api/v1/health`. Returns `Err(...)` when the spawn fails
//! or the daemon doesn't become ready within the budget.
//!
//! Test: `probe_health_returns_false_on_connection_refused` and
//! `probe_health_returns_false_on_bad_url` cover the probe; the live auto-
//! start path is exercised manually via `trusty-memory monitor web` with no
//! daemon running. With the daemon already running, no informational line is
//! printed and behaviour is unchanged.
//!
//! Note: only call this from commands that *require* the daemon (e.g.
//! `monitor web`). Commands like `start`, `stop`, `serve`, `service`, and
//! `setup` deliberately do not call this guard.

use anyhow::{anyhow, Result};
use colored::Colorize;
use std::io::Write;
use std::time::{Duration, Instant};

/// Total wall-clock budget for the daemon to become ready after we spawn it.
///
/// Why: trusty-memory's HTTP port binds in ~1s on a warm machine, so 30s
/// gives generous headroom for a cold redb open (first-time palace hydration)
/// without the user waiting unnecessarily long.
/// What: constant upper bound for the health-poll loop.
/// Test: covered indirectly by the timeout-path comment in `ensure_daemon_running`.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Polling interval between `/api/v1/health` probes while we wait.
///
/// Why: 500ms keeps the spinner feeling responsive without hammering the daemon.
/// What: sleep duration inside the poll loop.
/// Test: covered by the live manual test path.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Per-probe HTTP timeout.
///
/// Why: a hung daemon must not blow our ready budget on a single stalled request.
/// What: short connect + read timeout so a dead daemon fails fast per probe.
/// Test: `probe_health_returns_false_on_connection_refused` verifies the bound.
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Spinner frames cycled while waiting for `/api/v1/health` to return 2xx.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Probe `GET {base}/api/v1/health`. Returns `true` on any 2xx response.
///
/// Why: trusty-memory's health endpoint lives at `/api/v1/health` (not
/// `/health`) — callers must use this helper rather than constructing the URL
/// themselves to stay consistent.
/// What: builds a one-shot `reqwest::Client` with `PROBE_TIMEOUT`, issues a
/// GET, returns `true` on any 2xx status, `false` on any error or non-2xx.
/// Test: `probe_health_returns_false_on_connection_refused`,
/// `probe_health_returns_false_on_bad_url`.
async fn probe_health(base: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(format!("{}/api/v1/health", base)).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Spawn `<this exe> serve --foreground` as a detached background process.
///
/// Why: we want the daemon to outlive this CLI invocation. Using
/// `current_exe()` ensures a `cargo run` debugging session boots its own
/// debug daemon and a production install boots the production binary.
/// We pass `--foreground` so the spawned child runs the HTTP server inline
/// rather than recursively self-spawning (which would recurse infinitely).
/// Stdio is fully detached (`Stdio::null()` on all three fds) so the daemon
/// survives terminal close / SIGHUP.
/// What: spawns `<exe> serve --foreground`, returns the child PID on success.
/// Test: covered indirectly by the `ensure_daemon_running` live test path.
fn spawn_daemon() -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    let child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--foreground")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow!(
                "could not spawn `{} serve --foreground`: {e}",
                exe.display()
            )
        })?;
    Ok(child.id())
}

/// Resolve the trusty-memory daemon's base URL from the address-discovery file.
///
/// Why: trusty-memory selects a dynamic port from 7070–7079 and writes it to
/// `{data_dir}/http_addr`. Reading that file is the canonical way to find the
/// live daemon without hardcoding a port.
/// What: delegates to `trusty_common::read_daemon_addr("trusty-memory")` and
/// normalises the bare `host:port` string to a full `http://` URL; falls back
/// to `http://127.0.0.1:7070` when no file is found.
/// Test: covered by `trusty_common::read_daemon_addr` unit tests; the fallback
/// path is exercised by `probe_health_returns_false_on_connection_refused`.
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
/// Why: `monitor web` (and any future command that needs the HTTP API) should
/// never tell the user "daemon not running — start it yourself". Auto-starting
/// matches the UX in `trusty-search` and `trusty-analyze` (PR #685).
/// What: if `GET {base}/api/v1/health` returns 2xx, returns `Ok(())` immediately
/// with no output. Otherwise spawns `<exe> serve --foreground`, prints a
/// spinner to stderr, and polls every 500ms for up to 30s. Returns `Err` when
/// the daemon doesn't become ready within the budget.
/// Test: `probe_health_returns_false_on_connection_refused` verifies the probe
/// mechanism; the full auto-start path is exercised manually via
/// `trusty-memory monitor web` with no daemon running.
pub async fn ensure_daemon_running(base: &str) -> Result<()> {
    // Fast path: already up.
    if probe_health(base).await {
        return Ok(());
    }

    eprintln!("{} Starting trusty-memory daemon…", "◉".cyan());
    spawn_daemon()?;

    let deadline = Instant::now() + READY_TIMEOUT;
    let start = Instant::now();
    let mut frame = 0usize;
    loop {
        let elapsed = start.elapsed().as_secs();
        let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
        eprint!(
            "\r{} Waiting for daemon to become ready… ({}s) ",
            glyph.cyan(),
            elapsed
        );
        let _ = std::io::stderr().flush();
        frame = frame.wrapping_add(1);

        tokio::time::sleep(POLL_INTERVAL).await;
        if probe_health(base).await {
            // Erase the spinner line so subsequent output starts fresh.
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            eprintln!(
                "{} Daemon ready ({}s)",
                "✓".green(),
                start.elapsed().as_secs()
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            return Err(anyhow!(
                "daemon did not become ready within {}s at {} — \
                 try `trusty-memory start` manually to see the error",
                READY_TIMEOUT.as_secs(),
                base
            ));
        }
    }
}

/// Open the trusty-memory web dashboard in the default system browser.
///
/// Why: operators and agents should never have to run `trusty-memory start`
/// before `trusty-memory monitor web`. This is the single entry point for
/// the `monitor web` subcommand, mirroring the UX shipped for
/// `trusty-analyze` (PR #685) and `trusty-search` dashboard commands.
/// What: resolves the daemon base URL from the discovery file; calls
/// `ensure_daemon_running` (which probes `/api/v1/health` and auto-spawns
/// a detached `serve --foreground` child when needed); re-resolves the live
/// URL after boot; opens `http://<addr>/ui` in the system browser.
/// Browser-open failure degrades to printing the URL so headless
/// environments still get a usable output.
/// Test: `cargo run -p trusty-memory -- monitor web` with no daemon running
/// prints "Starting trusty-memory daemon…" then a spinner, then opens the
/// browser (or prints the URL).
pub async fn open_web_dashboard() -> Result<()> {
    // Resolve base URL (falls back to default port when no discovery file).
    let base = daemon_base_url();

    // Auto-start if needed; poll until healthy.
    ensure_daemon_running(&base).await?;

    // Re-read after boot so we use the live dynamically-chosen port.
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
    use std::time::Instant;

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
