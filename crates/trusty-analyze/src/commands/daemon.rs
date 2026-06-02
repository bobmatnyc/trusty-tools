//! Handlers for `start`, `stop`, `status`, and `doctor`.
//!
//! Why: gives users a familiar `start/stop/status/doctor` lifecycle for the
//! analyzer daemon without forcing them to learn launchd/systemd. Mirrors the
//! UX of `trusty-search` and `trusty-memory`.
//! What: spawns the binary itself in the background (writing a PID file under
//! `~/.trusty-analyze/`), sends SIGTERM on stop, TCP-probes the port for
//! status, and runs a small battery of sanity checks for doctor.
//! Test: integration coverage lives in tests against the binary; this module
//! is exercised manually via `trusty-analyze start` / `stop` / `status` /
//! `doctor` and via the unit tests below.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use colored::Colorize;

/// Resolve the data directory used for the PID file.
///
/// Why: every trusty-* daemon writes runtime metadata under `~/.<name>/` so
/// users can find it predictably. Aligns with the launchd service template
/// that already uses `~/.trusty-analyze/`.
/// What: returns `~/.trusty-analyze/`, creating it if missing.
/// Test: callers panic on $HOME-less environments — the error message is
/// surfaced via `anyhow::Result` rather than `expect`.
pub fn data_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve $HOME")?;
    let dir = home.join(".trusty-analyze");
    std::fs::create_dir_all(&dir).with_context(|| format!("create data dir {}", dir.display()))?;
    Ok(dir)
}

/// Resolve the facts-store path, creating the parent directory if needed.
///
/// Why: the old default (`trusty-analyze.facts.redb`) is a CWD-relative path
/// that crashes under launchd (which sets cwd to `/`, a read-only root on
/// macOS).  The new default anchors to `~/.trusty-tools/analyze/facts.redb`
/// so the daemon always has a writable location regardless of cwd.  The
/// `TRUSTY_ANALYZER_FACTS` env var (or `--facts-path` CLI flag) still wins
/// as an absolute-path override.  (closes #632 os-error-30 crash)
/// What: if `cli_path` is `Some`, use it directly.  Otherwise resolve
/// `$HOME/.trusty-tools/analyze/facts.redb` via the `dirs` crate.  Creates
/// the parent directory (but not the file) when it does not exist.
/// Test: `test_default_facts_path_is_home_anchored` in daemon tests.
pub fn resolve_facts_path(cli_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = cli_path {
        return Ok(p);
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory for facts store path"))?;
    let dir = home.join(".trusty-tools").join("analyze");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create facts store directory {}", dir.display()))?;
    }
    Ok(dir.join("facts.redb"))
}

/// Path to the PID file used by `start` / `stop` / `status`.
///
/// Why: a single well-known location keeps the lifecycle commands trivial to
/// implement and lets external tooling reuse the same file.
/// What: returns `~/.trusty-analyze/daemon.pid`.
/// Test: covered transitively by `start_writes_pid_file` integration.
pub fn pid_file_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("daemon.pid"))
}

/// Read the PID stored in `daemon.pid`, returning `None` if absent or invalid.
///
/// Why: stop / status both need a tolerant reader — a missing or corrupt file
/// is normal and should not panic.
/// What: parses the trimmed file contents as `u32`.
/// Test: `read_pid_handles_missing_file` below.
fn read_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

/// Probe whether the analyzer daemon's HTTP port is accepting connections.
///
/// Why: TCP probing is the lightest-weight signal that the daemon is up; it
/// doesn't require HTTP framing or response parsing.
/// What: connects to `127.0.0.1:<port>` with a 500 ms timeout.
/// Test: returns `false` against a free port.
fn port_reachable(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Spawn the daemon in the background and write its PID.
///
/// Why: gives users a one-command "boot the daemon" path without forcing them
/// onto launchd. Background detach is done by spawning `serve` on the same
/// executable and immediately returning.
/// What: if a PID file exists with a live PID we exit early; otherwise we
/// `Command::spawn` the current exe with `serve`, write the child PID to
/// `~/.trusty-analyze/daemon.pid`, and print the dashboard URL.
/// Test: run `trusty-analyze start` twice — the second invocation reports
/// "already running" and exits 0.
pub fn handle_start(port: u16) -> Result<()> {
    let pid_path = pid_file_path()?;
    if let Some(pid) = read_pid(&pid_path) {
        if port_reachable(port) {
            println!(
                "{} trusty-analyze already running (pid {pid}, port {port})",
                "✓".green()
            );
            return Ok(());
        }
        // Stale PID file — remove and continue.
        let _ = std::fs::remove_file(&pid_path);
    }

    let exe = std::env::current_exe().context("resolve current executable")?;
    let child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {} serve", exe.display()))?;

    std::fs::write(&pid_path, child.id().to_string())
        .with_context(|| format!("write pid file {}", pid_path.display()))?;

    println!(
        "{} trusty-analyze started (pid {}, port {port})",
        "✓".green(),
        child.id()
    );
    println!(
        "  Dashboard: {}",
        format!("http://127.0.0.1:{port}/ui").cyan()
    );
    Ok(())
}

/// Stop the running daemon by sending SIGTERM to the recorded PID.
///
/// Why: pairs with `start` — a single command users can run to tear the
/// background daemon down cleanly.
/// What: reads the PID file, invokes `kill -TERM`, polls up to 5 s for the
/// process to release the port, then removes the PID file.
/// Test: with a running daemon → "stopped" message within a few seconds.
pub fn handle_stop(port: u16) -> Result<()> {
    let pid_path = pid_file_path()?;
    let Some(pid) = read_pid(&pid_path) else {
        eprintln!(
            "{} No PID file at {} — daemon not running?",
            "✗".red(),
            pid_path.display()
        );
        std::process::exit(1);
    };

    println!("{} Stopping trusty-analyze (pid {pid})…", "⟳".cyan());
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .context("invoke kill -TERM")?;
    if !status.success() {
        eprintln!(
            "{} kill -TERM {pid} failed (process may already be gone)",
            "✗".red()
        );
        let _ = std::fs::remove_file(&pid_path);
        std::process::exit(1);
    }

    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if !port_reachable(port) {
            let _ = std::fs::remove_file(&pid_path);
            println!("{} trusty-analyze stopped", "✓".green());
            return Ok(());
        }
    }
    println!(
        "{} Daemon did not release port {port} within 5 s; PID file left in place",
        "⚠".yellow()
    );
    Ok(())
}

