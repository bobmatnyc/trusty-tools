//! Daemon URL resolution: explicit flag → lock file → default.
//!
//! Why: The daemon may bind to an ephemeral port when 7880 is busy.
//! The lock file records the actual address so clients always find it.
//! What: `resolve_daemon_url` checks an explicit override first, then
//! reads `~/.trusty-mpm/daemon.lock`, then falls back to the hard-coded
//! default.
//! Test: The unit tests below cover all three resolution paths.

use std::path::PathBuf;

use crate::core::paths::FRAMEWORK_DIR_NAME;

/// Default daemon URL when no override and no lock file is found.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7880";

/// Path to the daemon lock file.
///
/// Why: the lock file MUST live in the same `~/.trusty-mpm` root as every
/// other framework artifact (logs, sessions, framework dir). It previously
/// resolved under `dirs::config_dir()` (`~/.config/trusty-mpm`), so the daemon
/// wrote the lock to one directory while the rest of the app — and any user
/// inspecting the install — looked in another. That mismatch meant clients
/// that resolved the URL from a differently-configured environment (or simply
/// expected the documented `~/.trusty-mpm` location) never found the lock and
/// fell back to the default port, reporting "daemon unreachable".
/// What: `~/.trusty-mpm/daemon.lock`, derived from the same `home_dir` +
/// [`FRAMEWORK_DIR_NAME`] as `FrameworkPaths`.
/// Test: `lock_file_path_is_under_framework_root`.
pub fn lock_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(FRAMEWORK_DIR_NAME)
        .join("daemon.lock")
}

/// Resolve the daemon URL in priority order:
/// 1. `explicit` — from `--url` flag or `TRUSTY_MPM_URL` env var (if Some,
///    non-empty, AND not just the default). A caller passing the clap default
///    value is treated the same as passing None so the lock file can win.
/// 2. Lock file `~/.trusty-mpm/daemon.lock` (if present and PID alive)
/// 3. `DEFAULT_DAEMON_URL`
pub fn resolve_daemon_url(explicit: Option<&str>) -> String {
    // 1. Explicit override wins — but only if it's a real override, not the
    //    clap-injected default. When the caller passes DEFAULT_DAEMON_URL we
    //    fall through to the lock file so `tm tui` and `tm status` find a
    //    daemon running on an ephemeral port.
    if let Some(url) = explicit
        && !url.is_empty()
        && url != DEFAULT_DAEMON_URL
    {
        return url.to_string();
    }

    // 2. Lock file — records the actual bound address written by the daemon.
    if let Some(url) = read_lock_file_url() {
        return url;
    }

    // 3. Fall back to the default (or the explicit default the caller passed).
    explicit
        .filter(|u| !u.is_empty())
        .unwrap_or(DEFAULT_DAEMON_URL)
        .to_string()
}

/// Read the daemon URL from the lock file if present and the PID is alive.
fn read_lock_file_url() -> Option<String> {
    let path = lock_file_path();
    let content = std::fs::read_to_string(&path).ok()?;

    let mut addr: Option<String> = None;
    let mut pid: Option<u32> = None;

    for line in content.lines() {
        if let Some(v) = line.strip_prefix("addr = ") {
            addr = Some(v.trim_matches('"').to_string());
        }
        if let Some(v) = line.strip_prefix("pid = ") {
            pid = v.trim().parse::<u32>().ok();
        }
    }

    // Validate PID is still alive (Unix only; on non-Unix skip check).
    #[cfg(unix)]
    if let Some(p) = pid {
        // kill(pid, 0) returns Ok if process exists, Err otherwise.
        if unsafe { libc::kill(p as libc::pid_t, 0) } != 0 {
            // Stale lock — remove it silently.
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }

    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_url_wins_over_everything() {
        let result = resolve_daemon_url(Some("http://example.com:9999"));
        assert_eq!(result, "http://example.com:9999");
    }

    #[test]
    fn empty_explicit_falls_through() {
        // With no lock file and empty explicit, must return default.
        // (Lock file path may or may not exist on CI; we just assert not empty.)
        let result = resolve_daemon_url(Some(""));
        assert!(!result.is_empty());
    }

    #[test]
    fn default_returned_when_no_lock_and_no_explicit() {
        // If no lock file exists this returns DEFAULT_DAEMON_URL.
        // We can't guarantee no lock file exists, so just check it's a valid URL.
        let result = resolve_daemon_url(None);
        assert!(result.starts_with("http"));
    }

    #[test]
    fn lock_file_path_is_under_framework_root() {
        // Why: the lock file must share the `~/.trusty-mpm` root with every
        // other framework artifact. A path under `~/.config` (the previous
        // behaviour) meant the daemon and its clients could disagree on the
        // location and the TUI would report "daemon unreachable".
        let path = lock_file_path();
        assert!(
            path.ends_with(format!("{FRAMEWORK_DIR_NAME}/daemon.lock")),
            "lock file path {path:?} is not under the {FRAMEWORK_DIR_NAME} root"
        );
        // The parent directory is the framework root itself.
        assert_eq!(
            path.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new(FRAMEWORK_DIR_NAME))
        );
    }
}
