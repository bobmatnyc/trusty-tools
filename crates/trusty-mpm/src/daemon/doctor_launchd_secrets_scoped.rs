//! `tm doctor --fix-launchd-secrets` — the #8236 credential strip on its own.
//!
//! Why: `tm doctor --fix` runs every repair class — skills, hooks across every
//! project, statusline, session scope, stray `.mcp.json` quarantine, worktree
//! markers — before it reaches the LaunchAgent strip. An operator fixing one
//! credential exposure had no way to do it without those machine-wide writes.
//! And the strip's atomic rewrite preserves the old mode, so a scrubbed plist
//! stayed `0644` and the `launchd_secrets` row stayed at WARN.
//!
//! What: [`repair_launchd_secrets_only`] scans once, runs the strip
//! ([`repair_one`]: store pre-check, migrate, byte-equal read-back, strip only
//! the confirmed keys) on every plist the scan flags, and then narrows EVERY
//! scanned `com.trusty.*.plist` that is a regular file wider than `0600` to
//! `0600` (#8563) — a stripped one, a partly stripped one that still holds a
//! credential, one the strip refused, and one that never held a credential,
//! because the row WARNs on each of them. A symlink is never chmodded: `chmod`
//! would follow it out of `~/Library/LaunchAgents`. The decision reads the
//! scan's file type and mode, never a step's text. A chmod that fails, or that
//! leaves the mode wide, turns the step into [`StepStatus::Failed`]; it never
//! reports success. The dry run names the same tightenings. Step text carries
//! key names, modes and error kinds, never a value.
//!
//! Test: `doctor_launchd_secrets_scoped_tests.rs`.

use std::path::Path;
use std::sync::Arc;

use trusty_common::credentials::{KeyStore, default_store};

use super::doctor_launchd_secrets::{CHECK_NAME, PlistFinding, scan_launch_agents};
use super::doctor_launchd_secrets_repair::{listing_failed, repair_one};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The owner-only mode a trusty plist should carry.
const OWNER_ONLY: u32 = 0o600;

/// Run ONLY the LaunchAgent credential strip, then tighten every wide plist.
///
/// Why/What: see the module docs. Dry run under [`RepairMode::DryRun`]: each
/// planned step also says the mode will be tightened. Never runs another
/// repair class.
/// Test: `scoped_repair_runs_only_the_launchd_secrets_class`,
/// `scoped_repair_tightens_a_wide_plist_to_0600`,
/// `scoped_repair_fails_when_the_chmod_fails`,
/// `scoped_repair_output_never_carries_a_value`,
/// `a_partial_strip_still_tightens_the_plist`,
/// `a_wide_plist_without_a_credential_is_tightened`,
/// `a_symlinked_plist_is_never_chmodded`.
pub fn repair_launchd_secrets_only(home: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let store: Arc<dyn KeyStore> = Arc::from(default_store());
    scoped_with(home, mode, store.as_ref(), &set_mode, &read_mode)
}

/// [`repair_launchd_secrets_only`] with the store, `chmod` and `stat` injected.
///
/// Why: a test drives it with a `MemoryKeyStore` and a `chmod` that fails.
/// What: one scan; per plist, the strip when the scan flags it, then
/// [`tighten`]. A directory that cannot be listed is one failed step.
/// Test: as [`repair_launchd_secrets_only`].
fn scoped_with(
    home: &Path,
    mode: RepairMode,
    store: &dyn KeyStore,
    chmod: &dyn Fn(&Path, u32) -> std::io::Result<()>,
    stat: &dyn Fn(&Path) -> Option<u32>,
) -> Vec<RepairStep> {
    let findings = match scan_launch_agents(home) {
        Ok(findings) => findings,
        Err(e) => return vec![listing_failed(home, &e)],
    };
    findings
        .iter()
        .filter_map(|finding| {
            let strip = finding
                .actionable()
                .then(|| repair_one(finding, mode, store));
            tighten(finding, strip, mode, chmod, stat)
        })
        .collect()
}

