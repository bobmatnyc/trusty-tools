//! Repair a persisted hook or `statusLine` command that points into a Cargo
//! build tree, by repointing it at the installed binary (issue #7262).
//!
//! Why: #7286 taught `tm doctor` to SEE this corruption and stopped there.
//! `tm doctor --fix` planned zero repairs for `hooks_build_tree_binary`, and on
//! 2026-09-10 eight projects were repaired by hand with `sed`. The removal path
//! that already exists is the wrong remedy here twice over: it takes PM
//! enforcement offline until the project's next managed launch, and it cannot
//! touch `statusLine.command` at all, because the hooks writer does not own that
//! key and a strip would leave the operator with no statusline rather than a
//! working one. Repointing has neither problem — the entry the operator already
//! has keeps its argv and gains a path that exists.
//!
//! What: [`repoint_settings_file`] rewrites every command
//! [`super::build_tree`] claims in one `.claude/settings*.json`, taking a
//! timestamped snapshot ([`super::backup::snapshot_then_prune`], the same
//! convention #7244's writer fix uses) before it writes, and writing atomically.
//! [`build_tree_commands_in`] is the read-only probe the repair driver uses to
//! decide whether a file is worth reporting when no installed binary could be
//! resolved.
//!
//! **Fail-closed, everywhere.** A settings file that cannot be read, cannot be
//! parsed, is not a JSON object, or cannot be snapshotted returns `Err` and is
//! NOT rewritten — no backup is taken for an unparseable file, because there is
//! nothing this module could write back that would preserve its contents. That
//! is the opposite of [`super::cleanup::clean_settings_file`], which answers
//! `Ok(None)` for a malformed file: `clean` is also the doctor probe's scanner
//! and must tolerate a file it does not own, whereas this is a repair the
//! operator asked for and a silent skip would read as "nothing was wrong".
//!
//! **Idempotent by construction.** A repointed command names an installed
//! binary, which [`super::build_tree::is_build_tree_hook_command`] rejects, so a
//! second pass finds nothing, returns `Ok(None)`, and writes nothing at all —
//! not even a snapshot.
//!
//! Test: `repoint_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::backup::{HOOK_SETTINGS_SNAPSHOTS_KEPT, snapshot_then_prune};
use super::build_tree::{repointed_hook_command, repointed_statusline_command};
use super::cleanup::{build_tree_hook_commands, build_tree_statusline_command};

/// What one [`repoint_settings_file`] pass found — or changed — in one file.
///
/// Why: the repair driver prints one line per file and must name the exact
/// commands, because the operator's decision ("is that really tm's entry?")
/// depends on the string and not on a count. Reporting the before/after pair
/// rather than only the new value is what lets a dry run be read as a diff.
/// What: the file, every `(old, new)` hook-command pair, the `statusLine` pair
/// when that key was affected, and — only when `force` was set — the snapshot
/// taken before the rewrite.
/// Test: `repoint_settings_file_dry_run_changes_nothing`,
/// `repoint_settings_file_applies_and_snapshots`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepointOutcome {
    /// The settings file that was scanned.
    pub path: PathBuf,
    /// Every `hooks.<event>[*].hooks[*].command` rewritten, as `(old, new)`.
    pub hooks: Vec<(String, String)>,
    /// The `statusLine.command` rewrite, as `(old, new)`, when there was one.
    pub statusline: Option<(String, String)>,
    /// The snapshot written before mutating `path`, when `force` was set.
    pub backup_path: Option<PathBuf>,
}

impl RepointOutcome {
    /// How many commands this pass rewrote.
    ///
    /// Why: the driver's one-line summary counts commands, not files.
    /// What: the hook rewrites plus the `statusLine` one.
    /// Test: `repoint_settings_file_applies_and_snapshots`.
    pub fn command_count(&self) -> usize {
        self.hooks.len() + usize::from(self.statusline.is_some())
    }
}

/// Every build-tree command in `path`, hooks and `statusLine` alike (#7262).
///
/// Why: when no installed binary can be resolved there is nothing to repoint
/// to, and the driver must still say WHICH files carry the damage rather than
/// falling silent. Answering that from the same classifiers the repair uses
/// keeps the refusal and the repair talking about the same set.
/// What: the hook commands then the `statusLine` command, in that order. Empty
/// for a file that is missing, unreadable, unparseable, or clean — this is a
/// "should I speak up" probe, and every one of those answers is "no". The
/// repair itself reports a read or parse failure; see [`repoint_settings_file`].
/// Test: `build_tree_commands_in_lists_the_incident_commands`,
/// `build_tree_commands_in_is_empty_for_an_unparseable_file`.
pub fn build_tree_commands_in(path: &Path) -> Vec<String> {
    let Some(val) = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
    else {
        return Vec::new();
    };
    let mut found = build_tree_hook_commands(&val);
    found.extend(build_tree_statusline_command(&val));
    found
}

