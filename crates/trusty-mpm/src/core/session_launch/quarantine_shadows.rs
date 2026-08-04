//! Wire the #4448 quarantine into a workspace, once (issue #4448).
//!
//! Why: two live paths need this sweep — `prepare_session_inner` on every
//! session launch and `sync_session_assets` on every `tm sessions sync-assets`.
//! Both must aim it at the SAME directory with the SAME roster and the SAME
//! backup root. Two hand-rolled call sites would drift, and the drift is
//! asymmetric harm: one path repairs a workspace the other re-breaks, or one
//! aims somewhere the other never would.
//!
//! What: [`quarantine_workspace_shadows`] resolves the roster, the workspace
//! tier, and the backup root, then delegates to
//! [`trusty_agents_common::agents::quarantine::quarantine_shadowing_agents`].
//!
//! 🔴 THE TARGET IS ALWAYS `project_dir`'s OWN `.claude/agents`, spelled out
//! rather than taken from `fw.claude_agents_dir()`. Those are the same directory
//! for a managed `fw`, but `fw` is a home-tier `FrameworkPaths::default()` on
//! the non-git `tm session start` and TUI `/connect` paths — where
//! `fw.claude_agents_dir()` is the operator's `~/.claude/agents`. Sweeping THAT
//! would move the roster out of a Claude Code install with nothing to do with
//! trusty-mpm, and it is strictly more dangerous there than retraction is:
//! retraction can only delete ledger-tracked files (of which the operator's home
//! tier has none), while this sweep moves UNTRACKED ones, which is exactly what
//! that directory is full of. Binding the target to the workspace path here,
//! once, makes that structural.
//! Test: `prepare_session_never_quarantines_the_operator_home_agents_tier`.
//!
//! Test: `crates/trusty-mpm/src/core/session_launch/tests.rs`.

use std::path::{Path, PathBuf};

use trusty_agents_common::agents::quarantine::{QuarantineError, quarantine_shadowing_agents};
use trusty_agents_common::agents::quarantine_receipt::QuarantineReport;

use crate::core::bundled_roster::bundled_roster;
use crate::core::paths::FrameworkPaths;

/// Where a workspace's quarantined agents are preserved.
///
/// Deliberately OUTSIDE `.claude/agents/` — a backup nested inside the tier it
/// was taken from is one recursive loader away from being an agent again.
pub fn backup_root(project_dir: &Path) -> PathBuf {
    project_dir.join(".trusty-mpm").join("agent-quarantine")
}

/// The workspace agent tier this sweep may touch — never `fw.claude_agents_dir()`.
pub fn workspace_tier(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("agents")
}

/// Quarantine every shadowing leftover in `project_dir`'s own agent tier.
///
/// Why: `retract_framework_agents` (#4409) removes only what its ownership
/// ledger names. A copy written before that ledger existed is invisible to it,
/// outranks the canonical user tier, and is never refreshed again — #4408 made
/// permanent. This is the half that reaches those.
/// What: resolves the shared [`bundled_roster`] (the SAME one `tm doctor`'s
/// `asset_tier` probe reports from), stamps a UTC run id so a second sweep
/// never overwrites the first run's backups, and delegates. Returns whatever the
/// sweep reports, including a partially failed run — the caller logs it.
/// Callers must run this AFTER retraction: the two target the same directory,
/// and retraction is the cheaper, ledger-proven half.
/// Test: `prepare_session_quarantines_a_shadowing_workspace_agent`,
/// `prepare_session_never_quarantines_the_operator_home_agents_tier`,
/// `sync_assets_quarantines_a_shadowing_workspace_agent`.
pub fn quarantine_workspace_shadows(
    fw: &FrameworkPaths,
    project_dir: &Path,
) -> Result<QuarantineReport, QuarantineError> {
    let run_id = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    quarantine_shadowing_agents(
        &workspace_tier(project_dir),
        &backup_root(project_dir),
        &bundled_roster(fw),
        &run_id,
    )
}

/// Render one sweep's outcome as the line an operator needs, or `None` when
/// nothing happened.
///
/// Why: a sweep that moved a file changed the working tree under an operator
/// who did not ask for it; that must be visible, with the receipt path, not
/// buried at debug level. A clean sweep must say nothing at all — this runs on
/// every launch.
/// What: `None` when nothing moved and nothing failed. Otherwise a summary
/// naming the counts and the receipt (or why it could not be written).
/// Test: `quarantine_summary_is_silent_on_a_clean_sweep`.
pub fn summarize(report: &QuarantineReport) -> Option<String> {
    if !report.wrote_anything() {
        return None;
    }
    let receipt = match (&report.receipt, &report.receipt_error) {
        (Some(path), _) => format!("receipt: {}", path.display()),
        (None, Some(err)) => format!("RECEIPT COULD NOT BE WRITTEN: {err}"),
        (None, None) => "no receipt".to_owned(),
    };
    Some(format!(
        "quarantined {} shadowing project-tier agent file(s), {} failed, {} left alone; \
         nothing was deleted — each moved file has a backup and an inert `.md.disabled` \
         sibling ({receipt})",
        report.moved.len(),
        report.failed.len(),
        report.skipped.len(),
    ))
}
