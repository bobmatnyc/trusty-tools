//! `tm doctor --fix` for ancestor memory files (#7673).
//!
//! Why: the finding is only half the fix. The two shapes need opposite
//! treatment, and conflating them is how a repair destroys something:
//!
//! * A PURE SEED TEMPLATE is tm's own boilerplate with no project content in
//!   it. tm wrote it, tm recognises it, and it is pure token cost — so it is
//!   renamed aside, reversibly, out of the name Claude Code loads.
//! * A file with REAL CONTENT is someone's notes. Renaming it is not tm's call.
//!   Claude Code's own `claudeMdExcludes` key keeps it out of tm's sessions
//!   while leaving the file untouched. #12077 row 13: the key goes on this
//!   project's launch-settings record, which its tm launches pass as
//!   `--settings`, never into a Claude settings file, so it reaches only this
//!   project's tm sessions: no other project, and no bare `claude`.
//!
//! What: [`repair_ancestor_claude_md`] produces one
//! [`crate::core::doctor_repair::RepairStep`] per finding, `Planned` in dry run.
//! It NEVER touches a file inside the project root — the scan reports ancestors
//! only — and never touches a file it did not shape-test.
//! Test: `ancestor_claude_md_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::ancestor_claude_md::{
    AncestorMemoryFile, resolve_project_root, scan, stale_seed_name,
};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The `tm doctor` check these repairs answer.
///
/// Why: named once so the repair, its tests and the doctor row cannot drift
/// onto a check name the report does not emit.
/// Test: `the_check_name_matches_the_doctor_row`.
pub const ANCESTOR_CHECK: &str = "ancestor_claude_md";

/// Rename every pure-seed ancestor aside and exclude every content-carrying one.
///
/// Why: see the module header.
/// What: resolves `project_root` through [`resolve_project_root`] — `--fix` is
/// handed the process cwd, which may be any subdirectory — then emits one step
/// per finding that is not already excluded. #12077 row 13: the exclude is
/// added to the resolved root's launch-settings record
/// ([`crate::core::session_launch::launch_keys::add_launch_exclude`]); no Claude settings
/// file is written. A failed resolution or scan is ONE `Failed` step naming the
/// error, never an empty plan that reads as "nothing to fix". `today` is
/// injected (`%Y%m%d`) so the rename target is assertable; `home` and
/// `managed_config_dir` (read by the scan only) are injected for the same
/// reason the scan takes them — no process-global environment write (#5544).
/// Test: `a_seed_ancestor_is_renamed_aside`,
/// `a_content_ancestor_is_excluded_not_renamed`,
/// `dry_run_writes_nothing`, `an_already_excluded_ancestor_yields_no_step`,
/// `the_exclude_from_a_subdirectory_lands_on_the_resolved_roots_record`,
/// `a_missing_project_root_is_a_failed_step`,
/// `the_exclude_lands_on_the_launch_settings_record_12077`,
/// `the_exclude_needs_no_managed_config_dir_12077`,
/// `a_recorded_exclude_yields_no_step_12077`,
/// `an_unreadable_launch_record_fails_the_exclude_12077`.
pub fn repair_ancestor_claude_md(
    project_root: &Path,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
    today: &str,
    mode: RepairMode,
) -> Vec<RepairStep> {
    // #7673: scan from the resolved root, not whichever subdirectory `--fix`
    // ran from.
    let scanned = resolve_project_root(project_root, home)
        .and_then(|root| Ok((scan(&root, home, managed_config_dir)?.found, root)));
    let (found, root) = match scanned {
        Ok(scanned) => scanned,
        Err(err) => {
            return vec![RepairStep {
                check: ANCESTOR_CHECK,
                path: project_root.to_path_buf(),
                what: "scan for CLAUDE.md files above the project root".to_string(),
                status: StepStatus::Failed(err.to_string()),
            }];
        }
    };
    found
        .into_iter()
        .filter(|found| !found.excluded)
        .map(|found| step_for(&found, &root, today, mode))
        .collect()
}

/// One finding's worth of [`repair_ancestor_claude_md`].
///
/// Test: see [`repair_ancestor_claude_md`].
fn step_for(found: &AncestorMemoryFile, root: &Path, today: &str, mode: RepairMode) -> RepairStep {
    if found.seed_template {
        seed_step(found, stale_seed_name(&found.path, today), mode)
    } else {
        exclude_step(found, root, mode)
    }
}

/// Rename a pure seed template out of the name Claude Code loads.
fn seed_step(found: &AncestorMemoryFile, target: PathBuf, mode: RepairMode) -> RepairStep {
    let what = format!(
        "rename the tm seed template aside to {} (reversible; ~{} tokens per turn recovered)",
        target.display(),
        found.token_estimate()
    );
    let status = if mode == RepairMode::DryRun {
        StepStatus::Planned
    } else if target.exists() {
        // #7673: never clobber a previous run's rescue copy. A second `--fix` on
        // the same day would otherwise overwrite the first one's evidence.
        StepStatus::Refused(format!("{} already exists", target.display()))
    } else {
        match std::fs::rename(&found.path, &target) {
            // The rename IS the backup — the bytes are intact under the new name.
            Ok(()) => StepStatus::Applied { backup: None },
            Err(err) => StepStatus::Failed(err.to_string()),
        }
    };
    RepairStep {
        check: ANCESTOR_CHECK,
        path: found.path.clone(),
        what,
        status,
    }
}

/// Keep a content-carrying ancestor out of this project's tm sessions.
///
/// #12077 row 13: the exclude goes on `root`'s launch-settings record. A record
/// that cannot be read or stored is a `Failed` step and is left as it is.
fn exclude_step(found: &AncestorMemoryFile, root: &Path, mode: RepairMode) -> RepairStep {
    let record = crate::core::session_launch::launch_keys_record_path(root);
    let what = format!(
        "add it to `claudeMdExcludes` on this project's launch-settings record {}, which \
         `tm launch` passes as `--settings` (the file itself is left alone; ~{} tokens per \
         turn recovered)",
        record.display(),
        found.token_estimate()
    );
    let status = if mode == RepairMode::DryRun {
        StepStatus::Planned
    } else {
        match crate::core::session_launch::launch_keys::add_launch_exclude(root, &found.path) {
            // The record is tm's own, and the step adds one array entry.
            Ok(true) => StepStatus::Applied { backup: None },
            Ok(false) => StepStatus::Refused(
                "already listed in claudeMdExcludes on the launch-settings record".to_string(),
            ),
            Err(err) => StepStatus::Failed(err.to_string()),
        }
    };
    RepairStep {
        check: ANCESTOR_CHECK,
        path: found.path.clone(),
        what,
        status,
    }
}

#[cfg(test)]
#[path = "ancestor_claude_md_repair_tests.rs"]
mod tests;
