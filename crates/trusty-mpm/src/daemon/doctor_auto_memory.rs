//! `tm doctor` auto-memory probe and its `--fix` repair (#7685, #8436).
//!
//! Why: Claude Code's own auto memory (`MEMORY.md`) is off for every tm session,
//! with no fallback — `trusty-memory` is the memory (#8436, Bob Db0 Q5a
//! 2026-10-14, which retired the 2026-09-12 fallback ruling). Two write sites
//! make that true at launch (the `autoMemoryEnabled: false` key on the launch's
//! `--settings` payload, #12127, and
//! the `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` managed-spawn assignment), and
//! neither is observable afterwards: a project that has never been launched, or
//! whose settings file an operator edited, silently carries auto memory back on
//! with nothing reporting it. Measured on 2026-09-12, three headless subagent
//! probes in this project each read the auto-memory index and quoted its first
//! entry, so the exposure is real rather than theoretical.
//! What: [`check_auto_memory`] resolves the EFFECTIVE `autoMemoryEnabled` value
//! across tm's launch-settings record (the `--settings` tier, #12077) and the
//! four settings layers Claude Code itself consults — project local > project >
//! user local > user — reads whether the project's `MEMORY.md`
//! still holds facts, and names whether trusty-memory answered. Its doc carries
//! the full table. [`repair_auto_memory`] is the `tm doctor --fix` half: it
//! writes `false` into that record through
//! [`crate::core::session_launch::set_launch_key`] (#12077), never into the
//! project's `.claude/settings.json`. It never
//! writes `true` and does NOT migrate — `tm memory import-auto-memory` does, and
//! both the check and the repair name it.
//! Test: `auto_memory_fails_when_on_beside_a_healthy_trusty_memory`,
//! `auto_memory_fails_when_on_while_trusty_memory_is_down`,
//! `auto_memory_warns_when_off_while_trusty_memory_is_down`,
//! `auto_memory_ok_when_off_and_the_index_is_empty`,
//! `auto_memory_fails_when_the_index_still_holds_facts`,
//! `auto_memory_fails_when_the_index_holds_facts_and_memory_is_down`,
//! `auto_memory_project_local_overrides_project`,
//! `auto_memory_falls_back_to_the_user_tier`,
//! `auto_memory_fails_on_malformed_json`,
//! `auto_memory_repair_applies_when_absent`,
//! `auto_memory_repair_writes_false_when_memory_is_down`,
//! `auto_memory_repair_dry_run_writes_nothing`,
//! `auto_memory_repair_is_silent_when_already_false`,
//! `auto_memory_repair_reports_a_write_failure`,
//! `auto_memory_repair_never_migrates`,
//! `auto_memory_names_a_wedged_trusty_memory_apart_from_a_dead_one`.

use std::path::{Path, PathBuf};

use crate::core::claude_config::ClaudeConfigReader;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::memory_reachable::MemoryReachability;
use crate::core::session_launch::{
    AUTO_MEMORY_KEY, launch_keys_record_path, read_launch_keys, set_launch_key,
};

/// The `tm doctor` row this module produces.
pub(crate) const CHECK_NAME: &str = "auto_memory";

/// The documented env-var half of the same directive.
///
/// Why: named in this check's remedy text so an operator reading a `Fail` row
/// learns both levers, not only the one `--fix` writes. The assignment itself
/// lives in [`crate::runtime::managed_launch`]; this is prose.
/// What: the variable name, as Claude Code's docs spell it.
/// Test: `auto_memory_fails_when_on_beside_a_healthy_trusty_memory`.
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
/// Test: `auto_memory_fails_on_malformed_json`,
/// `auto_memory_fails_when_unset_beside_a_healthy_trusty_memory`.
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
/// for the report. #9156: the user pair is read from `CLAUDE_CONFIG_DIR` when
/// that is set, else from `<home>/.claude`.
/// Test: `auto_memory_project_local_overrides_project`,
/// `auto_memory_falls_back_to_the_user_tier`,
/// `auto_memory_reads_the_user_tier_from_claude_config_dir`.
fn resolution_layers(project_dir: Option<&Path>, home: &Path) -> Vec<(PathBuf, &'static str)> {
    let paths = ClaudeConfigReader::paths_for_project_with_home(project_dir.unwrap_or(home), home);
    let mut layers = Vec::with_capacity(4);
    if project_dir.is_some() {
        layers.push((paths.project_local_settings, "project-local"));
        layers.push((paths.project_settings, "project"));
    }
    // #9156: a session launched with CLAUDE_CONFIG_DIR reads its user tier there.
    match crate::core::session_record::claude_config_dir_override() {
        Some(dir) => {
            layers.push((dir.join("settings.local.json"), "user-local"));
            layers.push((dir.join("settings.json"), "user"));
        }
        None => {
            layers.push((paths.user_local_settings, "user-local"));
            layers.push((paths.user_settings, "user"));
        }
    }
    layers
}

