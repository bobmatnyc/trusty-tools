//! `tm doctor` auto-memory probe and its `--fix` repair (#7685).
//!
//! Why: the owner directive is that Claude Code's own auto memory (`MEMORY.md`)
//! is not used in tm sessions — `trusty-memory` is the memory. Two write sites
//! make that true at launch (the project-tier `autoMemoryEnabled: false` key and
//! the `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` managed-spawn assignment), and
//! neither is observable afterwards: a project that has never been launched, or
//! whose settings file an operator edited, silently carries auto memory back on
//! with nothing reporting it. Measured on 2026-09-12, three headless subagent
//! probes in this project each read the auto-memory index and quoted its first
//! entry, so the exposure is real rather than theoretical.
//! What: [`check_auto_memory`] resolves the EFFECTIVE `autoMemoryEnabled` value
//! across the same four settings layers Claude Code itself consults — project
//! local > project > user local > user — and reports `Ok` for `false`, `Fail`
//! for `true`, and `Warn` when no layer sets it (Claude Code's documented
//! default is ON, so an unset key is the directive unmet, not a pass).
//! [`repair_auto_memory`] is the `tm doctor --fix` half: it writes `false` into
//! the PROJECT tier through the same
//! [`crate::core::session_launch::merge_settings_key`] the launch path uses, so
//! a repaired project and a launched one converge on one code path.
//! Test: `auto_memory_ok_when_project_disables_it`,
//! `auto_memory_warns_when_absent`, `auto_memory_fails_when_enabled`,
//! `auto_memory_project_local_overrides_project`,
//! `auto_memory_falls_back_to_the_user_tier`,
//! `auto_memory_fails_on_malformed_json`,
//! `auto_memory_repair_applies_when_absent`,
//! `auto_memory_repair_dry_run_writes_nothing`,
//! `auto_memory_repair_is_silent_when_already_false`,
//! `auto_memory_repair_reports_a_write_failure`.

use std::path::{Path, PathBuf};

use crate::core::claude_config::ClaudeConfigReader;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::session_launch::{AUTO_MEMORY_KEY, merge_settings_key};

/// The `tm doctor` row this module produces.
pub(crate) const CHECK_NAME: &str = "auto_memory";

/// The documented env-var half of the same directive.
///
/// Why: named in this check's remedy text so an operator reading a `Warn` row
/// learns both levers, not only the one `--fix` writes. The assignment itself
/// lives in [`crate::runtime::claude_code::env_bin_prefix`]; this is prose.
/// What: the variable name, as Claude Code's docs spell it.
/// Test: `auto_memory_warns_when_absent`.
const DISABLE_ENV_VAR: &str = "CLAUDE_CODE_DISABLE_AUTO_MEMORY";

/// What one settings layer says about `autoMemoryEnabled`.
///
/// Why: "the file is absent", "the file exists and is silent on the key" and
/// "the file is malformed" are three different facts. The first two fall
/// through to the next layer identically; the third must stop resolution and
/// report, because a settings file Claude Code cannot parse is a problem this
/// check would otherwise hide.
/// What: `Set` carries the boolean, `Silent` means keep looking, `Broken`
/// carries the reason.
/// Test: exercised by every `auto_memory_*` check test.
enum LayerValue {
    Set(bool),
    Silent,
    Broken(String),
}

/// Read `autoMemoryEnabled` out of one settings file.
///
/// Why: shared by every layer in [`resolution_layers`] so all four apply
/// identical parsing rules.
/// What: `Silent` for a missing file, a non-object body, an absent key, or a
/// key whose value is not a boolean; `Broken` for an unreadable file or invalid
/// JSON; `Set` otherwise.
/// Test: `auto_memory_fails_on_malformed_json`, `auto_memory_warns_when_absent`.
fn read_layer(path: &Path) -> LayerValue {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LayerValue::Silent,
        Err(e) => return LayerValue::Broken(format!("{} unreadable: {e}", path.display())),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            return LayerValue::Broken(format!("{} is not valid JSON: {e}", path.display()));
        }
    };
    match value
        .get(AUTO_MEMORY_KEY)
        .and_then(serde_json::Value::as_bool)
    {
        Some(enabled) => LayerValue::Set(enabled),
        None => LayerValue::Silent,
    }
}

/// The settings layers Claude Code consults, highest precedence first.
///
/// Why: the same chain `daemon::doctor_output_style` walks, for the same reason
/// — a `settings.local.json` that sets the key outranks the project tier tm
/// writes, so reading only the project tier would report a value Claude Code
/// never uses.
/// What: project-local, project, user-local, user; the project pair is omitted
/// when the caller has no project directory. Each entry carries a scope label
/// for the report.
/// Test: `auto_memory_project_local_overrides_project`,
/// `auto_memory_falls_back_to_the_user_tier`.
fn resolution_layers(project_dir: Option<&Path>, home: &Path) -> Vec<(PathBuf, &'static str)> {
    let paths = ClaudeConfigReader::paths_for_project_with_home(project_dir.unwrap_or(home), home);
    let mut layers = Vec::with_capacity(4);
    if project_dir.is_some() {
        layers.push((paths.project_local_settings, "project-local"));
        layers.push((paths.project_settings, "project"));
    }
    layers.push((paths.user_local_settings, "user-local"));
    layers.push((paths.user_settings, "user"));
    layers
}

