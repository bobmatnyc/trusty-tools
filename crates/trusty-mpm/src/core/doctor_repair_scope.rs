//! `tm doctor --fix` re-applies the session-scope writes to a live project (#7678).
//!
//! Why: the two session-scope writes happen ONCE, inside `prepare_session`. A
//! project whose session was paused before the write existed — or resumed
//! across the upgrade that added it — runs against a settings file that never
//! took it, and nothing put it back. On the machine that motivated this, two
//! user-tier plugins kept loading at roughly 2,600 tokens per turn while
//! `tm doctor` reported them as "NOT loaded". The `session_scope` check now
//! names that gap; this is the arm that closes it.
//!
//! What: [`repair_session_scope`] emits at most two [`RepairStep`]s — the
//! project-tier `enabledPlugins` map, and the composed session-MCP file — each
//! only when what is on disk differs from what a launch would write, and each
//! performed by the SAME function the launch path calls
//! ([`crate::core::session_launch::write_enabled_plugins_with_trust`],
//! [`crate::core::session_mcp_scope::provision_at`]). There is no second
//! implementation of either write here.
//!
//! FAIL-CLOSED, deliberately. A write that returns an error is
//! [`StepStatus::Failed`], never a step that reports success; and a write that
//! returns `Ok` is verified back OFF DISK before it is reported applied, so a
//! silent no-op cannot render as a repair. [`RepairStep::changed`] is false for
//! both, so the summary counts neither as applied.
//!
//! Test: `doctor_repair_scope_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::paths::FRAMEWORK_DIR_NAME;
use crate::core::session_plugin_scope::ENABLED_PLUGINS_KEY;
use crate::core::session_scope_drift::PluginKeyDrift;

/// The `tm doctor` check every step here answers.
///
/// Why: one literal, shared with `daemon::doctor_session_scope::CHECK_NAME`, so
/// a `--fix` line and the check that reported it read the same name.
/// What: `session_scope`.
/// Test: `session_scope_repair_steps_name_the_check`.
pub const CHECK_NAME: &str = "session_scope";

/// Re-apply both session-scope writes to one live project.
///
/// Why: see the module doc. `--fix` is the only surface an operator can reach
/// between launches, so it is where a project that missed the write gets it.
/// What: returns no steps at all — writing nothing, reading nothing further —
/// unless the project carries a `.trusty-mpm/` directory. THAT DIRECTORY IS THE
/// GUARD: it is what `tm` creates for a project it manages, so its absence means
/// this is somebody else's checkout that happens to be the cwd, and `--fix` must
/// not write a default-deny plugin map into it. A `None` `config_dir` (an
/// unresolvable managed config dir) is the same refusal for the same reason the
/// launch writer declines it — enumerating nothing would write an empty map.
///
/// `mcp_path` is the composed session-MCP file's location
/// ([`crate::core::session_mcp_scope::session_mcp_path`]); `None` skips that
/// half. Threading it in rather than resolving it here keeps the repair
/// hermetic, since the real path is derived from the operator's `$HOME`.
/// Test: `session_scope_repair_skips_an_unregistered_project`,
/// `session_scope_repair_applies_the_default_deny_map`,
/// `session_scope_repair_writes_nothing_when_the_settings_already_match`,
/// `session_scope_repair_reports_an_unwritable_settings_file_as_failed`,
/// `session_scope_repair_provisions_an_absent_mcp_file`.
pub fn repair_session_scope(
    project_dir: &Path,
    config_dir: Option<&Path>,
    mcp_path: Option<&Path>,
    mode: RepairMode,
) -> Vec<RepairStep> {
    repair_session_scope_with_trust(
        project_dir,
        config_dir,
        mcp_path,
        mode,
        crate::core::project_trust::is_project_trusted(project_dir),
    )
}

/// [`repair_session_scope`] against an explicit trust decision.
///
/// Why: the hermetic seam, for the same reason
/// [`crate::core::session_mcp_scope::granted_plugins_with_trust`] has one — the
/// trust bit lives under the operator's `$HOME`, and a repair that could only be
/// tested against an untrusted project could never be shown to write `true` for
/// anything.
/// What: see [`repair_session_scope`]; `trusted` replaces the store lookup for
/// the plugin half. The MCP half still resolves trust itself, inside
/// [`crate::core::session_mcp_scope::provision_at`].
/// Test: `session_scope_repair_enables_an_opted_in_plugin_for_a_trusted_project`.
pub fn repair_session_scope_with_trust(
    project_dir: &Path,
    config_dir: Option<&Path>,
    mcp_path: Option<&Path>,
    mode: RepairMode,
    trusted: bool,
) -> Vec<RepairStep> {
    // #7678: the managed-project marker is the whole guard. No marker, no write.
    if !project_dir.join(FRAMEWORK_DIR_NAME).is_dir() {
        return Vec::new();
    }
    let Some(config_dir) = config_dir else {
        return Vec::new();
    };

    let mut steps = Vec::new();
    if let Some(step) = plugin_step(project_dir, config_dir, mode, trusted) {
        steps.push(step);
    }
    if let Some(step) = mcp_path.and_then(|path| mcp_step(path, project_dir, config_dir, mode)) {
        steps.push(step);
    }
    steps
}