/// Show daemon status: running/down, port, version when reachable.
///
/// Why: `health` already reports trusty-search status; this command focuses
/// on the analyzer itself with more detail (PID, version) for the user.
/// What: TCP-probes the analyzer port, reads the PID file, and queries
/// `/health` for the version string if the daemon answers.
/// Test: with the daemon down, prints "DOWN" and exits 0 (informational).
pub async fn handle_status(port: u16) -> Result<()> {
    let pid_path = pid_file_path()?;
    let pid = read_pid(&pid_path);
    let reachable = port_reachable(port);

    if reachable {
        println!("{} trusty-analyze: {}", "✓".green(), "RUNNING".green());
    } else {
        println!("{} trusty-analyze: {}", "✗".red(), "DOWN".red());
    }
    println!("  Port:     {port}");
    if let Some(pid) = pid {
        println!("  PID:      {pid} (from {})", pid_path.display());
    } else {
        println!("  PID:      {}", "<no pid file>".dimmed());
    }

    if reachable {
        let url = format!("http://127.0.0.1:{port}/health");
        let client = reqwest::Client::new();
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(v) = body.get("version").and_then(|v| v.as_str()) {
                        println!("  Version:  {v}");
                    }
                }
            }
            Ok(resp) => println!("  Health:   HTTP {}", resp.status()),
            Err(e) => println!("  Health:   probe failed: {e}"),
        }
    }
    Ok(())
}

/// Diagnose common configuration issues and print a ✓/✗ summary.
///
/// Why: gives users a fast self-service path to "why isn't this working?"
/// without needing to read tracing logs.
/// What: checks (1) daemon reachable on the configured port, (2) data dir
/// exists and is writable, (3) the facts-store path is openable.
/// Test: run `trusty-analyze doctor` with the daemon down — should print the
/// missing-daemon ✗ line and exit non-zero.
pub async fn handle_doctor(port: u16, facts_path: &Path) -> Result<()> {
    let mut ok = true;
    println!("trusty-analyze doctor:");

    // 1. Daemon reachability.
    if port_reachable(port) {
        println!("  {} daemon reachable on port {port}", "✓".green());
    } else {
        println!(
            "  {} daemon not reachable on port {port} (start it with `trusty-analyze start`)",
            "✗".red()
        );
        ok = false;
    }

    // 2. Data directory writable.
    match data_dir() {
        Ok(dir) => {
            let probe = dir.join(".doctor-probe");
            match std::fs::write(&probe, b"ok") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    println!("  {} data dir writable: {}", "✓".green(), dir.display());
                }
                Err(e) => {
                    println!(
                        "  {} data dir not writable ({}): {e}",
                        "✗".red(),
                        dir.display()
                    );
                    ok = false;
                }
            }
        }
        Err(e) => {
            println!("  {} could not resolve data dir: {e}", "✗".red());
            ok = false;
        }
    }

    // 3. Facts-store path openable. We don't actually open redb here — just
    // verify the parent directory exists / is creatable.
    let facts_parent = facts_path.parent().unwrap_or(Path::new("."));
    if facts_parent.as_os_str().is_empty() || facts_parent.exists() {
        println!(
            "  {} facts path parent exists: {}",
            "✓".green(),
            facts_path.display()
        );
    } else {
        match std::fs::create_dir_all(facts_parent) {
            Ok(()) => println!(
                "  {} facts path parent created: {}",
                "✓".green(),
                facts_parent.display()
            ),
            Err(e) => {
                println!(
                    "  {} could not create facts path parent {}: {e}",
                    "✗".red(),
                    facts_parent.display()
                );
                ok = false;
            }
        }
    }

    println!();
    if ok {
        println!("{} all checks passed", "✓".green());
        Ok(())
    } else {
        eprintln!("{} one or more checks failed", "✗".red());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a missing PID file is the normal "daemon not running" case and
    /// must not panic.
    /// What: passes a path that doesn't exist and asserts `None`.
    /// Test: this function.
    #[test]
    fn read_pid_handles_missing_file() {
        let tmp = std::env::temp_dir().join("trusty-analyze-no-such-pid");
        let _ = std::fs::remove_file(&tmp);
        assert!(read_pid(&tmp).is_none());
    }

    /// Why: garbage in the PID file should be treated the same as missing.
    /// What: writes "not-a-pid" and asserts `None`.
    /// Test: this function.
    #[test]
    fn read_pid_handles_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        std::fs::write(&path, "not-a-pid\n").unwrap();
        assert!(read_pid(&path).is_none());
    }

    /// Why: a well-formed PID file should round-trip.
    /// What: writes "12345" and asserts `Some(12345)`.
    /// Test: this function.
    #[test]
    fn read_pid_parses_valid_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(read_pid(&path), Some(12345));
    }

    /// Why: probing a definitely-free port must return false quickly.
    /// What: picks an unused port by binding+dropping a listener, then asserts
    /// `port_reachable` is false.
    /// Test: this function.
    #[test]
    fn port_reachable_returns_false_for_free_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!port_reachable(port));
    }
}
