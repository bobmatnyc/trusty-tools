//! Project-level settings hygiene: detect and remove tm hook contamination
//! from project `.claude/settings*.json` files (issue #2940).
//!
//! Why: `tm install` used to discover and mutate EVERY `.claude/settings.json`
//! / `settings.local.json` reachable under `$HOME`, wiring tm hooks into every
//! project it found — including projects the operator never launched via tm.
//! That was a one-way street (the project could no longer cleanly go back to
//! claude-mpm, since the tm entries now lived in a file the project owns) and
//! could conflict with a project's own pre-existing claude-mpm hooks. Hooks
//! now live solely in the tm-owned `CLAUDE_CONFIG_DIR` (see
//! [`super::ensure_managed_hooks`]); this module is the cleanup path for
//! settings files a PRE-FIX `tm install` already contaminated, exposed as
//! `tm hooks clean` and as the `tm doctor` hook-hygiene probe.
//! What: [`contains_tm_hooks`] / [`tm_hook_event_names`] detect tm-owned
//! entries; [`foreign_hook_event_names`] detects a DIFFERENT harness's hooks
//! (informational only — never removed); [`clean_settings_file`] performs the
//! actual scan-or-apply pass over one file, always backing up before writing.
//!
//! Known limitations:
//! - **Mixed hook groups (issue #2948, RESOLVED).** [`event_names_matching`]
//!   now classifies a group by `.any()` over its inner `hooks[*].command`
//!   entries, and [`super::strip_hook_entries_matching_for_events`] filters at
//!   PER-ENTRY granularity within a group rather than dropping/keeping the
//!   whole group. A hand-edited group whose inner array mixes ONE tm command
//!   with ONE foreign (e.g. claude-mpm) command is now flagged by both
//!   [`tm_hook_event_names`] and [`foreign_hook_event_names`] (it genuinely
//!   carries both kinds of entry), and `tm hooks clean --force` removes only
//!   the tm-owned entry, leaving the foreign entry and the group itself intact.
//!   Previously (issue #2940 review round 1) the `.all()` classification made
//!   such a group invisible to both `tm doctor` and `tm hooks clean` even
//!   though it carried a live tm entry.
//! - **The hash-suffixed binary matcher trusts any binary under a `deps/`
//!   directory.** [`super::is_mpm_hook_command`]'s hash-suffix branch (via
//!   [`super::is_mpm_hash_suffixed_artifact`]) requires the resolved path to
//!   contain a `deps` path component — narrowing, but not eliminating, the
//!   risk that a coincidentally-named foreign binary (`<stem>-<hexhash>` under
//!   *some* `deps/` directory that isn't actually a tm Cargo build artifact)
//!   is misclassified as tm-owned by `tm hooks clean --force`. The bare-name
//!   branch (`tm`/`trusty-mpm`/`session_manager_mvp`, unscoped by path) carries
//!   the same pre-existing residual risk. Dry-run mode always prints the
//!   exact matched command strings before any deletion (see `commands::hooks::clean`)
//!   so an operator can review before applying `--force`.
//!
//! Test: `cargo test -p trusty-mpm standalone::hooks::cleanup` — see
//! `cleanup_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{is_claude_mpm_hook_command, is_mpm_hook_command, strip_mpm_hook_entries};

/// Per-file outcome of a [`clean_settings_file`] pass.
///
/// Why: `tm hooks clean` needs to report exactly what it found (dry run) or
/// did (`--force`) per file, without re-deriving the event list from the
/// mutated value after the fact.
/// What: the scanned file's path, the `hooks` event keys that carried at
/// least one tm-owned group, and — only when the caller passed `force: true`
/// and a write actually happened — the backup file's path.
/// Test: `clean_settings_file_dry_run_reports_without_writing`,
/// `clean_settings_file_force_writes_backup_and_strips`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanOutcome {
    /// The settings file that was scanned.
    pub path: PathBuf,
    /// `hooks` event keys (e.g. `PreToolUse`) that contained a tm-owned group.
    pub removed_events: Vec<String>,
    /// The backup file written before mutating `path`, when `force` was set.
    pub backup_path: Option<PathBuf>,
}

/// Does this settings value contain at least one tm-owned hook group?
///
/// Why: the fast, read-only predicate both `tm hooks clean`'s dry run and the
/// `tm doctor` contamination check need — scanning without ever risking a
/// write.
/// What: walks every array under the top-level `hooks` object and returns
/// `true` as soon as one group's `hooks[*].command` entries include AT LEAST
/// ONE recognised by [`is_mpm_hook_command`] (issue #2948: entry-level, not
/// whole-group, so a hand-mixed group carrying one tm command alongside a
/// foreign one is still detected — mirroring the matching semantics
/// [`strip_mpm_hook_entries`] uses to decide what to remove).
/// Test: `contains_tm_hooks_true_for_tm_entry`,
/// `contains_tm_hooks_false_for_foreign_or_absent`.
pub fn contains_tm_hooks(val: &Value) -> bool {
    !tm_hook_event_names(val).is_empty()
}

/// The `hooks` event keys that carry at least one tm-owned group.
///
/// Why: [`CleanOutcome::removed_events`] must be computed BEFORE
/// [`strip_mpm_hook_entries`] mutates the value away, and the doctor probe
/// wants the same list for its message.
/// What: returns every event key (e.g. `PreToolUse`, `Stop`) under `hooks`
/// where at least one group carries at least one tm-owned entry (issue #2948:
/// entry-level match, so a hand-mixed group counts). Order follows the JSON
/// object's iteration order; empty when `hooks` is absent or not an object.
/// Test: `tm_hook_event_names_lists_only_contaminated_events`,
/// `tm_hook_event_names_flags_mixed_group`.
pub fn tm_hook_event_names(val: &Value) -> Vec<String> {
    event_names_matching(val, is_mpm_hook_command)
}

