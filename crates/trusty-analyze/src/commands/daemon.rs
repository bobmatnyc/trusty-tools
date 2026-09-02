//! Handlers for `start`, `stop`, `status`, and `doctor`.
//!
//! Why: gives users a familiar `start/stop/status/doctor` lifecycle for the
//! analyzer daemon without forcing them to learn launchd/systemd. Mirrors the
//! UX of `trusty-search` and `trusty-memory`.
//! What: spawns the binary itself in the background (writing a PID file under
//! `~/.trusty-analyze/`), sends SIGTERM on stop, probes the daemon's Unix
//! socket for status, and runs a small battery of sanity checks for doctor.
//!
//! #6287: every `port: u16` parameter became a `socket: &Path`, and the TCP
//! connect became `trusty_common::uds::socket_is_serving`. `status` reads the
//! version through an `analyze.health` call rather than a `GET /health`.
//!
//! Test: integration coverage lives in tests against the binary; this module
//! is exercised manually via `trusty-analyze start` / `stop` / `status` /
//! `doctor` and via the unit tests below.

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

/// How long a liveness connect may take before the socket is called dead.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Probe whether anything is serving the daemon's socket.
///
/// Why: a bare connect is the lightest-weight signal that the daemon is up, and
/// it does not require the daemon's dependencies to be healthy — a degraded
/// daemon is still a running one, which `stop` and `status` both need to know.
/// What: delegates to the shared [`trusty_common::uds::socket_is_serving`].
///
/// Synchronous, because `handle_start` and `handle_stop` are — and the runtime
/// it needs runs on a DEDICATED THREAD rather than through `Handle::block_on`.
/// `main` is `#[tokio::main]`, so every caller here is already inside a runtime,
/// and building a second one on that thread and blocking on it panics. Doing it
/// on its own thread makes the call safe from any caller regardless of what it
/// is running on — the same reason `trusty-console`'s `probe_health` and
/// `trusty-installer`'s `probe_member_http_blocking` are written this way.
/// Test: `socket_serving_returns_false_for_an_absent_socket`.
fn socket_serving(socket: &Path) -> bool {
    let socket = socket.to_path_buf();
    std::thread::Builder::new()
        .name("analyze-socket-probe".to_owned())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return false;
            };
            rt.block_on(trusty_common::uds::socket_is_serving(
                &socket,
                PROBE_TIMEOUT,
            ))
        })
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or(false)
}

/// The exact argument vector `start` hands the child.
///
/// Why: a bare `serve` is the whole contract — the child derives its socket
/// from the data directory, so passing `--socket` would start a daemon on a
/// path this parent never probed. Building it in a pure function is what lets
/// a test assert that without spawning anything, the same way `tga`'s
/// `audit::analyze::serve_args` does.
/// Test: `start_spawns_a_bare_serve_so_the_child_derives_the_same_socket`.
fn serve_args() -> [&'static str; 1] {
    ["serve"]
}

/// Spawn the daemon in the background and write its PID.
///
/// Why: gives users a one-command "boot the daemon" path without forcing them
/// onto launchd. Background detach is done by spawning `serve` on the same
/// executable and immediately returning.
///
/// #6287: this took a `socket: &Path` and ignored it when spawning, because the
/// child derives its own path. A caller that passed anything but the derived
/// default would have probed one socket and served another, and the "started"
/// line would have named the socket that was probed rather than the one now
/// being served. The parameter is gone rather than forwarded: there is one
/// correct path, and resolving it here is what makes the probe, the spawn and
/// the message read the same value. [`handle_stop`], [`handle_status`] and
/// [`handle_doctor`] keep theirs — they only observe a socket, never start one.
///
/// What: resolves the derived socket, exits early if a PID file names a live
/// daemon answering on it, otherwise spawns the current exe with
/// [`serve_args`], writes the child PID to `~/.trusty-analyze/daemon.pid`, and
/// prints that same path.
///
/// # Errors
///
/// When the socket path or the data directory cannot be resolved, or the spawn
/// fails.
///
/// Test: `start_spawns_a_bare_serve_so_the_child_derives_the_same_socket`; run
/// `trusty-analyze start` twice — the second reports "already running", exit 0.
pub fn handle_start() -> Result<()> {
    // The one resolution this function acts on: probed below, served by the
    // child through the same derivation, and printed at the end.
    let socket = trusty_analyze::service::socket_path()?;
    let socket = socket.as_path();
    let pid_path = pid_file_path()?;
    if let Some(pid) = read_pid(&pid_path) {
        if socket_serving(socket) {
            println!(
                "{} trusty-analyze already running (pid {pid}, socket {})",
                "✓".green(),
                socket.display()
            );
            return Ok(());
        }
        // Stale PID file — remove and continue.
        let _ = std::fs::remove_file(&pid_path);
    }

    // #6287: no `--socket` is passed — see `serve_args`.
    let exe = std::env::current_exe().context("resolve current executable")?;
    let child = std::process::Command::new(&exe)
        .args(serve_args())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {} serve", exe.display()))?;

    std::fs::write(&pid_path, child.id().to_string())
        .with_context(|| format!("write pid file {}", pid_path.display()))?;

    println!(
        "{} trusty-analyze started (pid {}, socket {})",
        "✓".green(),
        child.id(),
        socket.display()
    );
    Ok(())
}

