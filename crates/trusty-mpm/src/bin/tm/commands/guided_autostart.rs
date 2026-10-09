//! Daemon auto-start helper for the guided-default flow.
//!
//! Why: bare `tm` should bring the trusty-mpm daemon up automatically when
//! it is unreachable, so the operator gets the full picker UX without having
//! to run `tm start` first.
//! What: [`ensure_daemon_started`] runs the plan in
//! [`super::guided_autostart_plan`] (#9034): await a running daemon, start
//! the MAIN daemon's launchd job (`com.trusty.mpm` plist, macOS) with
//! `bootstrap`/`kickstart`, or fall back to a detached direct spawn,
//! recording the child PID in a discoverable pidfile. Both paths poll `/health` via lock-file URL resolution
//! for up to 5 s and return the resolved daemon URL on success.
//! Test: `main_daemon_plist_path_uses_main_label`,
//! `main_daemon_managed_by_launchd`,
//! `main_daemon_managed_ignores_supervisor_only_home`, and
//! `autostart_pidfile_roundtrip` cover the pure decision/path helpers;
//! integration coverage lives in the guided-default e2e suite.

use anyhow::Context as _;

/// The launchd label for the MAIN trusty-mpm session-manager daemon (macOS).
///
/// Why: `ensure_daemon_started` must decide whether the *main* daemon — the one
/// bare `tm` talks to — is managed by launchd, so it can nudge it via
/// `launchctl bootstrap` instead of raw-spawning a competing copy. The main
/// daemon registers under `com.trusty.mpm` (its plist runs
/// `trusty-mpm daemon --addr 127.0.0.1:7880`). This is a DIFFERENT launchd job
/// from the optional unattended supervisor (`com.trusty.mpm.supervisor`, which
/// runs `tm supervisor` for auto-resume/observation, see
/// `deploy/supervisor/`). The previous code checked the *supervisor* label
/// here, so the lookup always missed the real daemon and every autostart fell
/// through to a detached raw spawn — orphaning a stray daemon on a random port
/// (#1900).
/// What: the registry's `com.trusty.mpm` — matches the installed main-daemon
/// plist's `<key>Label</key>` entry. #4868: read from the canonical registry
/// rather than restated, since #1900 was itself a label-lookup miss and a
/// second literal is a second thing that can miss again.
/// Test: `main_daemon_plist_path_uses_main_label`,
/// `main_daemon_managed_ignores_supervisor_only_home`.
#[cfg(target_os = "macos")]
pub(crate) const MAIN_DAEMON_PLIST_LABEL: &str = trusty_common::launchd_labels::MPM;

/// Resolve the LaunchAgents plist path for `label` under an explicit home dir.
///
/// Why: taking `home` as a parameter keeps the derivation pure so tests can
/// point it at a temp dir instead of touching the real
/// `~/Library/LaunchAgents/`.
/// What: returns `<home>/Library/LaunchAgents/<label>.plist`.
/// Test: `main_daemon_plist_path_uses_main_label`.
#[cfg(target_os = "macos")]
fn plist_path_in(home: &std::path::Path, label: &str) -> std::path::PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

/// Decide whether the MAIN daemon is managed by launchd, given a home dir.
///
/// Why: this is the exact decision that gates the launchd-nudge vs. raw-spawn
/// branch in `ensure_daemon_started`. Extracting it as a pure function (home
/// dir injected) lets tests verify — with temp dirs — that a
/// `com.trusty.mpm.plist` triggers launchd management while a home containing
/// ONLY the supervisor plist does not (the #1900 regression guard).
/// What: returns `true` iff `<home>/Library/LaunchAgents/com.trusty.mpm.plist`
/// exists.
/// Test: `main_daemon_managed_by_launchd`,
/// `main_daemon_managed_ignores_supervisor_only_home`.
#[cfg(target_os = "macos")]
fn main_daemon_managed_by_launchd_in(home: &std::path::Path) -> bool {
    plist_path_in(home, MAIN_DAEMON_PLIST_LABEL).exists()
}

/// Resolve the discoverable pidfile path for an autostart-spawned daemon.
///
/// Why: the raw detached-spawn fallback (used when launchd cannot help) would
/// otherwise leave a fully-invisible child — if it races the real daemon and
/// gets orphaned it is hard for `tm`'s own tooling to find and kill (#1900).
/// Recording its PID under the framework root makes the stray trackable.
/// What: returns `<root>/autostart-daemon.pid`.
/// Test: `autostart_pidfile_roundtrip`.
fn autostart_pidfile_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("autostart-daemon.pid")
}