/// Tighten one scanned plist, or say it would.
///
/// Why (#8236 Fail-Open Check, #8563): a chmod that did not take must not
/// leave an `Applied` step behind, and a plist the strip did not fully clean
/// still holds a credential, so it needs `0600` more, not less.
/// What: `stat` is `None` for anything but a regular file, so a symlink is
/// never touched. A mode no wider than `0600` returns `strip` unchanged. A wide
/// one gets a step (the strip's own, or a new one for a plist with no
/// credential): the dry run appends "then tighten"; the apply chmods, re-reads,
/// and appends "tightened" or turns the step into `Failed`.
/// Test: `scoped_repair_tightens_a_wide_plist_to_0600`,
/// `scoped_repair_fails_when_the_chmod_fails`,
/// `scoped_repair_fails_when_the_mode_stays_wide`,
/// `a_partial_strip_still_tightens_the_plist`,
/// `a_wide_plist_without_a_credential_is_tightened`.
fn tighten(
    finding: &PlistFinding,
    strip: Option<RepairStep>,
    mode: RepairMode,
    chmod: &dyn Fn(&Path, u32) -> std::io::Result<()>,
    stat: &dyn Fn(&Path) -> Option<u32>,
) -> Option<RepairStep> {
    let Some(current) = stat(&finding.path).filter(|m| is_wide(*m)) else {
        return strip;
    };
    let mut step = strip.unwrap_or_else(|| RepairStep {
        check: CHECK_NAME,
        path: finding.path.clone(),
        what: "no credential in it, but a trusty plist wider than 0600 keeps the \
               `launchd_secrets` row at WARN"
            .to_string(),
        status: StepStatus::Planned,
    });
    if mode == RepairMode::DryRun {
        step.what
            .push_str(&format!("; then tighten mode {current:04o} to 0600"));
        return Some(step);
    }
    match chmod(&step.path, OWNER_ONLY).map(|()| stat(&step.path)) {
        Ok(Some(after)) if !is_wide(after) => {
            step.what
                .push_str(&format!("; tightened mode {current:04o} to {after:04o}"));
            if step.status == StepStatus::Planned {
                step.status = StepStatus::Applied { backup: None };
            }
        }
        Ok(after) => {
            let why = format!(
                "the mode is still {} after chmod 600 — run `chmod 600` on it by hand",
                after.map_or_else(|| "unknown".to_string(), |m| format!("{m:04o}"))
            );
            step.status = StepStatus::Failed(with_prior(&step.status, &why));
        }
        Err(e) => {
            let why = format!(
                "could not tighten mode {current:04o} to 0600 ({:?}) — run `chmod 600` on it \
                 by hand",
                e.kind()
            );
            step.status = StepStatus::Failed(with_prior(&step.status, &why));
        }
    }
    Some(step)
}

/// A chmod failure's reason, prefixed by what the strip had already reported.
fn with_prior(prior: &StepStatus, why: &str) -> String {
    match prior {
        StepStatus::Applied { .. } => format!("credentials migrated and removed, but {why}"),
        StepStatus::Failed(r) | StepStatus::Refused(r) => format!("{r}; also {why}"),
        _ => why.to_string(),
    }
}

/// Is `mode` readable or writable by anyone but the owner, or executable?
fn is_wide(mode: u32) -> bool {
    mode & 0o777 & !OWNER_ONLY != 0
}

/// Set `path`'s permission bits to `mode`, never through a symlink.
///
/// `O_NOFOLLOW` then `fchmod`: a link swapped in after the scan makes the open
/// fail instead of chmodding its target.
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?
            .set_permissions(std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

/// `path`'s own permission bits when it is a REGULAR file; `None` otherwise.
fn read_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .ok()
            .filter(std::fs::Metadata::is_file)
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