/// Stop the running daemon by sending SIGTERM to the recorded PID.
///
/// Why: pairs with `start` — a single command users can run to tear the
/// background daemon down cleanly.
/// What: reads the PID file, invokes `kill -TERM`, polls up to 5 s for the
/// socket to stop answering, then removes the PID file.
/// Test: with a running daemon → "stopped" message within a few seconds.
pub fn handle_stop(socket: &Path) -> Result<()> {
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
        if !socket_serving(socket) {
            let _ = std::fs::remove_file(&pid_path);
            println!("{} trusty-analyze stopped", "✓".green());
            return Ok(());
        }
    }
    println!(
        "{} Daemon is still answering {} after 5 s; PID file left in place",
        "⚠".yellow(),
        socket.display()
    );
    Ok(())
}

/// Show daemon status: running/down, port, version when reachable.
///
/// Why: `health` already reports trusty-search status; this command focuses
/// on the analyzer itself with more detail (PID, version) for the user.
/// What: probes the socket, reads the PID file, and calls `analyze.health` for
/// the version string if the daemon answers.
/// Test: with the daemon down, prints "DOWN" and exits 0 (informational).
pub async fn handle_status(socket: &Path) -> Result<()> {
    let pid_path = pid_file_path()?;
    let pid = read_pid(&pid_path);
    let reachable = trusty_common::uds::socket_is_serving(socket, PROBE_TIMEOUT).await;

    if reachable {
        println!("{} trusty-analyze: {}", "✓".green(), "RUNNING".green());
    } else {
        println!("{} trusty-analyze: {}", "✗".red(), "DOWN".red());
    }
    println!("  Socket:   {}", socket.display());
    if let Some(pid) = pid {
        println!("  PID:      {pid} (from {})", pid_path.display());
    } else {
        println!("  PID:      {}", "<no pid file>".dimmed());
    }

    if reachable {
        match health_envelope(socket).await {
            Ok(body) => {
                if let Some(v) = body.get("version").and_then(|v| v.as_str()) {
                    println!("  Version:  {v}");
                }
                // #6287: `analyze.health` answers with a RESULT frame even when
                // trusty-search is down, so a degraded daemon reports its
                // dependency here instead of looking like a failed probe.
                if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                    if status != "ok" {
                        println!(
                            "  Health:   {} (trusty-search unreachable)",
                            status.yellow()
                        );
                    }
                }
            }
            Err(e) => println!("  Health:   probe failed: {e:#}"),
        }
    }
    Ok(())
}

/// One `analyze.health` call, returning the raw `result` value.
///
/// Why: `status` and `doctor` both want the version off a live daemon, and both
/// need the same "answered but with an error" handling.
///
/// # Errors
///
/// When the socket cannot be dialled, or the daemon answers with an error frame.
async fn health_envelope(socket: &Path) -> Result<serde_json::Value> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": trusty_analyze::service::METHOD_HEALTH,
    });
    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request(socket, &request, Duration::from_secs(5))
            .await
            .with_context(|| format!("analyze.health over {}", socket.display()))?;
    if let Some(error) = response.error {
        anyhow::bail!("analyze.health returned {}: {}", error.code, error.message);
    }
    response
        .result
        .ok_or_else(|| anyhow::anyhow!("analyze.health answered with neither result nor error"))
}