/// Record the PID of an autostart-spawned daemon to a discoverable pidfile.
///
/// Why: makes a raw fallback spawn trackable/killable rather than invisible, so
/// a raced/orphaned autostart daemon can be found by `tm`'s own tooling (#1900).
/// What: writes `pid` (decimal) to `autostart_pidfile_path(root)` and returns
/// the path on success. Callers treat failure as non-fatal.
/// Test: `autostart_pidfile_roundtrip`.
fn write_autostart_pidfile(
    root: &std::path::Path,
    pid: u32,
) -> std::io::Result<std::path::PathBuf> {
    let path = autostart_pidfile_path(root);
    std::fs::write(&path, pid.to_string())?;
    Ok(path)
}

/// Remove the autostart pidfile, if present.
///
/// Why: once the daemon is stopped the recorded PID is stale; leaving it would
/// mislead tooling into probing a dead PID. Mirrors `daemon::cleanup_lock_file`,
/// which is where this is wired into the stop path.
/// What: best-effort `remove_file` of `autostart_pidfile_path(root)`.
/// Test: `autostart_pidfile_roundtrip`.
pub(crate) fn remove_autostart_pidfile(root: &std::path::Path) {
    let _ = std::fs::remove_file(autostart_pidfile_path(root));
}

/// Ensure the trusty-mpm daemon is running, starting it if unreachable.
///
/// Why: bare `tm` in the guided-default flow should not require the operator
/// to run `tm start` manually first. This helper transparently starts the
/// daemon so the picker UX appears on every invocation.
/// What: (1) [`super::guided_autostart_plan::prepare_autostart`] (#9034)
/// decides: a launchd job reporting `state = running`, or a lock naming a live
/// daemon pid, is awaited; a loaded-but-stopped job is kickstarted and an
/// unloaded one bootstrapped (macOS, MAIN daemon plist `com.trusty.mpm` only —
/// never the supervisor, #1900); otherwise (2) it spawns the current
/// executable with the `daemon` subcommand in a detached process, recording
/// the child PID in a discoverable pidfile and keeping the `Child`; (3) polls
/// `/health` every 500 ms for up to 5 s via lock-file URL resolution;
/// (4) returns the resolved daemon URL on success or, on timeout, removes any
/// pidfile this call wrote and returns
/// [`super::guided_autostart_plan::autostart_timeout_error`] over
/// [`super::guided_autostart_plan::timeout_evidence`] — a slow daemon (still
/// running) is a [`super::guided_liveness::DaemonAliveUnresponsive`]. The
/// `_url` parameter is intentionally unused: after auto-start the lock file
/// records the actual bound address.
/// Test: the decisions are `prepare_*`, `timeout_evidence_*` and
/// `timeout_error_*` in `guided_autostart_plan_tests.rs`; the pidfile helpers
/// are unit-tested here; detached-spawn and polling paths are covered by the
/// guided-default e2e suite.
pub(crate) async fn ensure_daemon_started(
    client: &reqwest::Client,
    _url: &str,
) -> anyhow::Result<String> {
    use super::guided_autostart_plan::{
        AutostartPlan, autostart_timeout_error, blocked_error, prepare_autostart, timeout_evidence,
    };
    // #9556: fail closed before any launchd kickstart (the host's live daemon)
    // or detached spawn (the host default address) in a sandbox.
    trusty_mpm::core::refuse_daemon_spawn_when_isolated()?;
    let launchd = launchd_target();
    let run = run_launchctl;
    let identify = super::daemon_pid_identity::pid_identity;
    let lock_path = trusty_mpm::core::lock_file_path();
    let plan = prepare_autostart(&run, launchd.as_ref(), &lock_path, &identify);
    // #9034: an unverified live lock pid fails closed — no poll, no spawn.
    if let AutostartPlan::Blocked(evidence) = &plan {
        return Err(blocked_error(evidence));
    }

    // When we take the fallback-spawn path we keep the child and the framework
    // root: the timeout branch below asks the child whether it still runs
    // (#9034) and removes only a pidfile this call actually created.
    let mut spawned: Option<(std::process::Child, std::path::PathBuf)> = None;
    if plan == AutostartPlan::Spawn {
        let root = trusty_mpm::core::paths::FrameworkPaths::default().root;
        std::fs::create_dir_all(&root).context("create framework dir for daemon log")?;
        let log_path = root.join("daemon.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context("open daemon log file")?;
        let log_copy = log_file.try_clone().context("clone log file handle")?;
        let exe = std::env::current_exe().context("resolve current executable path")?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_copy));
        // #8783: own session, so Ctrl-C to `tm`'s foreground group or a group
        // kill aimed at `tm` spares the daemon it auto-started.
        let child = trusty_common::daemon_guard::start_in_new_session(&mut cmd)
            .spawn()
            .context("spawn daemon process")?;
        // Record the child's PID so a raced/orphaned autostart daemon stays
        // discoverable to `tm`'s own tooling (#1900 hardening). Non-fatal.
        if let Err(e) = write_autostart_pidfile(&root, child.id()) {
            eprintln!("tm: warning: could not write autostart pidfile: {e}");
        }
        // Dropping a `Child` never kills it: the daemon outlives this process.
        spawned = Some((child, root));
    }

    // Poll until healthy or timeout (5 s). Re-resolve from the lock file each
    // iteration so we pick up the actual bound address written by the daemon.
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let resolved = trusty_mpm::core::resolve_daemon_url(None);
        if super::daemon::daemon_healthy(client, &resolved).await {
            eprintln!("tm: daemon ready");
            return Ok(resolved);
        }
    }
    // #9034: a spawned child that has not exited is still starting — slow,
    // not down. Only a child that died leaves a stale pidfile to remove.
    let mut still_running = None;
    if let Some((child, root)) = spawned.as_mut() {
        still_running = super::guided_autostart_plan::spawned_still_running(child);
        if still_running.is_none() {
            remove_autostart_pidfile(root);
        }
    }
    Err(autostart_timeout_error(timeout_evidence(
        &plan,
        &run,
        launchd.as_ref(),
        still_running,
    )))
}

