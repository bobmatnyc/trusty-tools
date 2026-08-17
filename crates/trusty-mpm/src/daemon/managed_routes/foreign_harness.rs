//! Warn at launch when a checkout carries another harness's hook wiring.
//!
//! Why: `tm doctor`'s `hooks_foreign_conflict` probe already detects a
//! project whose `.claude/settings*.json` wires claude-mpm's hooks, and those
//! hooks fire inside a tm session. Running only under `tm doctor` means the
//! finding surfaces when someone thinks to look, which is not when it matters —
//! an operator learns their session is running two harnesses' hooks at the
//! moment the session starts, or not in time to act on it. This emits the same
//! finding on the launch path.
//!
//! What: [`warn_on_foreign_harness_config`] reads the same settings files the
//! doctor probe reads, through the same
//! [`crate::daemon::doctor::doctor_hooks_hygiene::candidate_settings_files`] and
//! [`crate::daemon::doctor::doctor_hooks_hygiene::read_settings`] entry points,
//! and applies the same
//! [`crate::core::standalone::hooks::cleanup::foreign_hook_event_names`]
//! predicate. One detector, two surfaces.
//!
//! **Report-only, permanently.** tm never removes another harness's hooks —
//! that call belongs to the operator (`core::doctor_repair` records the same
//! rule for the doctor surface). This module logs and returns; it has no
//! mutating path and must not grow one.
//!
//! Test: `foreign_harness_tests.rs`.

use std::path::{Path, PathBuf};

use tracing::warn;

use crate::core::standalone::hooks::cleanup::foreign_hook_event_names;
use crate::daemon::doctor::doctor_hooks_hygiene::{candidate_settings_files, read_settings};

/// Settings files under `dirs` that wire a foreign (claude-mpm) hook.
///
/// Why: split from the logging so the detection has a return value a test can
/// assert on, rather than a test having to capture a tracing subscriber.
/// What: for every existing `<dir>/.claude/settings{,.local}.json`, parses the
/// JSON through the doctor probe's own `read_settings` and keeps the path when
/// [`foreign_hook_event_names`] finds at least one foreign hook group. A file
/// that is missing, unreadable, or not a JSON object contributes nothing — an
/// unparseable settings file is reported by the doctor probe, which owns that
/// finding; this launch-path warning is about foreign hooks it can actually see.
/// Test: `detects_foreign_hooks_in_launch_dir`, `clean_dir_reports_nothing`,
/// `malformed_settings_file_is_skipped_not_panicked`.
pub(super) fn foreign_harness_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    candidate_settings_files(None, dirs)
        .into_iter()
        .filter(|path| {
            read_settings(path).is_some_and(|val| !foreign_hook_event_names(&val).is_empty())
        })
        .collect()
}

/// Log a launch-time warning for every checkout carrying foreign hook wiring.
///
/// Why: the operator-facing half. Named at launch so the finding lands beside
/// the session it affects.
/// What: calls [`foreign_harness_files`] and emits one `warn!` naming the files.
/// Silent when nothing is found. Never modifies a file.
/// Test: `detects_foreign_hooks_in_launch_dir` covers the detection this logs.
pub(super) fn warn_on_foreign_harness_config(dirs: &[PathBuf]) {
    let found = foreign_harness_files(dirs);
    if found.is_empty() {
        return;
    }
    let listed: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
    warn!(
        files = %listed.join(", "),
        "spawn_managed: this checkout carries another harness's (claude-mpm) hook \
         entries, which will fire inside this tm session alongside tm's own hooks. \
         This is informational — tm never removes another harness's hooks; removing \
         them is your call. `tm doctor` reports the same finding as \
         `hooks_foreign_conflict`."
    );
}

/// The directories a launch-time scan covers.
///
/// Why: a redirected launch has TWO trees worth checking — the managed checkout
/// the session will run in, and the unmanaged directory the operator launched
/// from, which is where inherited framework state actually accumulates. Checking
/// only the session workspace would miss exactly the case that motivated this:
/// a foreign config sitting in the operator's own clone. Pure, so the
/// de-duplication is assertable without a tracing subscriber.
/// What: `[launch_dir]`, plus `workspace` when it differs.
/// Test: `launch_and_workspace_dirs_are_deduplicated`.
pub(super) fn launch_scan_dirs(launch_dir: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![launch_dir.to_path_buf()];
    if workspace != launch_dir {
        dirs.push(workspace.to_path_buf());
    }
    dirs
}

/// Scan and warn for both directories a launch touches.
///
/// Why: the single call the spawn path makes.
/// What: [`launch_scan_dirs`] then [`warn_on_foreign_harness_config`].
/// Test: `detects_foreign_hooks_in_launch_dir`.
pub(super) fn warn_for_launch(launch_dir: &Path, workspace: &Path) {
    warn_on_foreign_harness_config(&launch_scan_dirs(launch_dir, workspace));
}

#[cfg(test)]
#[path = "foreign_harness_tests.rs"]
mod tests;