/// Diagnose common configuration issues and print a ✓/✗ summary.
///
/// Why: gives users a fast self-service path to "why isn't this working?"
/// without needing to read tracing logs.
/// What: checks (1) something is serving the daemon socket, (2) data dir
/// exists and is writable, (3) the facts-store path is openable.
/// Test: run `trusty-analyze doctor` with the daemon down — should print the
/// missing-daemon ✗ line and exit non-zero.
pub async fn handle_doctor(socket: &Path, facts_path: &Path) -> Result<()> {
    let mut ok = true;
    println!("trusty-analyze doctor:");

    // 1. Daemon reachability.
    if trusty_common::uds::socket_is_serving(socket, PROBE_TIMEOUT).await {
        println!("  {} daemon serving {}", "✓".green(), socket.display());
    } else {
        println!(
            "  {} nothing is serving {} (start it with `trusty-analyze start`)",
            "✗".red(),
            socket.display()
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

    // 4. A retired LaunchAgent plist left behind by a pre-#6350 install.
    for line in stale_unit_warnings() {
        println!("  {} {line}", "!".yellow());
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

/// Warn about each retired LaunchAgent plist still on disk (#6621).
///
/// Why: #6350 retired this daemon's LaunchAgent, and `service uninstall` —
/// which `tctl install`/`upgrade` also run unattended — unloads and DELETES it.
/// A host that has run neither still carries `~/Library/LaunchAgents/
/// com.trusty.analyze.plist`, and that plist declares `KeepAlive: true`: the
/// next `launchctl load`, or the next login on a host where it is still
/// referenced, restarts a resident daemon that fights the idle exit. Nothing
/// told the operator it was there, so `doctor` does.
///
/// What: a WARN per surviving plist, never a deletion and never a failure. This
/// function observes; `service uninstall` is the one path that removes, and the
/// warning names it. A plist path that cannot be resolved yields no line —
/// there is nothing to report about a home directory that does not resolve, and
/// the data-dir check above already fails on that.
///
/// Test: `stale_unit_warnings_name_the_retired_labels`,
/// `stale_unit_warning_points_at_the_uninstall_command`.
#[cfg(target_os = "macos")]
fn stale_unit_warnings() -> Vec<String> {
    trusty_common::launchd_labels::retired_labels_for_member(RETIRED_MEMBER)
        .into_iter()
        .filter_map(|label| {
            let path = trusty_common::launchd::user_plist_path(label).ok()?;
            path.exists().then(|| stale_unit_warning(&path))
        })
        .collect()
}

/// Nothing to warn about off macOS — there are no LaunchAgents there.
#[cfg(not(target_os = "macos"))]
fn stale_unit_warnings() -> Vec<String> {
    Vec::new()
}

/// The member whose retired labels [`stale_unit_warnings`] looks for.
const RETIRED_MEMBER: &str = "trusty-analyze";

/// The warning text for one surviving plist.
///
/// Pure, so the message contract is asserted without a home directory that
/// happens to have one — the filesystem walk above is what varies per host.
fn stale_unit_warning(path: &Path) -> String {
    format!(
        "a retired LaunchAgent is still installed: {} \
         (it declares KeepAlive and would fight the on-demand idle exit — \
         clear it with `trusty-analyze service uninstall`)",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6621): the issue's host still carried
    /// `~/Library/LaunchAgents/com.trusty.analyze.plist` from a pre-#6350
    /// install — `KeepAlive: true`, not currently loaded, and nothing told the
    /// operator it was there. The check is only as good as the labels it looks
    /// for, so this is what says it looks for the right ones.
    /// Test: this is the test.
    #[test]
    fn stale_unit_warnings_name_the_retired_labels() {
        let labels = trusty_common::launchd_labels::retired_labels_for_member(RETIRED_MEMBER);
        assert!(
            labels.contains(&"com.trusty.analyze"),
            "the pre-#6350 label must be one doctor looks for: {labels:?}"
        );
    }

    /// Why: `doctor` observes and never deletes — a check that removed a plist
    /// would be doing `service uninstall`'s job behind an operator who typed a
    /// read-only command. The message has to hand that job back.
    /// Test: this is the test.
    #[test]
    fn stale_unit_warning_points_at_the_uninstall_command() {
        let warning = stale_unit_warning(Path::new(
            "/Users/x/Library/LaunchAgents/com.trusty.analyze.plist",
        ));
        assert!(warning.contains("com.trusty.analyze.plist"), "{warning}");
        assert!(
            warning.contains("trusty-analyze service uninstall"),
            "the warning must name the command that clears it: {warning}"
        );
    }

    /// REGRESSION (#6287): `start` must hand the child a bare `serve`.
    ///
    /// Why: the parent resolves the socket, probes it, and prints it; the child
    /// re-derives the same path from the data directory. A `--socket` in this
    /// argv would break that agreement silently — the parent would report one
    /// path while the daemon served another, and every consumer dials the
    /// derived one. That is the shape `handle_start` dropping its parameter
    /// exists to prevent, and this is what keeps the spawn honest.
    /// Test: this is the test.
    #[test]
    fn start_spawns_a_bare_serve_so_the_child_derives_the_same_socket() {
        assert_eq!(serve_args(), ["serve"]);
        assert!(
            !serve_args().iter().any(|a| a.contains("--socket")),
            "a socket override would serve a path the parent never probed: {:?}",
            serve_args()
        );
    }

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

    /// Why (#6287): probing a path nothing binds must return false quickly, and
    /// must do so from inside a tokio runtime — `main` is `#[tokio::main]`, so
    /// every real caller is. A `Handle::block_on` implementation would panic
    /// here rather than answer.
    /// What: calls `socket_serving` from an async test against an absent path.
    /// Test: this function.
    #[tokio::test]
    async fn socket_serving_returns_false_for_an_absent_socket() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!socket_serving(&tmp.path().join("absent.sock")));
    }
}
