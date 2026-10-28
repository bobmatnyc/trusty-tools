//! Persisted operator desired-state for supervisor auto-resume (#1222 / RFC Q6).
//!
//! Why: `TRUSTY_MPM_AUTO_RESUME` is a *process* env var read by the supervisor
//! at startup. The supervisor runs as a separate launchd-managed process, so the
//! daemon (and therefore the console, which speaks MCP to the daemon) cannot
//! mutate the supervisor's live environment. To give the console a real, non-CLI
//! control over auto-resume (RFC §6 Q6 — "the console SHALL provide controls to
//! enable/disable auto-resume"), we persist the operator's *desired* flag to a
//! tiny file under the framework root. The supervisor reads this file on every
//! sweep, and `supervisor_status` surfaces it so the console can render the
//! toggle's true state.
//!
//! #5208: until this issue, the supervisor read nothing — the console wrote the
//! file, `supervisor_status` displayed it, and no code path acted on it. The
//! resuming process consults it through [`AutoResumeGate`] with this
//! precedence: the file when present (the operator's live console choice) >
//! the boot-time `TRUSTY_MPM_AUTO_RESUME` env > off. #10356 (owner ruling
//! D74): that process is now the daemon, not `tm supervisor`.
//! Decide on that with [`read_override_at`] (tri-state), never with
//! [`read_desired_at`] — the latter collapses "file absent" into `false`, so
//! using it would let a never-touched toggle silently disable an env-enabled
//! supervisor.
//! What: [`read_desired`] / [`write_desired`] read/write
//! `~/.trusty-mpm/auto_resume` (a one-line `true` / `false`); [`read_override`] /
//! [`read_override_at`] expose the same file as a tri-state `Option<bool>` that
//! distinguishes "no override" from "explicitly off"; [`effective_from_env`]
//! reports the env-derived flag the supervisor process actually booted with. A
//! missing file means "no operator override"; [`read_desired_at`] renders that as
//! `false` for display (auto-resume is opt-in).
//! Test: `read_missing_is_false`, `write_then_read_round_trips`,
//! `read_override_distinguishes_absent_from_false`,
//! `read_override_propagates_non_notfound_errors`,
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
    // #5208: display-only flattening — the supervisor must use `read_override_at`.
    Ok(read_override_at(path)?.unwrap_or(false))
}

