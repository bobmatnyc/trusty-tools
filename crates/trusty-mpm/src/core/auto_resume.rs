//! Persisted operator desired-state for supervisor auto-resume (#1222 / RFC Q6).
//!
//! Why: `TRUSTY_MPM_AUTO_RESUME` is a *process* env var read by the supervisor
//! at startup. The supervisor runs as a separate launchd-managed process, so the
//! daemon (and therefore the console, which speaks MCP to the daemon) cannot
//! mutate the supervisor's live environment. To give the console a real, non-CLI
//! control over auto-resume (RFC §6 Q6 — "the console SHALL provide controls to
//! enable/disable auto-resume"), we persist the operator's *desired* flag to a
//! tiny file under the framework root. The supervisor reads this file on each
//! sweep (wiring tracked separately), and `supervisor_status` surfaces it so the
//! console can render the toggle's true state.
//! What: [`AutoResumeState`] is the persisted flag; [`read_desired`] /
//! [`write_desired`] read/write `~/.trusty-mpm/auto_resume` (a one-line `true` /
//! `false`); [`effective_from_env`] reports the env-derived flag the supervisor
//! process actually booted with. A missing file means "no operator override" and
//! reads as `false` (the safe default — auto-resume is opt-in).
//! Test: `read_missing_is_false`, `write_then_read_round_trips`,
//! `effective_from_env_parses_truthy` in the `tests` module.

use std::path::{Path, PathBuf};

use crate::core::paths::FrameworkPaths;
use crate::supervisor::config::ENV_AUTO_RESUME;

/// Filename (under the framework root) holding the persisted desired flag.
///
/// Why: a single named constant keeps the console, the daemon, and the
/// supervisor agreeing on one location without drift.
/// What: `auto_resume` — a plain-text file containing `true` or `false`.
/// Test: `desired_path_is_under_root`.
pub const AUTO_RESUME_FILE: &str = "auto_resume";

/// Resolve the path of the persisted desired-state file.
///
/// Why: centralising the path keeps every reader/writer consistent and lets
/// tests point it at a temp root.
/// What: `<root>/auto_resume` derived from the given [`FrameworkPaths`].
/// Test: `desired_path_is_under_root`.
pub fn desired_path(paths: &FrameworkPaths) -> PathBuf {
    paths.root.join(AUTO_RESUME_FILE)
}

/// Read the persisted desired flag from an explicit path.
///
/// Why: separating the path-taking core from the home-resolving wrapper keeps
/// the file logic hermetically testable.
/// What: returns `Ok(true)` only when the file's trimmed contents are a truthy
/// token (`1`, `true`, `yes`, `on`, case-insensitive); a missing file is
/// `Ok(false)` (no override). I/O errors other than not-found propagate.
/// Test: `read_missing_is_false`, `write_then_read_round_trips`.
pub fn read_desired_at(path: &Path) -> std::io::Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(parse_truthy(contents.trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Write the persisted desired flag to an explicit path, creating parents.
///
/// Why: the console toggle must durably record the operator's choice so it
/// survives daemon restarts and is visible to the supervisor.
/// What: ensures the parent directory exists, then writes `true` or `false` plus
/// a trailing newline.
/// Test: `write_then_read_round_trips`.
pub fn write_desired_at(path: &Path, enabled: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, if enabled { "true\n" } else { "false\n" })
}

/// Read the persisted desired flag from the default framework root.
///
/// Why: production callers want `~/.trusty-mpm/auto_resume` without resolving
/// the home directory themselves.
/// What: resolves [`FrameworkPaths::default`] and reads `desired_path`.
/// Test: covered indirectly via `read_desired_at`.
pub fn read_desired() -> std::io::Result<bool> {
    read_desired_at(&desired_path(&FrameworkPaths::default()))
}

/// Write the persisted desired flag to the default framework root.
///
/// Why: the console's auto-resume toggle calls this through the MCP backend.
/// What: resolves [`FrameworkPaths::default`] and writes `desired_path`.
/// Test: covered indirectly via `write_desired_at`.
pub fn write_desired(enabled: bool) -> std::io::Result<()> {
    write_desired_at(&desired_path(&FrameworkPaths::default()), enabled)
}

/// Report the auto-resume flag the supervisor process booted with.
///
/// Why: the console shows both the persisted desire and the live env flag so the
/// operator can tell when a restart is required for the change to take effect.
/// What: parses `TRUSTY_MPM_AUTO_RESUME` from the process environment as a
/// truthy token; absent or non-truthy is `false`.
/// Test: `effective_from_env_parses_truthy`.
pub fn effective_from_env() -> bool {
    std::env::var(ENV_AUTO_RESUME)
        .ok()
        .map(|v| parse_truthy(v.trim()))
        .unwrap_or(false)
}

/// Parse a truthy token the same way across the env var and the file.
///
/// Why: the console may write `true`/`false` while an operator may set the env
/// to `1`; one parser keeps both forms consistent.
/// What: case-insensitive match against `1`, `true`, `yes`, `on`.
/// Test: `parse_truthy_accepts_known_tokens`.
fn parse_truthy(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Why: a missing override file must read as `false` (auto-resume is opt-in),
    /// not error — the console shows the toggle off by default.
    /// Test: this test.
    #[test]
    fn read_missing_is_false() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("auto_resume");
        assert!(!read_desired_at(&path).expect("read missing"));
    }

    /// Why: the console toggle's write must be durable and round-trip exactly so
    /// a re-read (after a daemon restart) reflects the operator's choice.
    /// Test: this test.
    #[test]
    fn write_then_read_round_trips() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("nested").join("auto_resume");
        write_desired_at(&path, true).expect("write true");
        assert!(read_desired_at(&path).expect("read true"));
        write_desired_at(&path, false).expect("write false");
        assert!(!read_desired_at(&path).expect("read false"));
    }

    /// Why: an operator may set the env to `1` (not `true`); the env parser must
    /// accept the same truthy tokens as the file.
    /// Test: this test.
    #[test]
    fn parse_truthy_accepts_known_tokens() {
        for t in ["1", "true", "TRUE", "Yes", "on"] {
            assert!(parse_truthy(t), "{t} should be truthy");
        }
        for f in ["0", "false", "no", "off", ""] {
            assert!(!parse_truthy(f), "{f} should be falsy");
        }
    }

    /// Why: the desired-state file must live under the framework root so the
    /// supervisor and console agree on one location.
    /// Test: this test.
    #[test]
    fn desired_path_is_under_root() {
        let paths = FrameworkPaths::under("/tmp/test-base");
        let p = desired_path(&paths);
        assert!(p.ends_with(".trusty-mpm/auto_resume"), "{p:?}");
    }
}