/// Repoint every build-tree command in one settings file at `installed`.
///
/// Why: see the module doc — this is the `--fix` arm `hooks_build_tree_binary`
/// lacked, and the reason the 2026-09-10 repair had to be done by hand. The
/// read-modify-write below takes no cross-process lock, so a settings file
/// written concurrently by another process loses one side's edit; that limit is
/// shared with every other writer of a `.claude/settings.json` in this crate —
/// [`super::write_project_hooks`] and [`super::cleanup::clean_settings_file`].
/// What: reads `path` (missing → `Ok(None)`), parses it, and rewrites every
/// command [`repointed_hook_command`] / [`repointed_statusline_command`]
/// claims. Returns `Ok(None)` when nothing matched, which is also what a second
/// pass over an already-repaired file returns. With `force`, snapshots `path`
/// to `<name>.<YYYYMMDDTHHMMSSZ>.bak` FIRST and then writes atomically via
/// [`trusty_common::claude_config::write_json_atomic`]; in dry run the file is
/// never opened for writing and `backup_path` is `None`.
///
/// Fail-closed on every failure it can reach: a read error other than
/// `NotFound`, unparseable JSON, a non-object document, or a snapshot that
/// could not be taken each return `Err` with nothing written and no backup
/// left behind. `installed` is validated once, up front — an `installed` that
/// is itself an ephemeral build path is refused rather than written, so this
/// repair can never persist the corruption it exists to remove.
/// Test: `repoint_settings_file_applies_and_snapshots`,
/// `repoint_settings_file_dry_run_changes_nothing`,
/// `repoint_settings_file_is_idempotent`,
/// `repoint_settings_file_refuses_unparseable_json`,
/// `repoint_settings_file_refuses_a_non_object_document`,
/// `repoint_settings_file_refuses_an_ephemeral_installed_binary`,
/// `repoint_settings_file_leaves_foreign_entries_alone`.
pub fn repoint_settings_file(
    path: &Path,
    installed: &Path,
    force: bool,
) -> anyhow::Result<Option<RepointOutcome>> {
    // #7262: writing a build-tree path back would recreate the exact damage.
    if !installed.is_absolute() || trusty_common::bin_resolve::is_ephemeral_build_path(installed) {
        anyhow::bail!(
            "refusing to repoint at {} — it is not an installed absolute binary path",
            installed.display()
        );
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => anyhow::bail!("read {}: {e}", path.display()),
    };
    // Fail-closed: an unparseable file is reported, never rewritten and never
    // backed up — a snapshot of a file we cannot restore buys nothing.
    let mut val: Value = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) — refusing to rewrite it; no backup was taken",
            path.display()
        )
    })?;
    if !val.is_object() {
        anyhow::bail!(
            "{} is valid JSON but not an object — refusing to rewrite it",
            path.display()
        );
    }

    let hooks = rewrite_hook_commands(&mut val, installed);
    let statusline = rewrite_statusline_command(&mut val, installed);
    if hooks.is_empty() && statusline.is_none() {
        return Ok(None);
    }

    let mut backup_path = None;
    if force {
        backup_path = snapshot_then_prune(path, HOOK_SETTINGS_SNAPSHOTS_KEPT)
            .map_err(|e| anyhow::anyhow!("snapshot {}: {e}", path.display()))?;
        trusty_common::claude_config::write_json_atomic(path, &val)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    }

    Ok(Some(RepointOutcome {
        path: path.to_path_buf(),
        hooks,
        statusline,
        backup_path,
    }))
}

/// Rewrite every build-tree `hooks.<event>[*].hooks[*].command` in place.
///
/// Why: the read-side walker in [`super::cleanup`] hands out `&str`, which
/// cannot be assigned through. This is the same four-level traversal with
/// mutable access, kept beside the only mutation that needs it rather than
/// widening the shared walker to a shape one of its two callers cannot use.
/// What: replaces each claimed command with its repointed form and returns the
/// `(old, new)` pairs in document order. A level whose JSON shape is not the
/// expected object/array is skipped, exactly as the read-side walker skips it.
/// Test: `repoint_settings_file_applies_and_snapshots`,
/// `repoint_settings_file_leaves_foreign_entries_alone`.
fn rewrite_hook_commands(val: &mut Value, installed: &Path) -> Vec<(String, String)> {
    let mut changed = Vec::new();
    let Some(events) = val.get_mut("hooks").and_then(Value::as_object_mut) else {
        return changed;
    };
    for groups in events.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for entry in entries {
                let Some(cmd) = entry.get("command").and_then(Value::as_str) else {
                    continue;
                };
                let Some(new) = repointed_hook_command(cmd, installed) else {
                    continue;
                };
                changed.push((cmd.to_string(), new.clone()));
                entry["command"] = Value::String(new);
            }
        }
    }
    changed
}

/// Rewrite a build-tree `statusLine.command` in place (#7262, #4492).
///
/// Why: the corruption reaches this key too, and #7286 could only name it. The
/// key is a single string rather than a nested array, so it gets its own
/// two-line rewrite instead of a branch inside [`rewrite_hook_commands`].
/// What: `Some((old, new))` when [`repointed_statusline_command`] claims the
/// current value; `None` for any other shape or a missing key.
/// Test: `repoint_settings_file_applies_and_snapshots`,
/// `repoint_settings_file_ignores_an_installed_statusline`.
fn rewrite_statusline_command(val: &mut Value, installed: &Path) -> Option<(String, String)> {
    let cmd = val.get("statusLine")?.get("command")?.as_str()?;
    let new = repointed_statusline_command(cmd, installed)?;
    let old = cmd.to_string();
    val["statusLine"]["command"] = Value::String(new.clone());
    Some((old, new))
}

#[cfg(test)]
#[path = "repoint_tests.rs"]
mod tests;
