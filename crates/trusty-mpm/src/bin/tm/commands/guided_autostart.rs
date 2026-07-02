//! Daemon auto-start helper for the guided-default flow.
//!
//! Why: bare `tm` should bring the trusty-mpm daemon up automatically when
//! it is unreachable, so the operator gets the full picker UX without having
//! to run `tm start` first.
//! What: [`ensure_daemon_started`] checks whether the MAIN session-manager
//! daemon is managed by launchd on macOS (via its `com.trusty.mpm` plist) and,
//! if so, nudges it with `launchctl bootstrap` (reboot-durable); otherwise it
//! falls back to a detached direct spawn, recording the child PID in a
//! discoverable pidfile. Both paths poll `/health` via lock-file URL resolution
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
/// What: `"com.trusty.mpm"` — matches the installed main-daemon plist's
/// `<key>Label</key>` entry.
/// Test: `main_daemon_plist_path_uses_main_label`,
/// `main_daemon_managed_ignores_supervisor_only_home`.
#[cfg(target_os = "macos")]
pub(crate) const MAIN_DAEMON_PLIST_LABEL: &str = "com.trusty.mpm";

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

/// Outcome of a `launchctl bootstrap` attempt (macOS only).
///
/// Why: distinguishes "already running" (EALREADY) from "truly unavailable" so
/// `ensure_daemon_started` can skip the lock-delete+spawn step when launchd
/// already owns the service, preventing a double-start race.
/// What: three variants — `Launched` (exit 0), `AlreadyLoaded` (exit 37 /
/// EALREADY or matching stderr), `Unavailable` (plist absent, `id -u` failed,
/// or unknown non-zero exit).
/// Test: covered indirectly by the launchd e2e smoke test.
#[cfg(target_os = "macos")]
enum LaunchctlOutcome {
    /// launchd accepted the bootstrap command — daemon is now (re)starting.
    Launched,
    /// launchd reports the service is already bootstrapped (exit 37 / EALREADY).
    /// No second spawn is needed — proceed straight to the health-poll loop.
    AlreadyLoaded,
    /// launchctl is not available, the plist is absent, or bootstrap failed for
    /// an unknown reason. Fall back to a detached direct spawn.
    Unavailable,
}