/// The scope label of tm's launch-settings record (#12077).
const RECORD_SCOPE: &str = "launch-settings record";

/// What tm's launch-settings record says about `autoMemoryEnabled` (#12077).
///
/// Why: 4a delivers the key on the launch's `--settings` payload from this
/// record (`crate::core::session_launch::read_launch_keys`), and its one-time
/// cleanup removes the project-file copy, so the check reads it here.
/// What: `Silent` for an absent record or a missing or non-boolean key;
/// `Broken` for a record that cannot be read or parsed, so it is reported.
/// Test: `auto_memory_reads_a_delivered_key_from_the_launch_record_12077`,
/// `a_malformed_launch_record_fails_the_check_and_the_repair_12077`.
fn record_layer(project_dir: &Path) -> LayerValue {
    match read_launch_keys(project_dir) {
        Ok(keys) => match keys.and_then(|k| k.get(AUTO_MEMORY_KEY).and_then(|v| v.as_bool())) {
            Some(enabled) => LayerValue::Set(enabled),
            None => LayerValue::Silent,
        },
        Err(err) => LayerValue::Broken(err.to_string()),
    }
}

/// The effective `autoMemoryEnabled` across every layer Claude Code consults.
///
/// Why: three of the four verdicts depend on one fact — is auto memory ON right
/// now — and a malformed settings file is a fourth outcome that has to stop
/// resolution rather than fall through as "unset".
/// What: `Err(reason)` for the first unparseable layer; otherwise
/// `Ok(Some((value, scope, path)))` for the first layer that sets the key, and
/// `Ok(None)` when none does.
/// Test: every `auto_memory_*` check test.
type Effective = Result<Option<(bool, &'static str, PathBuf)>, String>;

fn effective_setting(project_dir: Option<&Path>, home: &Path) -> Effective {
    // #12077: the launch's `--settings` tier outranks every file below it.
    if let Some(dir) = project_dir {
        match record_layer(dir) {
            LayerValue::Silent => {}
            LayerValue::Broken(reason) => return Err(reason),
            LayerValue::Set(value) => {
                return Ok(Some((value, RECORD_SCOPE, launch_keys_record_path(dir))));
            }
        }
    }
    for (path, scope) in resolution_layers(project_dir, home) {
        match read_layer(&path) {
            LayerValue::Silent => continue,
            LayerValue::Broken(reason) => return Err(reason),
            LayerValue::Set(value) => return Ok(Some((value, scope, path))),
        }
    }
    Ok(None)
}

/// Grade this project's auto-memory posture (#7685, #8436).
///
/// Why: auto memory is off for every tm session, trusty-memory down included
/// (#8436, Bob Db0 Q5a 2026-10-14). The row flags any project where it is on,
/// and any whose `MEMORY.md` still strands facts; with auto memory correctly off,
/// a trusty-memory that does not answer is the remaining finding — the session
/// has no memory until it does.
/// What: resolves the effective key via [`effective_setting`] and reads the
/// project's `MEMORY.md` through
/// [`crate::core::auto_memory_import::index_has_content`], then:
///
/// | auto memory | `MEMORY.md` | trusty-memory | verdict |
/// |---|---|---|---|
/// | on | any | any | `Fail` — `--fix` writes `false` |
/// | off | non-empty | any | `Fail` — facts stranded; run the migration |
/// | off | empty/absent | not healthy | `Warn` — no memory until it answers |
/// | off | empty/absent | healthy | `Ok` |
///
/// An unset key counts as ON: Claude Code documents auto memory as on by
/// default. A malformed settings file is `Fail` regardless. Read-only —
/// [`repair_auto_memory`] is the write half, and it never migrates.
/// Test: `auto_memory_fails_when_on_beside_a_healthy_trusty_memory`,
/// `auto_memory_fails_when_on_while_trusty_memory_is_down`,
/// `auto_memory_ok_when_off_and_the_index_is_empty`,
/// `auto_memory_fails_when_the_index_still_holds_facts`,
/// `auto_memory_fails_when_the_index_holds_facts_and_memory_is_down`,
/// `auto_memory_warns_when_off_while_trusty_memory_is_down`,
/// `auto_memory_project_local_overrides_project`,
/// `auto_memory_falls_back_to_the_user_tier`,
/// `auto_memory_fails_on_malformed_json`,
/// `auto_memory_names_a_wedged_trusty_memory_apart_from_a_dead_one`.
pub(crate) fn check_auto_memory(
    project_dir: Option<&Path>,
    home: &Path,
    config_dir: Option<&Path>,
    memory: &MemoryReachability,
) -> DoctorCheck {
    // #7685: the row names WHY trusty-memory is not the memory — a wedged daemon
    // is sampled before it is restarted, a dead one is just started.
    let memory_state = memory.describe();
    let effective = match effective_setting(project_dir, home) {
        Ok(effective) => effective,
        Err(reason) => return DoctorCheck::new(CHECK_NAME, CheckStatus::Fail, reason),
    };
    let auto_on = !matches!(effective, Some((false, _, _)));
    let where_set = match &effective {
        Some((_, scope, path)) => format!(
            "`{AUTO_MEMORY_KEY}` at the {scope} tier ({})",
            path.display()
        ),
        None => {
            format!("`{AUTO_MEMORY_KEY}` unset in every settings tier (Claude Code defaults it ON)")
        }
    };

    // #8436: auto-memory off for every session (Bob Db0 Q5a, 2026-10-14) — on
    // is a defect whatever trusty-memory's state.
    if auto_on {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "Claude Code auto-memory is ON — {where_set} — but every tm session runs \
                 with it off ({memory_state}). Run `{REMEDY}` to write \
                 `{AUTO_MEMORY_KEY}: false` into tm's launch-settings record; a managed `tm` launch \
                 also sets `{DISABLE_ENV_VAR}=1` on the spawned session"
            ),
        );
    }

    if let IndexState::Holding(path) = index_state(project_dir, config_dir) {
        let first = if memory.is_reachable() {
            String::new()
        } else {
            format!(" once trusty-memory answers ({memory_state})")
        };
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "Claude Code auto-memory is off ({where_set}) but its index still holds \
                 facts ({}) — nothing reads them and nothing has moved them into the \
                 palace. Run `{MIGRATE}`{first}; it stores each fact, archives the file, \
                 and only then empties the index",
                path.display()
            ),
        );
    }

    if !memory.is_reachable() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "Claude Code auto-memory is off as expected ({where_set}), but \
                 {memory_state} — sessions here have no memory until trusty-memory \
                 answers. Start or restart it; auto memory stays off (#8436)"
            ),
        );
    }

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "trusty-memory is the memory here — auto-memory off ({where_set}) and its \
             index is empty"
        ),
    )
}

