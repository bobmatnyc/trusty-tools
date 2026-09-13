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
//!
//! The 2026-09-12 owner ruling made auto memory a FALLBACK rather than a thing
//! to be off unconditionally, which is why this row grades a PAIR: the same
//! configuration is a defect beside a healthy trusty-memory and correct beside a
//! dead one — and auto memory off beside a dead one is the project having no
//! memory at all.
//! What: [`check_auto_memory`] resolves the EFFECTIVE `autoMemoryEnabled` value
//! across the same four settings layers Claude Code itself consults — project
//! local > project > user local > user — reads whether the project's `MEMORY.md`
//! still holds facts, and grades the pair against whether trusty-memory
//! answered. Its doc carries the full table. [`repair_auto_memory`] is the
//! `tm doctor --fix` half: it writes the key into the PROJECT tier through the
//! same [`crate::core::session_launch::merge_settings_key`] the launch path
//! uses, so a repaired project and a launched one converge on one code path —
//! and in the same DIRECTION, `false` beside a healthy trusty-memory and `true`
//! beside a dead one. It does NOT migrate — `tm memory import-auto-memory` does,
//! and the check names it.
//! Test: `auto_memory_fails_when_on_beside_a_healthy_trusty_memory`,
//! `auto_memory_is_ok_when_on_while_trusty_memory_is_down`,
//! `auto_memory_warns_when_off_while_trusty_memory_is_down`,
//! `auto_memory_ok_when_off_and_the_index_is_empty`,
//! `auto_memory_fails_when_the_index_still_holds_facts`,
//! `auto_memory_warns_when_the_index_holds_facts_and_memory_is_down`,
//! `auto_memory_project_local_overrides_project`,
//! `auto_memory_falls_back_to_the_user_tier`,
//! `auto_memory_fails_on_malformed_json`,
//! `auto_memory_repair_applies_when_absent`,
//! `auto_memory_repair_restores_the_fallback_when_memory_is_down`,
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
use crate::core::session_launch::{AUTO_MEMORY_KEY, merge_settings_key};

/// The `tm doctor` row this module produces.
pub(crate) const CHECK_NAME: &str = "auto_memory";

/// The documented env-var half of the same directive.
///
/// Why: named in this check's remedy text so an operator reading a `Warn` row
/// learns both levers, not only the one `--fix` writes. The assignment itself
/// lives in [`crate::runtime::claude_code::env_bin_prefix`]; this is prose.
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
    for (path, scope) in resolution_layers(project_dir, home) {
        match read_layer(&path) {
            LayerValue::Silent => continue,
            LayerValue::Broken(reason) => return Err(reason),
            LayerValue::Set(value) => return Ok(Some((value, scope, path))),
        }
    }
    Ok(None)
}

/// Probe the auto-memory FALLBACK posture for this project (#7685).
///
/// Why: the owner ruling of 2026-09-12 made this a check about the FALLBACK
/// POSTURE rather than about one boolean. Auto memory being on is a finding only
/// while trusty-memory is UP; while it is down, auto memory on is the posture the
/// directive asks for, and auto memory OFF is the double-loss state — a project
/// with no memory at all — which is exactly what an operator needs told.
/// What: resolves the effective key via [`effective_setting`] and reads the
/// project's `MEMORY.md` through
/// [`crate::core::auto_memory_import::index_has_content`], then:
///
/// | trusty-memory | auto memory | `MEMORY.md` | verdict |
/// |---|---|---|---|
/// | up | on | any | `Fail` — the directive is unmet |
/// | up | off | non-empty | `Fail` — facts are stranded in the fallback |
/// | up | off | empty/absent | `Ok` |
/// | down | on | any | `Ok` — the fallback is carrying the project |
/// | down | off | any | `Warn` — no memory at all; `--fix` restores it |
///
/// An unset key counts as ON: Claude Code documents auto memory as on by
/// default. A malformed settings file is `Fail` regardless. Read-only —
/// [`repair_auto_memory`] is the write half, and it never migrates.
/// Test: `auto_memory_fails_when_on_beside_a_healthy_trusty_memory`,
/// `auto_memory_is_ok_when_on_while_trusty_memory_is_down`,
/// `auto_memory_ok_when_off_and_the_index_is_empty`,
/// `auto_memory_fails_when_the_index_still_holds_facts`,
/// `auto_memory_warns_when_off_while_trusty_memory_is_down`,
/// `auto_memory_warns_when_the_index_holds_facts_and_memory_is_down`,
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
    let memory_reachable = memory.is_reachable();
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
    let index = index_state(project_dir, config_dir);

    if auto_on {
        if memory_reachable {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Fail,
                format!(
                    "trusty-memory is healthy and IS the memory here, but Claude Code \
                     auto-memory is ON — {where_set}. Run `{REMEDY}` to write \
                     `{AUTO_MEMORY_KEY}: false` into the project tier; a managed `tm` launch \
                     also sets `{DISABLE_ENV_VAR}=1` on the spawned session"
                ),
            );
        }
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "{memory_state}, so Claude Code auto-memory is carrying this project as the \
                 FALLBACK — {where_set}. This is the posture the directive asks for while \
                 trusty-memory is down"
            ),
        );
    }

    // Auto memory is OFF. With trusty-memory down that is the double-loss state
    // the two-way launch write exists to prevent, and it outranks the index
    // finding — a project with no memory at all is the more urgent fact.
    if !memory_reachable {
        let stranded = match &index {
            IndexState::Holding(path) => {
                format!("; its index also still holds facts ({})", path.display())
            }
            IndexState::Empty => String::new(),
        };
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "the fallback is disabled while {memory_state} — {where_set}, so this \
                 project currently has NO memory. Run `{REMEDY}` to write \
                 `{AUTO_MEMORY_KEY}: true` and restore it{stranded}"
            ),
        );
    }

    match index {
        IndexState::Holding(path) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "Claude Code auto-memory is off ({where_set}) but its index still holds \
                 facts ({}) — nothing has moved them into the palace. Run `{MIGRATE}`; \
                 it stores each fact, archives the file, and only then empties the index",
                path.display()
            ),
        ),
        IndexState::Empty => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "trusty-memory is the memory here — auto-memory off ({where_set}) and its \
                 index is empty"
            ),
        ),
    }
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
/// What: the command line, quoted verbatim in the `Holding` verdicts.
/// Test: `auto_memory_fails_when_the_index_still_holds_facts`.
const MIGRATE: &str = "tm memory import-auto-memory";