/// Ensure the trusty-mpm daemon is running, starting it if unreachable.
///
/// Why: bare `tm` in the guided-default flow should not require the operator
/// to run `tm start` manually first. This helper transparently starts the
/// daemon so the picker UX appears on every invocation.
/// What: (1) on macOS, if the MAIN daemon plist (`com.trusty.mpm`) is installed,
/// issues `launchctl bootstrap gui/<uid> <plist>` for reboot-durable startup and
/// skips the spawn step when the service is already loaded (EALREADY) — the
/// check targets the main daemon, NOT the optional supervisor (#1900);
/// (2) when launchd is unavailable, the main-daemon plist is absent, or on
/// non-macOS, removes any stale lock file and spawns the current executable with
/// the `daemon` subcommand in a detached process (recording the child PID in a
/// discoverable pidfile); (3) polls `/health` every 500 ms for up to 5 s via
/// lock-file URL resolution; (4) returns the resolved daemon URL on success or,
/// on timeout, removes any pidfile written by this call's fallback spawn (so a
/// failed autostart leaves no stale PID) and returns `Err`. The `_url` parameter
/// is intentionally unused: after auto-start the lock file records the actual
/// bound address.
/// Test: launchd path requires a macOS launchd environment; the pure gate
/// (`main_daemon_managed_by_launchd_in`) and pidfile helpers are unit-tested;
/// detached-spawn and polling paths are covered by the guided-default e2e suite.
pub(crate) async fn ensure_daemon_started(
    client: &reqwest::Client,
    _url: &str,
) -> anyhow::Result<String> {
    // Determine whether we need to spawn a new daemon process.
    // On macOS: if the MAIN daemon plist is installed, let launchd own it and
    // nudge it via bootstrap; spawn only when launchctl truly cannot help.
    // On all other platforms: always spawn directly.
    #[cfg(target_os = "macos")]
    let needs_spawn = {
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        if main_daemon_managed_by_launchd_in(&home) {
            let plist_path = plist_path_in(&home, MAIN_DAEMON_PLIST_LABEL);
            matches!(
                try_launchctl_start(&plist_path),
                LaunchctlOutcome::Unavailable
            )
        } else {
            // No main-daemon plist installed → launchd cannot help; spawn.
            true
        }
    };
    #[cfg(not(target_os = "macos"))]
    let needs_spawn = true;

    // When we take the fallback-spawn path we record the framework root so the
    // health-poll timeout branch below can clean up the pidfile we wrote. On the
    // launchd path (or when no spawn happened) this stays `None` and no pidfile
    // is touched — we only ever remove a pidfile this call actually created.
    let mut spawned_root: Option<std::path::PathBuf> = None;
    if needs_spawn {
        // launchd not available or plist absent — fall back to detached spawn,
        // the same approach used by `commands::daemon::start`.
        let lock_path = trusty_mpm::core::lock_file_path();
        let _ = std::fs::remove_file(&lock_path);
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
        let child = std::process::Command::new(&exe)
            .arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_copy))
            .spawn()
            .context("spawn daemon process")?;
        // Record the child's PID so a raced/orphaned autostart daemon stays
        // discoverable to `tm`'s own tooling (#1900 hardening). Non-fatal.
        if let Err(e) = write_autostart_pidfile(&root, child.id()) {
            eprintln!("tm: warning: could not write autostart pidfile: {e}");
        }
        spawned_root = Some(root);
        // `child` is intentionally dropped here: the daemon is meant to outlive
        // this process (on Unix it re-parents to init on our exit), so we do not
        // wait on or kill it — dropping the handle just detaches from it.
        drop(child);
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
    // Health poll timed out. If we spawned a fallback daemon, the pidfile we
    // wrote now points at a PID that never became reachable — the spawned child
    // may have died immediately or bound an unreachable address. The single-file
    // pidfile is overwritten per spawn, so this does not *accumulate* entries,
    // but leaving it behind hands `tm`'s tooling a stale PID that no longer
    // corresponds to a healthy daemon. Remove it here (mirrors the stop-path
    // cleanup in `daemon::cleanup_lock_file`) so a failed autostart leaves no
    // misleading pidfile. Best-effort; ignore removal errors.
    if let Some(root) = spawned_root.as_deref() {
        remove_autostart_pidfile(root);
    }
    anyhow::bail!("daemon did not become healthy within 5 s after auto-start")
}

/// Attempt to start the daemon via launchd `bootstrap` (macOS only).
///
/// Why: `launchctl bootstrap` is preferred over a bare process spawn because
/// launchd registers the unit for reboot-durability and respawn-on-crash. The
/// detached-spawn fallback is session-only (no reboot durability).
/// What: if `plist_path` exists, resolves the current user UID via `id -u`,
/// then runs `launchctl bootstrap gui/<uid> <plist>`. Returns `Launched` on
/// exit 0, `AlreadyLoaded` on exit 37 (EALREADY) or when stderr contains
/// "already bootstrapped"/"service already loaded", and `Unavailable` in all
/// other cases (plist absent, `id -u` failure, unknown launchctl error).
/// Test: requires a real macOS launchd environment; covered by e2e smoke test.
#[cfg(target_os = "macos")]
fn try_launchctl_start(plist_path: &std::path::Path) -> LaunchctlOutcome {
    if !plist_path.exists() {
        return LaunchctlOutcome::Unavailable;
    }
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if uid.is_empty() {
        return LaunchctlOutcome::Unavailable;
    }
    let domain = format!("gui/{uid}");
    let plist_str = match plist_path.to_str() {
        Some(s) => s,
        None => return LaunchctlOutcome::Unavailable,
    };
    let output = match std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, plist_str])
        .output()
    {
        Ok(o) => o,
        Err(_) => return LaunchctlOutcome::Unavailable,
    };
    if output.status.success() {
        return LaunchctlOutcome::Launched;
    }
    // exit 37 = EALREADY; some launchctl versions surface this in stderr instead.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(37)
        || stderr.contains("already bootstrapped")
        || stderr.contains("service already loaded")
    {
        return LaunchctlOutcome::AlreadyLoaded;
    }
    LaunchctlOutcome::Unavailable
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