/// Whether this project's auto-memory index still holds anything.
enum IndexState {
    /// `MEMORY.md` exists and is non-empty; the path says which one.
    Holding(PathBuf),
    /// Empty, absent, or not resolvable — nothing to migrate either way.
    Empty,
}

/// Read the project's auto-memory index state.
///
/// Why: the index is the other half of the directive — a project whose key says
/// `false` while `MEMORY.md` still carries facts has those facts in neither
/// memory. The path resolution is
/// [`crate::core::auto_memory_import::auto_memory_dir`]'s, so the check and the
/// migration always look at the same directory.
/// What: [`IndexState::Empty`] when either the project dir or the config dir is
/// unknown (there is nothing to name), else the index's own state.
/// Test: `auto_memory_fails_when_the_index_still_holds_facts`,
/// `auto_memory_ok_when_off_and_the_index_is_empty`.
fn index_state(project_dir: Option<&Path>, config_dir: Option<&Path>) -> IndexState {
    let (Some(project_dir), Some(config_dir)) = (project_dir, config_dir) else {
        return IndexState::Empty;
    };
    let memory_dir = crate::core::auto_memory_import::auto_memory_dir(config_dir, project_dir);
    if crate::core::auto_memory_import::index_has_content(&memory_dir) {
        IndexState::Holding(memory_dir.join(crate::core::auto_memory_import::INDEX_FILE))
    } else {
        IndexState::Empty
    }
}