/// The `enabledPlugins` half — one step, or none when the file already matches.
///
/// Why: an idempotent `--fix` is what lets an operator run it without thinking
/// about whether it is needed, so "already current" has to be a NO step rather
/// than a step that reports writing the same bytes back.
/// What: plans through [`crate::core::session_scope_drift`], names every
/// divergent key in the step's description, and in
/// [`RepairMode::Apply`] performs the write through the launch path's own
/// writer. A successful write is re-planned off disk before it is reported
/// applied: if the file still diverges, the step is [`StepStatus::Failed`],
/// because a repair that reports success without changing anything is the
/// failure mode this check exists to catch.
/// Test: `session_scope_repair_applies_the_default_deny_map`,
/// `session_scope_repair_preserves_foreign_settings_keys`,
/// `session_scope_repair_reports_an_unwritable_settings_file_as_failed`.
fn plugin_step(
    project_dir: &Path,
    config_dir: &Path,
    mode: RepairMode,
    trusted: bool,
) -> Option<RepairStep> {
    let plan = crate::core::session_scope_drift::plan_enabled_plugins_with_trust(
        project_dir,
        config_dir,
        trusted,
    )?;
    if plan.drift.is_empty() {
        return None;
    }

    let what = format!(
        "re-apply the project-tier `{ENABLED_PLUGINS_KEY}` scope — {} key(s): {}",
        plan.drift.len(),
        plan.drift
            .iter()
            .map(PluginKeyDrift::describe)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let status = match mode {
        RepairMode::DryRun => StepStatus::Planned,
        RepairMode::Apply => match crate::core::session_launch::write_enabled_plugins_with_trust(
            project_dir,
            Some(config_dir),
            trusted,
        ) {
            Err(err) => StepStatus::Failed(err.to_string()),
            Ok(()) => verify_plugin_write(project_dir, config_dir, trusted),
        },
    };

    Some(RepairStep {
        check: CHECK_NAME,
        path: plan.settings_path,
        what,
        status,
    })
}

/// Confirm from disk that the plugin write actually landed.
///
/// Why: `Ok(())` proves the writer returned, not that the file now carries the
/// map — and "reported a repair that did not happen" is exactly the shape of the
/// defect #7678 was filed for.
/// What: re-plans against the file as it now is; [`StepStatus::Applied`] when
/// nothing is left to write, [`StepStatus::Failed`] naming the survivors
/// otherwise.
/// Test: `session_scope_repair_applies_the_default_deny_map`.
fn verify_plugin_write(project_dir: &Path, config_dir: &Path, trusted: bool) -> StepStatus {
    let left = crate::core::session_scope_drift::plan_enabled_plugins_with_trust(
        project_dir,
        config_dir,
        trusted,
    )
    .map(|plan| plan.drift)
    .unwrap_or_default();

    if left.is_empty() {
        return StepStatus::Applied { backup: None };
    }
    StepStatus::Failed(format!(
        "the write reported success but the file still diverges: {}",
        left.iter()
            .map(PluginKeyDrift::describe)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The composed session-MCP file half — one step, or none when it is current.
///
/// Why: unlike `enabledPlugins`, this file is NOT a settings file and is not
/// read from the repository: it is handed to Claude Code as
/// `--strict-mcp-config --mcp-config <path>` at spawn and rewritten on every
/// launch. Repairing it therefore changes nothing for a session already running
/// — it makes the next launch correct, and it makes `--fix` honest about having
/// re-applied both halves of the scope rather than one.
/// What: compares the file's bytes against
/// [`crate::core::session_mcp_scope::composed_body`] and, when they differ,
/// rewrites through [`crate::core::session_mcp_scope::provision_at`] — the same
/// writer, permissions included, that the spawn path calls.
/// Test: `session_scope_repair_provisions_an_absent_mcp_file`,
/// `session_scope_repair_leaves_a_current_mcp_file_alone`.
fn mcp_step(
    path: &Path,
    project_dir: &Path,
    config_dir: &Path,
    mode: RepairMode,
) -> Option<RepairStep> {
    let want = match crate::core::session_mcp_scope::composed_body(project_dir, config_dir) {
        Ok(body) => body,
        Err(err) => {
            return Some(RepairStep {
                check: CHECK_NAME,
                path: path.to_path_buf(),
                what: "recompose the session-scoped MCP config".to_string(),
                status: StepStatus::Failed(err.to_string()),
            });
        }
    };

    let existed = path.exists();
    if std::fs::read_to_string(path).ok().as_deref() == Some(want.as_str()) {
        return None;
    }

    let what = if existed {
        "rewrite the session-scoped MCP config (its contents differ from what a launch \
         would compose)"
            .to_string()
    } else {
        "write the session-scoped MCP config (absent — this project has not launched since \
         default-deny scoping landed)"
            .to_string()
    };

    let status = match mode {
        RepairMode::DryRun => StepStatus::Planned,
        RepairMode::Apply => {
            match crate::core::session_mcp_scope::provision_at(path, project_dir, config_dir) {
                Ok(_) => StepStatus::Applied { backup: None },
                Err(err) => StepStatus::Failed(err.to_string()),
            }
        }
    };

    Some(RepairStep {
        check: CHECK_NAME,
        path: PathBuf::from(path),
        what,
        status,
    })
}

#[cfg(test)]
#[path = "doctor_repair_scope_tests.rs"]
mod tests;
