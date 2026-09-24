//! `tm doctor --fix-launchd-secrets` — the #8236 credential strip on its own.
//!
//! Why: `tm doctor --fix` runs every repair class — skills, hooks across every
//! project, statusline, session scope, stray `.mcp.json` quarantine, worktree
//! markers — before it reaches the LaunchAgent strip. An operator fixing one
//! credential exposure had no way to do it without those machine-wide writes.
//! And the strip's atomic rewrite preserves the old mode, so a scrubbed plist
//! stayed `0644` and the `launchd_secrets` row stayed at WARN.
//!
//! What: [`repair_launchd_secrets_only`] runs
//! [`repair_launchd_plist_secrets`] unchanged — migrate, byte-equal read-back,
//! strip only the confirmed keys — and then, for each plist it stripped
//! successfully, narrows a mode wider than `0600` to `0600` and re-reads it.
//! A chmod that fails, or that leaves the mode wide, turns the step into
//! [`StepStatus::Failed`]; it never reports success. Step text carries key
//! names, modes and error kinds, never a value.
//!
//! Test: `doctor_launchd_secrets_scoped_tests.rs`.

use std::path::Path;

use super::doctor_launchd_secrets_repair::repair_launchd_plist_secrets;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The owner-only mode a credential-bearing plist should carry.
const OWNER_ONLY: u32 = 0o600;

/// Run ONLY the LaunchAgent credential strip, then tighten each stripped plist.
///
/// Why/What: see the module docs. Dry run under [`RepairMode::DryRun`]: the
/// planned step also says the mode will be tightened. Never runs another
/// repair class.
/// Test: `scoped_repair_runs_only_the_launchd_secrets_class`,
/// `scoped_repair_tightens_a_wide_plist_to_0600`,
/// `scoped_repair_fails_when_the_chmod_fails`,
/// `scoped_repair_output_never_carries_a_value`.
pub fn repair_launchd_secrets_only(home: &Path, mode: RepairMode) -> Vec<RepairStep> {
    tighten_modes(
        repair_launchd_plist_secrets(home, mode),
        mode,
        &set_mode,
        &read_mode,
    )
}

/// Apply the mode-tightening pass to each step the strip produced.
///
/// Why: separated from [`repair_launchd_secrets_only`] so a test can drive it
/// with an injected store and an injected `chmod` that fails.
/// What: maps [`tighten_one`] over `steps`.
/// Test: as [`repair_launchd_secrets_only`].
fn tighten_modes(
    steps: Vec<RepairStep>,
    mode: RepairMode,
    chmod: &dyn Fn(&Path, u32) -> std::io::Result<()>,
    stat: &dyn Fn(&Path) -> Option<u32>,
) -> Vec<RepairStep> {
    steps
        .into_iter()
        .map(|step| tighten_one(step, mode, chmod, stat))
        .collect()
}

/// Tighten one plist after a successful strip, or say it would.
///
/// Why (#8236): Fail-Open Check — a chmod that did not take must not leave an
/// `Applied` step behind, or `--fix-launchd-secrets` reports a fix the row
/// still WARNs about.
/// What: leaves the step alone unless its mode is readable and wider than
/// `0600`. A `Planned` step gains "then tighten"; an `Applied` step is
/// chmodded, re-read, and either gains "tightened" or becomes `Failed`.
/// Refused and Failed steps are untouched — nothing was stripped.
/// Test: `scoped_repair_tightens_a_wide_plist_to_0600`,
/// `scoped_repair_fails_when_the_chmod_fails`,
/// `scoped_repair_fails_when_the_mode_stays_wide`.
fn tighten_one(
    mut step: RepairStep,
    mode: RepairMode,
    chmod: &dyn Fn(&Path, u32) -> std::io::Result<()>,
    stat: &dyn Fn(&Path) -> Option<u32>,
) -> RepairStep {
    let Some(current) = stat(&step.path).filter(|m| is_wide(*m)) else {
        return step;
    };
    match (&step.status, mode) {
        (StepStatus::Planned, _) => {
            step.what
                .push_str(&format!("; then tighten mode {current:04o} to 0600"));
        }
        (StepStatus::Applied { .. }, RepairMode::Apply) => {
            let outcome = chmod(&step.path, OWNER_ONLY).map(|()| stat(&step.path));
            match outcome {
                Ok(Some(after)) if !is_wide(after) => {
                    step.what
                        .push_str(&format!("; tightened mode {current:04o} to {after:04o}"));
                }
                Ok(after) => {
                    step.status = StepStatus::Failed(format!(
                        "credentials migrated and removed, but the mode is still {} after \
                         chmod 600 — run `chmod 600` on it by hand",
                        after.map_or_else(|| "unknown".to_string(), |m| format!("{m:04o}"))
                    ));
                }
                Err(e) => {
                    step.status = StepStatus::Failed(format!(
                        "credentials migrated and removed, but could not tighten mode \
                         {current:04o} to 0600 ({:?}) — run `chmod 600` on it by hand",
                        e.kind()
                    ));
                }
            }
        }
        _ => {}
    }
    step
}

/// Is `mode` readable or writable by anyone but the owner, or executable?
fn is_wide(mode: u32) -> bool {
    mode & 0o777 & !OWNER_ONLY != 0
}

/// Set `path`'s permission bits to `mode`.
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// `path`'s own permission bits, never a symlink target's.
fn read_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
#[path = "doctor_launchd_secrets_scoped_tests.rs"]
mod tests;