/// The `tm doctor --fix` invocation that applies this check's repair.
const REMEDY: &str = "tm doctor --fix --yes";

/// The migration `--fix` deliberately does NOT run.
///
/// Why: `--fix` writes one boolean key; moving facts between two stores is a
/// data migration with its own failure modes, and an operator must choose to run
/// it. Naming it in the remedy text is how the row stays actionable anyway.
/// What: the command line, quoted verbatim in the `Holding` verdict and the
/// repair step.
/// Test: `auto_memory_fails_when_the_index_still_holds_facts`,
/// `auto_memory_repair_writes_false_when_memory_is_down`.
const MIGRATE: &str = "tm memory import-auto-memory";

/// Write `autoMemoryEnabled: false` into tm's launch-settings record under
/// `--fix`.
///
/// Why: the launch writes this key on every launch; a project that has not
/// launched since its settings were edited has no other route back. #12077:
/// the key is delivered from the launch-settings record and 4a's cleanup
/// removes the project-file copy, so the repair writes the record and never the
/// project `.claude/settings.json`. #8436: `false` whatever trusty-memory's
/// state; there is no fallback to restore.
/// What: no step when the record already reads `false`, or when
/// `settings.local.json` (the operator's layer, which the launch leaves the key
/// to) reads `false`. A local `true` is [`StepStatus::Refused`]: the launch drops
/// the key from the record there, so a write would not last. A record or local
/// file that cannot be read is [`StepStatus::Failed`] in both modes, and the
/// record is not overwritten. Otherwise `Planned` in [`RepairMode::DryRun`];
/// [`RepairMode::Apply`] writes through [`set_launch_key`], and a failed write is
/// `Failed`, never `Applied`.
///
/// It deliberately does NOT migrate the auto-memory store (#7685): that is
/// [`MIGRATE`], an operator-run data move with its own failure modes; the step
/// text names it.
/// Test: `auto_memory_repair_applies_when_absent`,
/// `auto_memory_repair_writes_false_when_memory_is_down`,
/// `auto_memory_repair_dry_run_writes_nothing`,
/// `auto_memory_repair_is_silent_when_already_false`,
/// `auto_memory_repair_reports_a_write_failure`,
/// `auto_memory_repair_never_migrates`,
/// `doctor_fix_never_re_adds_auto_memory_to_the_project_file_12077`,
/// `a_malformed_launch_record_fails_the_check_and_the_repair_12077`,
/// `auto_memory_repair_refuses_a_local_true_12077`.
pub fn repair_auto_memory(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    // #12077: the tm-owned target, never `<project>/.claude/settings.json`.
    let record = launch_keys_record_path(project_dir);
    let local = project_dir.join(".claude").join("settings.local.json");
    // #8436: auto-memory off for every session (Bob Db0 Q5a, 2026-10-14)
    let what = format!(
        "set `{AUTO_MEMORY_KEY}: false` (auto memory is off for every tm session; facts \
         still in `MEMORY.md` move with `{MIGRATE}`)"
    );
    let step = |path: PathBuf, status: StepStatus| {
        vec![RepairStep {
            check: CHECK_NAME,
            path,
            what: what.clone(),
            status,
        }]
    };
    match read_layer(&local) {
        LayerValue::Set(false) => return Vec::new(),
        LayerValue::Set(true) => {
            return step(
                local,
                StepStatus::Refused(
                    "settings.local.json turns auto memory on; it is the operator's file and \
                     the launch leaves the key to it — set it to false there"
                        .to_string(),
                ),
            );
        }
        LayerValue::Broken(reason) => return step(local, StepStatus::Failed(reason)),
        LayerValue::Silent => {}
    }
    match record_layer(project_dir) {
        LayerValue::Set(false) => return Vec::new(),
        LayerValue::Broken(reason) => return step(record, StepStatus::Failed(reason)),
        LayerValue::Set(true) | LayerValue::Silent => {}
    }
    if mode == RepairMode::DryRun {
        return step(record, StepStatus::Planned);
    }
    let status = match set_launch_key(project_dir, AUTO_MEMORY_KEY, serde_json::Value::Bool(false))
    {
        Ok(_) => StepStatus::Applied { backup: None },
        Err(err) => StepStatus::Failed(err.to_string()),
    };
    step(record, status)
}

#[cfg(test)]
#[path = "doctor_auto_memory_tests.rs"]
mod tests;