/// Write `autoMemoryEnabled` into the project tier under `--fix` (#7685).
///
/// Why: the launch path already writes this key, but only on a launch. A project
/// whose settings file predates #7685, or that an operator edited, has no other
/// route back to the directive short of hand-editing JSON. The repair is purely
/// additive — one boolean key, every other key preserved — which is why it is in
/// the auto-repairable set at all.
///
/// It is TWO-WAY for the same reason the launch write is: a repair that could
/// only ever write `false` would deepen the exact failure the check now warns
/// about, disabling the fallback on a host whose trusty-memory is down.
/// What: derives the wanted value as `!memory_reachable` and reads the PROJECT
/// tier only (the tier tm owns; a higher-precedence `settings.local.json` is the
/// operator's and is never rewritten, the same refusal `output_style_legacy_ids`
/// makes). Produces no step when that tier already reads the wanted value.
/// Otherwise plans the write in [`RepairMode::DryRun`], and in
/// [`RepairMode::Apply`] performs it through [`merge_settings_key`]. A write that
/// fails yields [`StepStatus::Failed`] carrying the error — never an `Applied`
/// step, so a `--fix` run cannot report a repair it did not make.
///
/// It deliberately does NOT migrate the auto-memory store (#7685): that is
/// [`MIGRATE`], an operator-run data move with its own failure modes, and a
/// `--fix` that quietly rewrote two stores would be the wrong shape of repair.
/// Test: `auto_memory_repair_applies_when_absent`,
/// `auto_memory_repair_restores_the_fallback_when_memory_is_down`,
/// `auto_memory_repair_dry_run_writes_nothing`,
/// `auto_memory_repair_is_silent_when_already_false`,
/// `auto_memory_repair_reports_a_write_failure`,
/// `auto_memory_repair_never_migrates`.
pub fn repair_auto_memory(
    project_dir: &Path,
    mode: RepairMode,
    memory_reachable: bool,
) -> Vec<RepairStep> {
    let wanted = !memory_reachable;
    let settings_path = project_dir.join(".claude").join("settings.json");
    if matches!(read_layer(&settings_path), LayerValue::Set(v) if v == wanted) {
        return Vec::new();
    }

    let what = if wanted {
        format!(
            "set `{AUTO_MEMORY_KEY}: true`, restoring the fallback (trusty-memory is not \
             answering)"
        )
    } else {
        format!("set `{AUTO_MEMORY_KEY}: false` (trusty-memory is the memory)")
    };
    if mode == RepairMode::DryRun {
        return vec![RepairStep {
            check: CHECK_NAME,
            path: settings_path,
            what,
            status: StepStatus::Planned,
        }];
    }

    let status = match merge_settings_key(
        project_dir,
        AUTO_MEMORY_KEY,
        serde_json::Value::Bool(wanted),
    ) {
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
