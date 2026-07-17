//! `tm doctor` hook-hygiene probe: tm hook contamination + foreign harness
//! conflicts in project-level Claude settings files (issue #2940).
//!
//! Why: split out of `doctor.rs` to keep it under the 500-SLOC production
//! cap, mirroring the existing `doctor_output_style.rs` / `doctor_fs_checks.rs`
//! splits. A pre-#2940 `tm install` wired tm hooks into every project's
//! `.claude/settings.json`/`settings.local.json` it discovered under `$HOME`;
//! this probe surfaces machines still carrying that damage on the projects
//! `tm doctor` actually knows about (`tm hooks clean` is the fix — its
//! default, no-`--path` mode does the exhaustive `$HOME`-wide sweep this
//! probe deliberately does NOT do), and separately warns when a project's OWN
//! pre-existing claude-mpm hooks would fire inside a tm session
//! (informational only — tm never touches another harness's hooks).
//! What: [`check_hooks_hygiene`] scans `project_dir` (when given) plus every
//! currently-live `active_workspace_paths` — the SAME bounded scope
//! `check_agents`/`check_skills` use — and returns TWO checks:
//! `hooks_contamination` (`Warn` when any file carries a tm-owned hook group)
//! and `hooks_foreign_conflict` (`Warn` when any file carries a foreign
//! claude-mpm hook group).
//! Test: the `tests` module below covers both checks against temp
//! directories.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::standalone::hooks::cleanup::{foreign_hook_event_names, tm_hook_event_names};

/// Every `.claude/settings*.json` this probe should inspect.
///
/// Why (performance): an earlier revision of this probe walked the entire
/// `$HOME` tree via `discover_claude_settings` on every `tm doctor` call —
/// on a real developer machine with hundreds of checked-out projects that
/// walk alone took 15+ seconds, turning `tm doctor` (and `GET
/// /api/v1/doctor`) into a timeout risk for every caller. `tm doctor` must
/// stay fast, so this probe is scoped to exactly the projects it already
/// knows about: `project_dir` (the workspace doctor was invoked for) and
/// `active_workspace_paths` (every workspace path the daemon currently
/// tracks) — mirroring the scoping [`super::check_agents`] /
/// [`super::check_skills`] already use. The exhaustive `$HOME`-wide sweep
/// (needed to find contamination in projects tm is not currently tracking)
/// remains available as an explicit, user-invoked operation via `tm hooks
/// clean` (no `--path`), which pays that cost once rather than on every
/// diagnostic call.
/// What: returns the deduplicated set of `<dir>/.claude/{settings.json,
/// settings.local.json}` for `project_dir` plus every `active_workspace_paths`
/// entry, filtered to files that actually exist.
fn candidate_settings_files(
    project_dir: Option<&Path>,
    active_workspace_paths: &[PathBuf],
) -> Vec<PathBuf> {
    let mut set: BTreeSet<PathBuf> = BTreeSet::new();
    let roots = project_dir
        .into_iter()
        .chain(active_workspace_paths.iter().map(PathBuf::as_path));
    for root in roots {
        for name in ["settings.json", "settings.local.json"] {
            let candidate = root.join(".claude").join(name);
            if candidate.is_file() {
                set.insert(candidate);
            }
        }
    }
    set.into_iter().collect()
}

/// Read and parse one settings file, tolerating a missing or malformed file.
fn read_settings(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&text).ok()?;
    val.is_object().then_some(val)
}

/// Probe every reachable project settings file for tm hook contamination and
/// foreign (claude-mpm) hook conflicts (issue #2940).
///
/// Why: an operator running a pre-#2940 build has tm hooks baked into project
/// files that `tm doctor` should surface so `tm hooks clean` gets run; a
/// project carrying its own claude-mpm hooks is a silent double-firing risk
/// the operator needs to know about even though tm must never touch it. See
/// [`candidate_settings_files`] for why the scan is scoped rather than
/// `$HOME`-wide.
/// What: scans [`candidate_settings_files`] and returns two checks:
/// `hooks_contamination` — `Ok` when no file carries a tm-owned hook group,
/// else `Warn` naming up to 3 affected files (plus a count) and suggesting
/// `tm hooks clean`; `hooks_foreign_conflict` — `Ok` when no file carries a
/// foreign claude-mpm hook group, else `Warn` naming up to 3 affected files
/// (plus a count), purely informational (never suggests removal — that is
/// the operator's call).
/// Test: `check_hooks_hygiene_ok_when_clean`,
/// `check_hooks_hygiene_warns_on_tm_contamination`,
/// `check_hooks_hygiene_warns_on_foreign_conflict`,
/// `check_hooks_hygiene_never_double_counts_active_workspace_dupes`.
pub(super) fn check_hooks_hygiene(
    project_dir: Option<&Path>,
    active_workspace_paths: &[PathBuf],
) -> (DoctorCheck, DoctorCheck) {
    let files = candidate_settings_files(project_dir, active_workspace_paths);

    let mut contaminated: Vec<PathBuf> = Vec::new();
    let mut foreign: Vec<PathBuf> = Vec::new();
    for path in &files {
        let Some(val) = read_settings(path) else {
            continue;
        };
        if !tm_hook_event_names(&val).is_empty() {
            contaminated.push(path.clone());
        }
        if !foreign_hook_event_names(&val).is_empty() {
            foreign.push(path.clone());
        }
    }

    let contamination_check = if contaminated.is_empty() {
        DoctorCheck::new(
            "hooks_contamination",
            CheckStatus::Ok,
            "no tm hook entries found in project-level settings files",
        )
    } else {
        DoctorCheck::new(
            "hooks_contamination",
            CheckStatus::Warn,
            format!(
                "{} project settings file{} carr{} tm hook entries — run `tm hooks clean` \
                 (dry run first) to remove them: {}",
                contaminated.len(),
                if contaminated.len() == 1 { "" } else { "s" },
                if contaminated.len() == 1 { "ies" } else { "y" },
                summarize_paths(&contaminated),
            ),
        )
    };

    let foreign_check = if foreign.is_empty() {
        DoctorCheck::new(
            "hooks_foreign_conflict",
            CheckStatus::Ok,
            "no foreign (claude-mpm) hook entries found in project-level settings files",
        )
    } else {
        DoctorCheck::new(
            "hooks_foreign_conflict",
            CheckStatus::Warn,
            format!(
                "{} project settings file{} carr{} claude-mpm hook entries that would fire \
                 inside a tm session — this is informational only, tm never removes another \
                 harness's hooks: {}",
                foreign.len(),
                if foreign.len() == 1 { "" } else { "s" },
                if foreign.len() == 1 { "ies" } else { "y" },
                summarize_paths(&foreign),
            ),
        )
    };

    (contamination_check, foreign_check)
}

/// Render up to 3 paths plus an overflow count for a check message.
fn summarize_paths(paths: &[PathBuf]) -> String {
    let shown: Vec<String> = paths
        .iter()
        .take(3)
        .map(|p| p.display().to_string())
        .collect();
    if paths.len() > 3 {
        format!("{} (+{} more)", shown.join(", "), paths.len() - 3)
    } else {
        shown.join(", ")
    }
}

#[cfg(test)]
#[path = "doctor_hooks_hygiene_tests.rs"]
mod tests;