/// Probe whether Claude Code's auto memory is off for this project (#7685).
///
/// Why: see the module doc. The directive is an owner ruling, so an unset key
/// is a finding rather than a default — Claude Code documents auto memory as on
/// by default.
/// What: walks [`resolution_layers`] and reports the first layer that sets
/// [`AUTO_MEMORY_KEY`]. `Ok` when that value is `false`; `Fail` when it is
/// `true` or when a layer could not be parsed; `Warn` when no layer sets it,
/// naming both the settings key and [`DISABLE_ENV_VAR`]. Read-only —
/// [`repair_auto_memory`] is the write half.
/// Test: `auto_memory_ok_when_project_disables_it`,
/// `auto_memory_warns_when_absent`, `auto_memory_fails_when_enabled`,
/// `auto_memory_project_local_overrides_project`,
/// `auto_memory_falls_back_to_the_user_tier`,
/// `auto_memory_fails_on_malformed_json`.
pub(crate) fn check_auto_memory(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    for (path, scope) in resolution_layers(project_dir, home) {
        match read_layer(&path) {
            LayerValue::Silent => continue,
            LayerValue::Broken(reason) => {
                return DoctorCheck::new(CHECK_NAME, CheckStatus::Fail, reason);
            }
            LayerValue::Set(false) => {
                return DoctorCheck::new(
                    CHECK_NAME,
                    CheckStatus::Ok,
                    format!(
                        "Claude Code auto-memory is off — `{AUTO_MEMORY_KEY}: false` at the \
                         {scope} tier ({})",
                        path.display()
                    ),
                );
            }
            LayerValue::Set(true) => {
                return DoctorCheck::new(
                    CHECK_NAME,
                    CheckStatus::Fail,
                    format!(
                        "Claude Code auto-memory is ON — `{AUTO_MEMORY_KEY}: true` at the {scope} \
                         tier ({}). trusty-memory is the memory here; set it to false, or run \
                         `{REMEDY}`",
                        path.display()
                    ),
                );
            }
        }
    }

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "`{AUTO_MEMORY_KEY}` is unset in every settings tier, and Claude Code defaults auto \
             memory ON — trusty-memory is the memory here. Run `{REMEDY}` to write \
             `{AUTO_MEMORY_KEY}: false` into the project tier; a managed `tm` launch also sets \
             `{DISABLE_ENV_VAR}=1` on the spawned session"
        ),
    )
}

/// The `tm doctor --fix` invocation that applies this check's repair.
const REMEDY: &str = "tm doctor --fix --yes";

/// Write `autoMemoryEnabled: false` into the project tier under `--fix` (#7685).
///
/// Why: the launch path already writes this key, but only on a launch. A project
/// whose settings file predates #7685, or that an operator edited, has no other
/// route back to the directive short of hand-editing JSON. The repair is purely
/// additive — one boolean key, every other key preserved — which is why it is in
/// the auto-repairable set at all.
/// What: reads the PROJECT tier only (the tier tm owns; a higher-precedence
/// `settings.local.json` is the operator's and is never rewritten, the same
/// refusal `output_style_legacy_ids` makes). Produces no step when that tier
/// already reads `false`. Otherwise plans the write in [`RepairMode::DryRun`],
/// and in [`RepairMode::Apply`] performs it through
/// [`merge_settings_key`]. A write that fails yields
/// [`StepStatus::Failed`] carrying the error — never an `Applied` step, so a
/// `--fix` run cannot report a repair it did not make.
/// Test: `auto_memory_repair_applies_when_absent`,
/// `auto_memory_repair_dry_run_writes_nothing`,
/// `auto_memory_repair_is_silent_when_already_false`,
/// `auto_memory_repair_reports_a_write_failure`.
pub fn repair_auto_memory(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let settings_path = project_dir.join(".claude").join("settings.json");
    if matches!(read_layer(&settings_path), LayerValue::Set(false)) {
        return Vec::new();
    }

    let what = format!("set `{AUTO_MEMORY_KEY}: false` (trusty-memory is the memory)");
    if mode == RepairMode::DryRun {
        return vec![RepairStep {
            check: CHECK_NAME,
            path: settings_path,
            what,
            status: StepStatus::Planned,
        }];
    }

    let status =
        match merge_settings_key(project_dir, AUTO_MEMORY_KEY, serde_json::Value::Bool(false)) {
            Ok(()) => StepStatus::Applied { backup: None },
            Err(err) => StepStatus::Failed(err.to_string()),
        };
    vec![RepairStep {
        check: CHECK_NAME,
        path: settings_path,
        what,
        status,
    }]
}

#[cfg(test)]
#[path = "doctor_auto_memory_tests.rs"]
mod tests;
