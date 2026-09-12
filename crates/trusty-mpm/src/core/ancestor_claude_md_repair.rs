//! `tm doctor --fix` for ancestor memory files (#7673).
//!
//! Why: the finding is only half the fix. The two shapes need opposite
//! treatment, and conflating them is how a repair destroys something:
//!
//! * A PURE SEED TEMPLATE is tm's own boilerplate with no project content in
//!   it. tm wrote it, tm recognises it, and it is pure token cost — so it is
//!   renamed aside, reversibly, out of the name Claude Code loads.
//! * A file with REAL CONTENT is someone's notes. Renaming it is not tm's call.
//!   Claude Code's own `claudeMdExcludes` key keeps it out of THIS project's
//!   sessions while leaving the file, and every other project, untouched.
//!
//! What: [`repair_ancestor_claude_md`] produces one
//! [`crate::core::doctor_repair::RepairStep`] per finding, `Planned` in dry run.
//! It NEVER touches a file inside the project root — [`scan_with_excludes`]
//! reports ancestors only — and never touches a file it did not shape-test.
//! Test: `ancestor_claude_md_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::ancestor_claude_md::{AncestorMemoryFile, scan, stale_seed_name};
use crate::core::claude_md_excludes::{ExcludeWrite, add_exclude};
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
/// What: one step per finding that is not already excluded. `today` is injected
/// (`%Y%m%d`) so the rename target is assertable. `home` and
/// `managed_config_dir` are injected for the same reason the scan takes them —
/// no process-global environment write (#5544). The exclude is written into
/// `<project_root>/.claude/settings.local.json`, the layer that is the
/// operator's own per-project override and is not committed.
/// Test: `a_seed_ancestor_is_renamed_aside`,
/// `a_content_ancestor_is_excluded_not_renamed`,
/// `dry_run_writes_nothing`, `an_already_excluded_ancestor_yields_no_step`.
pub fn repair_ancestor_claude_md(
    project_root: &Path,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
    today: &str,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let settings = project_root.join(".claude").join("settings.local.json");
    scan(project_root, home, managed_config_dir)
        .into_iter()
        .filter(|found| !found.excluded)
        .map(|found| step_for(&found, &settings, today, mode))
        .collect()
}

/// One finding's worth of [`repair_ancestor_claude_md`].
///
/// Test: see [`repair_ancestor_claude_md`].
fn step_for(
    found: &AncestorMemoryFile,
    settings: &Path,
    today: &str,
    mode: RepairMode,
) -> RepairStep {
    if found.seed_template {
        seed_step(found, stale_seed_name(&found.path, today), mode)
    } else {
        exclude_step(found, settings, mode)
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

/// Keep a content-carrying ancestor out of this project's sessions.
fn exclude_step(found: &AncestorMemoryFile, settings: &Path, mode: RepairMode) -> RepairStep {
    let what = format!(
        "add it to `claudeMdExcludes` in {} (the file itself is left alone; \
         ~{} tokens per turn recovered)",
        settings.display(),
        found.token_estimate()
    );
    let status = if mode == RepairMode::DryRun {
        StepStatus::Planned
    } else {
        match add_exclude(settings, &found.path) {
            ExcludeWrite::Added => StepStatus::Applied {
                backup: crate::core::statusline_settings::backup_of(settings)
                    .is_file()
                    .then(|| crate::core::statusline_settings::backup_of(settings)),
            },
            ExcludeWrite::AlreadyListed => {
                StepStatus::Refused("already listed in claudeMdExcludes at this layer".to_string())
            }
            ExcludeWrite::Refused(why) => StepStatus::Refused(why),
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