/// Read the persisted flag as a tri-state operator override.
///
/// Why: the supervisor has to tell "the operator never touched the console
/// toggle" (no file → its boot-time env / `--auto-resume` flag stands) apart from
/// "the operator explicitly turned auto-resume OFF" (file says `false` → override
/// the boot flag). [`read_desired_at`] collapses both to `false`, which is right
/// for rendering a toggle and wrong for deciding behavior: it would let an absent
/// file silently disable an env-enabled supervisor — the same write-only /
/// fail-open shape #5208 exists to close.
/// What: `Ok(None)` when the file does not exist, `Ok(Some(flag))` when it does,
/// and `Err` for every other I/O failure so the caller can decide what to hold on
/// to rather than being handed a fabricated `false`.
/// Test: `read_override_distinguishes_absent_from_false`,
/// `read_override_propagates_non_notfound_errors`.
pub fn read_override_at(path: &Path) -> std::io::Result<Option<bool>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(parse_truthy(contents.trim()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
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

/// Read the tri-state operator override from the default framework root.
///
/// Why: the console's status payload must distinguish "no override" from
/// "explicitly off" to report what the next supervisor sweep will actually do.
/// What: resolves [`FrameworkPaths::default`] and reads [`read_override_at`].
/// Test: covered indirectly via `read_override_distinguishes_absent_from_false`.
pub fn read_override() -> std::io::Result<Option<bool>> {
    read_override_at(&desired_path(&FrameworkPaths::default()))
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
/// Why: the console shows the boot env flag beside the persisted desire so an
/// operator can see which one is in force. Since #5208 the persisted file
/// outranks this and applies on the next sweep, so this is the fallback when no
/// override file exists — not, as it once was, the thing a restart is needed to
/// change.
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
///
/// Visibility (#5784): `pub(crate)` so
/// [`crate::core::host_state_gate`]'s opt-in reads the same spellings rather
/// than adding a fourth truthy parser to this crate.
/// Test: `parse_truthy_accepts_known_tokens`.
pub(crate) fn parse_truthy(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// The auto-resume flag in force for one sweep, re-read from disk each time.
///
/// Why: #5208. An operator toggling auto-resume in the console writes
/// [`AUTO_RESUME_FILE`], and the process that resumes must act on it without a
/// restart. #10356 (owner ruling D74) moved auto-resume from `tm supervisor`
/// into the daemon, so the precedence rule moved here, where the daemon's sweep
/// can use it.
/// What: [`Self::resolve`] applies file when present > boot flag > off, logs
/// each transition once, and on a read error holds the last value it saw
/// (falling back to the boot flag). It never fails open to `false`.
/// Test: `the_console_toggle_enables_a_running_daemon_sweep`,
/// `a_console_disable_overrides_an_env_enabled_daemon`,
/// `an_absent_desired_file_keeps_the_boot_flag`,
/// `an_unreadable_desired_file_does_not_disable_resume` in
/// `daemon::auto_resume_loop` tests.
#[derive(Debug, Clone)]
pub struct AutoResumeGate {
    /// The boot-time `TRUSTY_MPM_AUTO_RESUME` value.
    boot_flag: bool,
    /// The console-written desired-state file.
    path: PathBuf,
    /// The last override read successfully.
    last_override: Option<bool>,
}

impl AutoResumeGate {
    /// A gate over `path` with `boot_flag` as the fallback.
    pub fn new(boot_flag: bool, path: impl Into<PathBuf>) -> Self {
        Self {
            boot_flag,
            path: path.into(),
            last_override: None,
        }
    }

    /// The desired-state file this gate reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve the flag for the sweep about to run. See the type's doc.
    pub fn resolve(&mut self) -> bool {
        match read_override_at(&self.path) {
            Ok(Some(desired)) => {
                if self.last_override != Some(desired) {
                    tracing::info!(
                        desired,
                        path = %self.path.display(),
                        "auto-resume: desired state applied from console"
                    );
                    self.last_override = Some(desired);
                }
                desired
            }
            Ok(None) => {
                if self.last_override.is_some() {
                    tracing::info!(
                        boot_flag = self.boot_flag,
                        path = %self.path.display(),
                        "auto-resume: override removed; reverting to boot flag"
                    );
                    self.last_override = None;
                }
                self.boot_flag
            }
            Err(e) => {
                let held = self.last_override.unwrap_or(self.boot_flag);
                tracing::error!(
                    path = %self.path.display(),
                    auto_resume = held,
                    "auto-resume: cannot read desired state: {e}; holding the last known \
                     value (NOT failing open to off)"
                );
                held
            }
        }
    }
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

    /// Why: #5208's precedence rule turns on this distinction — "file absent"
    /// must leave the supervisor's boot flag alone, while "file says false" must
    /// override it. `read_desired_at` cannot express that; `read_override_at`
    /// must.
    /// Test: this test.
    #[test]
    fn read_override_distinguishes_absent_from_false() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("auto_resume");
        assert_eq!(read_override_at(&path).expect("absent"), None);

        write_desired_at(&path, false).expect("write false");
        assert_eq!(
            read_override_at(&path).expect("explicit false"),
            Some(false)
        );

        write_desired_at(&path, true).expect("write true");
        assert_eq!(read_override_at(&path).expect("explicit true"), Some(true));

        // The display-flattening wrapper still reports both absent and explicit
        // false as `false`, which is why the supervisor must not use it.
        std::fs::remove_file(&path).expect("remove");
        assert!(!read_desired_at(&path).expect("flattened absent"));
    }

    /// Why: an I/O failure that is NOT not-found (a directory in the file's
    /// place, an unreadable mode) must reach the caller. Swallowing it into
    /// `false` would silently disable an operator-enabled supervisor — the exact
    /// fail-open #5208 closes, relocated one layer out.
    /// Test: this test.
    #[test]
    fn read_override_propagates_non_notfound_errors() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("auto_resume");
        // A directory where the file should be: reads fail with EISDIR, not NotFound.
        std::fs::create_dir(&path).expect("mkdir");
        let err = read_override_at(&path).expect_err("a directory must not read as a flag");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "only not-found may be flattened to `no override`: {err}"
        );
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