/// The MAIN daemon's launchd job, when its plist is installed (macOS).
///
/// Test: the plist gate is `main_daemon_managed_by_launchd`.
#[cfg(target_os = "macos")]
fn launchd_target() -> Option<super::guided_autostart_plan::LaunchdTarget> {
    let home = dirs::home_dir()?;
    if !main_daemon_managed_by_launchd_in(&home) {
        return None;
    }
    // SAFETY: getuid() takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    Some(super::guided_autostart_plan::LaunchdTarget {
        domain: format!("gui/{uid}"),
        label: MAIN_DAEMON_PLIST_LABEL.to_string(),
        plist: plist_path_in(&home, MAIN_DAEMON_PLIST_LABEL),
    })
}

/// No launchd off macOS.
#[cfg(not(target_os = "macos"))]
fn launchd_target() -> Option<super::guided_autostart_plan::LaunchdTarget> {
    None
}

/// The real `launchctl` runner injected into the autostart plan.
///
/// Test: I/O; the plan is tested against a fake runner.
fn run_launchctl(args: &[String]) -> Option<super::guided_autostart_plan::LaunchctlReply> {
    let out = std::process::Command::new("launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(super::guided_autostart_plan::LaunchctlReply {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    })
}

/// Extract the host token from a git remote URL (lowercased input expected).
///
/// Why: `is_github_remote` needs to compare the host portion of the URL, not
/// the whole string, so that `github-duetto` SSH aliases are matched without
/// also matching unrelated hosts that happen to contain `"github"`.
/// What: handles scp-style (`git@HOST:path` → `HOST`), `https://[user@]HOST/…`,
/// and `ssh://[user@]HOST/…`. Strips any trailing `:<port>` from scheme-style
/// URLs so `github.com:443` → `github.com`. Returns the full input when no
/// host can be extracted — the caller's equality checks will then simply fail.
/// Test: `is_github_remote_*` tests in `tests_behavior_c_tests.rs` exercise
/// this indirectly; `github_host_*` tests verify extraction directly.
pub(crate) fn github_host(lower_url: &str) -> &str {
    // scp-style: git@HOST:path  — HOST is between '@' and first ':'
    // (no port in scp-style; the ':' separates host from path)
    if let Some(at) = lower_url.find('@') {
        let after = &lower_url[at + 1..];
        if let Some(col) = after.find(':') {
            return &after[..col];
        }
    }
    // scheme-URL: [scheme://][user@]HOST[:port][/path]
    let without_scheme = lower_url
        .find("://")
        .map(|i| &lower_url[i + 3..])
        .unwrap_or(lower_url);
    let after_userinfo = without_scheme
        .find('@')
        .map(|i| &without_scheme[i + 1..])
        .unwrap_or(without_scheme);
    let host_with_port = after_userinfo.split('/').next().unwrap_or(after_userinfo);
    // Strip optional :<port> suffix (e.g. "github.com:443" → "github.com").
    // Only strip when the suffix is all ASCII digits to avoid mangling IPv6.
    if let Some(colon) = host_with_port.rfind(':') {
        let port_str = &host_with_port[colon + 1..];
        if !port_str.is_empty() && port_str.chars().all(|c| c.is_ascii_digit()) {
            return &host_with_port[..colon];
        }
    }
    host_with_port
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the MAIN daemon plist path is derived from the main-daemon label.
    ///
    /// Why: `ensure_daemon_started` must look up the main daemon
    /// (`com.trusty.mpm`), not the supervisor — the #1900 mismatch made every
    /// autostart miss the real launchd job. Pin the derived filename and dir.
    /// What: asserts the path is `<home>/Library/LaunchAgents/com.trusty.mpm.plist`.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn main_daemon_plist_path_uses_main_label() {
        let home = std::path::Path::new("/tmp/fake-home");
        let path = plist_path_in(home, MAIN_DAEMON_PLIST_LABEL);
        assert_eq!(MAIN_DAEMON_PLIST_LABEL, "com.trusty.mpm");
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert_eq!(
            filename, "com.trusty.mpm.plist",
            "must target the main daemon plist, not the supervisor"
        );
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        assert_eq!(
            parent, "LaunchAgents",
            "plist must live under Library/LaunchAgents/"
        );
    }

    /// Verify a present main-daemon plist makes the launchd gate return true.
    ///
    /// Why: when the real `com.trusty.mpm.plist` is installed, autostart must
    /// route through `launchctl bootstrap` rather than a raw spawn (#1900).
    /// What: creates `com.trusty.mpm.plist` under a temp home and asserts the
    /// pure decision function returns true; asserts false when it is absent.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn main_daemon_managed_by_launchd() {
        let tmp = tempfile::tempdir().expect("create temp home");
        let home = tmp.path();
        // Absent plist → not launchd-managed → spawn path.
        assert!(
            !main_daemon_managed_by_launchd_in(home),
            "empty home must not report launchd management"
        );
        // Install the main-daemon plist.
        let agents = plist_path_in(home, MAIN_DAEMON_PLIST_LABEL);
        std::fs::create_dir_all(agents.parent().expect("agents dir"))
            .expect("create LaunchAgents dir");
        std::fs::write(&agents, "<plist/>").expect("write main plist");
        assert!(
            main_daemon_managed_by_launchd_in(home),
            "installed main-daemon plist must report launchd management"
        );
    }

    /// Verify a supervisor-only home does NOT report the main daemon as managed.
    ///
    /// Why: this is the direct #1900 regression guard — the supervisor plist
    /// (`com.trusty.mpm.supervisor`) is a distinct optional service; its presence
    /// must never be mistaken for the main daemon being launchd-managed.
    /// What: writes only `com.trusty.mpm.supervisor.plist` and asserts the gate
    /// returns false so autostart correctly falls through to the spawn path.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn main_daemon_managed_ignores_supervisor_only_home() {
        let tmp = tempfile::tempdir().expect("create temp home");
        let home = tmp.path();
        let supervisor = plist_path_in(home, "com.trusty.mpm.supervisor");
        std::fs::create_dir_all(supervisor.parent().expect("agents dir"))
            .expect("create LaunchAgents dir");
        std::fs::write(&supervisor, "<plist/>").expect("write supervisor plist");
        assert!(
            !main_daemon_managed_by_launchd_in(home),
            "a supervisor-only home must not report the main daemon as managed (#1900)"
        );
    }

    /// Verify the autostart pidfile is written, read back, and removed.
    ///
    /// Why: the fallback-spawn hardening records the child PID so a raced/
    /// orphaned autostart daemon is discoverable and killable (#1900). Round-trip
    /// the write and the cleanup that `daemon::cleanup_lock_file` invokes on stop.
    /// What: writes a PID under a temp root, asserts the file contains that PID,
    /// then removes it and asserts it is gone.
    /// Test: this test.
    #[test]
    fn autostart_pidfile_roundtrip() {
        let tmp = tempfile::tempdir().expect("create temp root");
        let root = tmp.path();
        let path = write_autostart_pidfile(root, 424242).expect("write pidfile");
        assert_eq!(path, autostart_pidfile_path(root));
        let contents = std::fs::read_to_string(&path).expect("read pidfile");
        assert_eq!(contents, "424242", "pidfile must contain the child PID");
        remove_autostart_pidfile(root);
        assert!(
            !path.exists(),
            "remove_autostart_pidfile must delete the pidfile"
        );
    }

    /// Verify github_host strips a port suffix from scheme-style URLs.
    ///
    /// Why: `https://github.com:443/o/r.git` is a valid remote URL but the
    /// old code returned `"github.com:443"`, causing `is_github_remote` to
    /// return false — a regression vs the previous substring-match approach.
    /// What: asserts `github_host` strips `":443"` so `is_github_remote` fires.
    /// Test: this test; regression guard for the port-stripping fix.
    #[test]
    fn github_host_strips_port_from_https_url() {
        assert_eq!(
            github_host("https://github.com:443/owner/repo.git"),
            "github.com",
            "port suffix must be stripped from https:// URLs"
        );
        assert_eq!(
            github_host("ssh://git@github-work:2222/o/r.git"),
            "github-work",
            "port suffix must be stripped from ssh:// URLs with userinfo"
        );
        // scp-style has no port — unchanged
        assert_eq!(
            github_host("git@github.com:owner/repo.git"),
            "github.com",
            "scp-style host must be unaffected"
        );
    }
}