/// The `hooks` event keys that carry at least one FOREIGN (claude-mpm) group.
///
/// Why: `tm doctor` must warn about foreign hooks that would fire inside a tm
/// session WITHOUT ever touching them — that call belongs to the operator.
/// What: returns every event key under `hooks` where at least one group
/// carries at least one entry recognised by [`is_claude_mpm_hook_command`]
/// (issue #2948: entry-level match). [`is_mpm_hook_command`] and
/// [`is_claude_mpm_hook_command`] are mutually exclusive PER COMMAND STRING —
/// a single command is never classified as both — but a single EVENT can now
/// appear in both [`tm_hook_event_names`] and this function's result when a
/// hand-mixed group (or two separate groups under the same event) carries one
/// entry of each kind; that is a real, simultaneous contamination-and-conflict
/// state, not a classification bug.
/// Test: `foreign_hook_event_names_lists_claude_mpm_entries`,
/// `foreign_hook_event_names_flags_mixed_group`.
pub fn foreign_hook_event_names(val: &Value) -> Vec<String> {
    event_names_matching(val, is_claude_mpm_hook_command)
}

/// Shared walker behind [`tm_hook_event_names`] / [`foreign_hook_event_names`].
///
/// Why (issue #2948): a hook GROUP is classified by whether ANY of its inner
/// `hooks[*].command` entries match — not ALL of them. The previous `.all()`
/// classification made a hand-mixed group (one tm command + one foreign
/// command in the same matcher group) invisible to both the tm-owned and
/// foreign-owned predicates, since neither ALL-tm nor ALL-foreign held. `.any()`
/// makes the walker symmetric with [`super::strip_hook_entries_matching_for_events`],
/// which also now operates at entry (not group) granularity.
fn event_names_matching(val: &Value, matches_cmd: impl Fn(&str) -> bool) -> Vec<String> {
    let Some(hooks) = val.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        let hit = groups.iter().any(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|inner| {
                    inner.iter().any(|entry| {
                        entry
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(&matches_cmd)
                    })
                })
        });
        if hit {
            names.push(event.clone());
        }
    }
    names
}

/// Scan (and, when `force`, clean) one settings file for tm hook contamination.
///
/// Why: the single write path for `tm hooks clean` — every call backs up
/// before mutating so a bad scan can always be undone by hand, and a missing
/// or non-object file is treated as "nothing to clean" rather than an error
/// (a project with no settings file, or one that failed to parse, has no tm
/// contamination to report). The final write MUST be atomic (issue #2940
/// review round 1, HIGH): a plain truncate-then-write left `settings.json`
/// corrupt — unparseable by Claude Code until a manual `.bak` restore — if
/// the process died mid-write; [`trusty_common::claude_config::write_json_atomic`]
/// writes to a temp file and renames over `path`, matching the atomicity
/// [`super::remove_global_trusty_mpm_hooks`] (mod.rs:353) already uses for
/// the same class of mutation.
/// What: reads `path` (missing file → `Ok(None)`, malformed/non-object JSON →
/// `Ok(None)` — never touched), returns `Ok(None)` when
/// [`contains_tm_hooks`] is `false`. Otherwise computes the contaminated
/// event list, and when `force` is `true`: writes a byte-identical backup of
/// the pre-clean text to `<path>.bak-<unix-epoch-seconds>` (this crate's own
/// backup, reported via [`CleanOutcome::backup_path`]; `write_json_atomic`
/// additionally maintains its own internal `<path>.bak`, a harmless second
/// safety net), strips the tm-owned groups via [`strip_mpm_hook_entries`],
/// and atomically writes the result back (preserving every other key). In
/// dry-run mode (`force: false`) the file is never touched and `backup_path`
/// is `None`.
/// Test: `clean_settings_file_dry_run_reports_without_writing`,
/// `clean_settings_file_force_writes_backup_and_strips`,
/// `clean_settings_file_preserves_non_tm_keys`,
/// `clean_settings_file_missing_file_is_noop`,
/// `clean_settings_file_malformed_json_is_noop`,
/// `clean_settings_file_non_object_json_is_noop`.
pub fn clean_settings_file(path: &Path, force: bool) -> anyhow::Result<Option<CleanOutcome>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::anyhow!("read {}: {e}", path.display()));
        }
    };
    let Ok(mut val) = serde_json::from_str::<Value>(&text) else {
        return Ok(None);
    };
    if !val.is_object() {
        return Ok(None);
    }

    let removed_events = tm_hook_event_names(&val);
    if removed_events.is_empty() {
        return Ok(None);
    }

    let mut backup_path = None;
    if force {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bak = PathBuf::from(format!("{}.bak-{ts}", path.display()));
        std::fs::write(&bak, &text)
            .map_err(|e| anyhow::anyhow!("write backup {}: {e}", bak.display()))?;

        let changed = strip_mpm_hook_entries(&mut val);
        debug_assert!(changed, "removed_events was non-empty but nothing stripped");
        trusty_common::claude_config::write_json_atomic(path, &val)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
        backup_path = Some(bak);
    }

    Ok(Some(CleanOutcome {
        path: path.to_path_buf(),
        removed_events,
        backup_path,
    }))
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod tests;
