//! Daemon auto-start helper for the guided-default flow.
//!
//! Why: bare `tm` should bring the trusty-mpm daemon up automatically when
//! it is unreachable, so the operator gets the full picker UX without having
//! to run `tm start` first.
//! What: [`ensure_daemon_started`] checks for a launchd supervisor plist
//! (preferred: reboot-durable) and falls back to a detached direct spawn.
//! Both paths poll `/health` via lock-file URL resolution for up to 5 s and
//! return the resolved daemon URL on success.
//! Test: [`supervisor_plist_path_has_expected_components`] verifies the path
//! derivation; integration coverage lives in the guided-default e2e suite.

use anyhow::Context as _;

/// The launchd label for the trusty-mpm supervisor plist.
///
/// Why: the identical string appears as `PLIST_LABEL` in `trusty-installer`;
/// defining it here avoids importing that crate (which would create a circular
/// dependency). It MUST match the installer's constant exactly so that
/// `try_launchctl_start` and the installer target the same plist.
/// What: `"com.trusty.mpm.supervisor"`.
/// Test: `supervisor_plist_path_has_expected_components` derives the filename
/// from this constant and asserts it matches the expected plist basename.
pub(crate) const SUPERVISOR_PLIST_LABEL: &str = "com.trusty.mpm.supervisor";

/// Resolve the expected macOS LaunchAgents path for the supervisor plist.
///
/// Why: factored out so `ensure_daemon_started` can check whether a supervisor
/// plist is installed before deciding between launchd-start and detached-spawn.
/// What: returns `~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist`.
/// Test: `supervisor_plist_path_has_expected_components`.
pub(crate) fn supervisor_plist_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SUPERVISOR_PLIST_LABEL}.plist"))
}

/// Ensure the trusty-mpm daemon is running, starting it if unreachable.
///
/// Why: bare `tm` in the guided-default flow should not require the operator
/// to run `tm start` manually first. This helper transparently starts the
/// daemon so the picker UX appears on every invocation.
/// What: (1) on macOS, if the supervisor plist exists at
/// `~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist`, issues
/// `launchctl bootstrap gui/<uid> <plist>` for reboot-durable startup;
/// (2) otherwise removes any stale lock file and spawns the current executable
/// with the `daemon` subcommand in a detached process (same logic as
/// `commands::daemon::start`); (3) polls `/health` every 500 ms for up to 5 s
/// via lock-file URL resolution; (4) returns the resolved daemon URL on success
/// or `Err` on timeout. The `_url` parameter is intentionally unused: after
/// auto-start the lock file records the actual bound address, which may differ
/// from the stale or default URL the caller held.
/// Test: launchd path requires a macOS launchd environment; detached-spawn and
/// polling paths are covered by the guided-default e2e smoke test.
pub(crate) async fn ensure_daemon_started(
    client: &reqwest::Client,
    _url: &str,
) -> anyhow::Result<String> {
    let plist_path = supervisor_plist_path();
    if !try_launchctl_start(&plist_path) {
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
        std::process::Command::new(&exe)
            .arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_copy))
            .spawn()
            .context("spawn daemon process")?;
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
    anyhow::bail!("daemon did not become healthy within 5 s after auto-start")
}

/// Attempt to start the daemon via launchd `bootstrap` (macOS only).
///
/// Why: `launchctl bootstrap` is preferred over a bare process spawn because
/// launchd registers the unit for reboot-durability and respawn-on-crash. The
/// detached-spawn fallback is session-only (no reboot durability).
/// What: on macOS, if `plist_path` exists, resolves the current user UID via
/// `id -u`, then runs `launchctl bootstrap gui/<uid> <plist>`. Returns `true`
/// when the command exits 0. Returns `false` on non-macOS, when the plist is
/// absent, when `id -u` fails, or when `launchctl` exits non-zero (e.g. already
/// loaded — the caller falls through to the polling wait either way).
/// Test: requires a real macOS launchd environment; covered by e2e smoke test.
#[cfg(target_os = "macos")]
fn try_launchctl_start(plist_path: &std::path::Path) -> bool {
    if !plist_path.exists() {
        return false;
    }
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if uid.is_empty() {
        return false;
    }
    let domain = format!("gui/{uid}");
    let plist_str = match plist_path.to_str() {
        Some(s) => s,
        None => return false,
    };
    std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, plist_str])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn try_launchctl_start(_plist_path: &std::path::Path) -> bool {
    false
}

/// Extract the host token from a git remote URL (lowercased input expected).
///
/// Why: `is_github_remote` needs to compare the host portion of the URL, not
/// the whole string, so that `github-duetto` SSH aliases are matched without
/// also matching unrelated hosts that happen to contain `"github"`.
/// What: handles scp-style (`git@HOST:path` → `HOST`), `https://[user@]HOST/…`,
/// and `ssh://[user@]HOST/…`. Returns the full input when no host can be
/// extracted — the caller's equality checks will then simply fail safely.
/// Test: `is_github_remote_*` tests in `tests_behavior_c_tests.rs` exercise
/// this indirectly; `github_host_*` tests verify extraction directly.
pub(crate) fn github_host(lower_url: &str) -> &str {
    // scp-style: git@HOST:path  — HOST is between '@' and ':'
    if let Some(at) = lower_url.find('@') {
        let after = &lower_url[at + 1..];
        if let Some(col) = after.find(':') {
            return &after[..col];
        }
    }
    // scheme-URL: [scheme://][user@]HOST[/path]
    let without_scheme = lower_url
        .find("://")
        .map(|i| &lower_url[i + 3..])
        .unwrap_or(lower_url);
    let after_userinfo = without_scheme
        .find('@')
        .map(|i| &without_scheme[i + 1..])
        .unwrap_or(without_scheme);
    after_userinfo.split('/').next().unwrap_or(after_userinfo)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the supervisor plist path is derived correctly from the label constant.
    ///
    /// Why: the path must match what `launchctl bootstrap` and the installer
    /// both expect so they all refer to the same file.
    /// What: asserts the filename is `<LABEL>.plist` and the parent dir is
    /// `LaunchAgents`.
    /// Test: this test.
    #[test]
    fn supervisor_plist_path_has_expected_components() {
        let path = supervisor_plist_path();
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert_eq!(
            filename,
            format!("{SUPERVISOR_PLIST_LABEL}.plist"),
            "plist filename must be <LABEL>.plist"
        );
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        assert_eq!(
            parent, "LaunchAgents",
            "plist must live in ~/Library/LaunchAgents/"
        );
    }
}
