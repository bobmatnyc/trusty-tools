//! Re-merge the project-tier tm hook groups into an EXISTING
//! `.claude/settings.json` on every resume and relaunch (issue #7490).
//!
//! Why: [`super::prepare_session`] is the ONLY caller of
//! [`super::settings::write_project_hooks`], and no resume path reaches it —
//! `runtime::claude_code::spawn_resume` and the bare-`tm` in-place relaunch
//! both compose a session through `prepare_managed_config`, which provisions
//! the tm-owned `CLAUDE_CONFIG_DIR` and regenerates
//! `INSTRUCTIONS-COMPILED.md` but never touches the project's own
//! `.claude/settings.json`. A project provisioned before an event group
//! existed therefore never gained it: on session `trusty-tools-c4` the file's
//! `SessionStart` array carried only `trusty-memory inbox-check`, so
//! `tm hook`'s `SessionStart` arm never fired, no `savings.jsonl` row was
//! written under the live Claude session id, and the 💸 statusline segment
//! stayed hidden behind `SavingsTotal::is_zero`.
//! What: [`ensure_project_hooks`] re-derives the same manifest/config layers
//! `prepare_session_inner` resolves and calls the SAME
//! [`super::settings::write_project_hooks`] merge — no second merge
//! implementation, so the #7244 no-op exit (a file that already carries every
//! group is left byte-identical) and the snapshot-before-write rule come
//! along unchanged. [`missing_lifecycle_hook_events`] is the read-only
//! predicate `tm doctor`'s `hooks_missing_tm_group` check and its `--fix`
//! repair share.
//! Test: `resume_hooks_tests.rs`.

use std::path::Path;

use serde_json::Value;

use super::PrepError;
use super::project_hooks::is_project_managed_hook_command;
use super::settings::write_project_hooks;
use crate::core::standalone::hooks::MPM_LIFECYCLE_HOOK_EVENTS;
use crate::core::standalone::hooks::cleanup::{event_names_matching, tm_hook_event_names};

/// Merge the project-tier tm hook groups into `<project_dir>/.claude/settings.json`.
///
/// Why (#7490): see the module doc — this is the call every resume and
/// relaunch path was missing. It exists as its own entry point rather than as
/// a second `write_project_hooks` call site so the manifest/config
/// re-derivation those paths need (they hold no `HarnessPlan`, exactly like
/// `core::mcp_session_env`) lives in one place.
/// What: resolves [`crate::core::paths::FrameworkPaths::for_managed_workspace`],
/// reads `[hooks] prompt_context` from [`crate::core::config::MpmConfig`] and
/// `[divert] enabled` from the re-resolved plan, then delegates to
/// [`write_project_hooks`] — which strips tm's own prior entries, deep-merges
/// the additions, returns without writing when the result equals the file as
/// read, and otherwise snapshots before the atomic write. Foreign entries
/// (the `trusty-memory inbox-check` under `SessionStart`) survive: the strip
/// is by tm-owned identity, never by event key.
/// `exe_override` pins the hook binary; production passes `None` and lets
/// `resolve_stable_hook_exe` find the installed one, which refuses a build
/// artifact and so writes nothing (#7244).
/// Test: `resume_merge_adds_the_sessionstart_group_to_an_existing_file`,
/// `resume_merge_preserves_the_inbox_check_entry`,
/// `resume_merge_leaves_a_complete_file_byte_identical`.
pub fn ensure_project_hooks(
    project_dir: &Path,
    exe_override: Option<&Path>,
) -> Result<(), PrepError> {
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(project_dir);
    ensure_project_hooks_with(&fw, project_dir, exe_override)
}

/// [`ensure_project_hooks`] with the framework layout supplied explicitly.
///
/// Why (#5040's seam, reused): `FrameworkPaths::for_managed_workspace` reads
/// `$HOME`, and the two layers resolved from it — `[hooks] prompt_context` and
/// `[divert] enabled` — decide which event keys get written. A test asserting
/// byte-identity across two calls would therefore read the operator's real
/// config, and would see the two answers DIFFER if any concurrently-running
/// test redirected the process-global `$HOME` between them.
/// What: the body of [`ensure_project_hooks`], taking the layout rather than
/// deriving it.
/// Test: `resume_merge_leaves_a_complete_file_byte_identical`.
pub(crate) fn ensure_project_hooks_with(
    fw: &crate::core::paths::FrameworkPaths,
    project_dir: &Path,
    exe_override: Option<&Path>,
) -> Result<(), PrepError> {
    let config = crate::core::config::MpmConfig::load(&fw.root);
    let plan = crate::core::mcp_session_env::resolve_plan(fw, project_dir);
    write_project_hooks(
        project_dir,
        exe_override,
        config.hooks.prompt_context,
        plan.divert_enabled,
    )
}

/// The lifecycle hook events a tm-provisioned settings file is missing.
///
/// Why (#7490): `tm doctor` has to say WHICH event is unwired — the incident
/// was one missing `SessionStart` group in a file that otherwise looked
/// complete, and a file-counting report cannot express that. Read-only, so
/// the diagnostic stays a diagnostic; the repair calls
/// [`ensure_project_hooks`].
/// What: empty for a settings value tm has never provisioned (no
/// `trusty-memory` / PM-guard / lifecycle / divert command anywhere in it) —
/// a project that does not run tm sessions owes no hook group, and adding one
/// would undo what `hooks_contamination`'s repair just removed. Otherwise
/// every [`MPM_LIFECYCLE_HOOK_EVENTS`] entry whose array carries no tm-owned
/// command, in the canonical six-event order.
/// Test: `missing_events_names_a_sessionstart_gap`,
/// `missing_events_is_empty_for_a_complete_file`,
/// `missing_events_is_empty_for_a_file_tm_never_provisioned`.
pub fn missing_lifecycle_hook_events(val: &Value) -> Vec<String> {
    // #7490: an untouched foreign project is not a gap — see the doc above.
    if event_names_matching(val, is_project_managed_hook_command).is_empty() {
        return Vec::new();
    }
    let present = tm_hook_event_names(val);
    MPM_LIFECYCLE_HOOK_EVENTS
        .iter()
        .filter(|event| !present.iter().any(|seen| seen == *event))
        .map(|event| (*event).to_string())
        .collect()
}

#[cfg(test)]
#[path = "resume_hooks_tests.rs"]
mod tests;
