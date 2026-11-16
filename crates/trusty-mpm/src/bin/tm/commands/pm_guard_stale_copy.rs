//! The stale-copy guard: replacing writes to registered artifacts (#10574).
//!
//! Why: the PM rebuilt a document from its own copy and lost an unmarked owner
//! edit. `pm_guard_write_boundary` never content-checks a `.md` write, and the
//! Claude Code `Write` staleness check does not cover a Bash redirect.
//! What: [`evaluate_stale_copy`] runs for every `Read`, edit tool and `Bash`
//! call when the project's `.trusty-mpm.toml` has a `[stale_copy]` table
//! (owner ruling Dr8 B; see `trusty_mpm::core::stale_copy`).
//!
//! * A `Read` of a registered artifact records its observed version.
//! * A write to a `locked` artifact is denied `LOCKED`, whatever the hash.
//! * A replacing write (edit tool, or a Bash write target from
//!   [`shell_write_targets`]) to a registered artifact is denied
//!   `STALE_INPUT_HASH` when the file differs from its record, has no record,
//!   or the store cannot be read. Creating a file that does not exist replaces
//!   nothing and is allowed.
//! * An allowed `Write` records its `content`; an allowed `Edit`/`MultiEdit`
//!   records the edited text when the edit applies; any other allowed write
//!   (a Bash write, a `NotebookEdit`) drops the record, so the next replacing
//!   write needs a fresh `Read`. The record is written before the tool runs,
//!   so a write the operator then refuses leaves a record the file does not
//!   match, which denies and asks for a re-read: the error runs toward deny.
//!
//! Fail-closed only where the guard is on: an inert or rejected config
//! (missing, empty, unreadable) allows every call; a rejected one prints a
//! warning to stderr. Residual: a Bash read (`cat docs/x.md`) records nothing,
//! so a later write after only a Bash read is denied as never read.
//!
//! Coverage gaps: [`shell_write_targets`] does not see every Bash write shape.
//! The guard does not judge `perl -i`, `awk -i inplace`, `patch`, `git apply`,
//! `git checkout -- <file>`, `ln`, `curl -o`, a write made inside a script the
//! command runs, or a target spelled through `$VAR` or `~`. Such a write is
//! allowed unchecked; these are known gaps, not judged writes.
//! Test: `pm_guard_stale_copy_tests.rs`; end to end
//! `tests/tm_hook_pm_guard_stale_copy_10574.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::guard_policy::{self, GuardPolicy};
use trusty_mpm::core::stale_copy::{
    Artifact, Freshness, ObservedStore, StaleCopyConfig, StaleCopyState, stale_copy_for,
};

use super::pm_guard::{EDIT_TOOLS, edit_tool_target_path};
use super::pm_guard_bash::shell_write_targets;
use super::pm_guard_deny_log::DenyContext;
use super::pm_guard_floor::ArchitectGate;
use super::pm_guard_refusal::refuse;

/// Deny-code prefix of a stale or unjudgeable replacing write.
pub(crate) const STALE_INPUT_HASH: &str = "STALE_INPUT_HASH";
/// Deny-code prefix of a write to a locked artifact.
pub(crate) const LOCKED: &str = "LOCKED";

/// One stale-copy refusal.
#[derive(Debug)]
pub(crate) struct StaleCopyDeny {
    /// The policy entry the refusal names.
    pub(crate) policy: &'static GuardPolicy,
    /// The reason text, led by the deny code.
    pub(crate) reason: String,
}

/// The `pm_guard` call site: refuse and answer `true` on a deny.
///
/// Why: binds the PM and every agent, so `pm_guard` calls it ahead of Guards 1
/// and 4, like the write boundary; the process-bound Architect is exempt.
/// What: [`evaluate_stale_copy`] on the payload's tool call and `cwd` (else
/// the process cwd); a deny is refused through its policy entry.
/// Test: `tests/tm_hook_pm_guard_stale_copy_10574.rs`.
pub(crate) async fn gate(
    refused: &DenyContext<'_>,
    payload: &Value,
    architect: &ArchitectGate<'_>,
) -> bool {
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(deny) = evaluate_stale_copy(tool_name, payload.get("tool_input"), &cwd) else {
        return false;
    };
    if architect.is_architect() {
        return false;
    }
    refuse(refused, deny.policy, &deny.reason).await;
    true
}

/// The guard's verdict for one tool call, resolving config from `cwd`.
///
/// What: `None` (allow) for any tool but `Read`, an edit tool or `Bash`, and
/// whenever the guard is inert or rejected; otherwise [`evaluate_with`].
/// Test: `a_rejected_config_allows_and_stays_inactive`.
pub(crate) fn evaluate_stale_copy(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> Option<StaleCopyDeny> {
    if tool_name != "Read" && tool_name != "Bash" && !EDIT_TOOLS.contains(&tool_name) {
        return None;
    }
    match stale_copy_for(cwd) {
        StaleCopyState::Inert => None,
        StaleCopyState::Rejected(reason) => {
            eprintln!(
                "trusty-mpm: stale-copy guard inactive (#10574): {reason}; \
                 no artifact is registered until the config is fixed"
            );
            None
        }
        StaleCopyState::Active(cfg) => {
            let store = ObservedStore::for_project(cfg.project_dir());
            evaluate_with(&cfg, &store, tool_name, tool_input, cwd)
        }
    }
}

/// The verdict against an explicit config and store.
///
/// Test: the `pm_guard_stale_copy_tests.rs` suite.
pub(crate) fn evaluate_with(
    cfg: &StaleCopyConfig,
    store: &ObservedStore,
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> Option<StaleCopyDeny> {
    if tool_name == "Read" {
        if let Some(art) = edit_tool_target_path(tool_input).and_then(|p| cfg.classify(p, cwd)) {
            observe(store, &art);
        }
        return None;
    }
    if EDIT_TOOLS.contains(&tool_name) {
        let art = edit_tool_target_path(tool_input).and_then(|p| cfg.classify(p, cwd))?;
        if let Some(deny) = judge(store, &art) {
            return Some(deny);
        }
        after_edit_allow(store, &art, tool_name, tool_input);
        return None;
    }
    let command = tool_input?.get("command")?.as_str()?;
    let is_dir = |t: &str| cwd.join(t).is_dir();
    // An unplaceable write is the write boundary's to refuse (#8730).
    let targets = shell_write_targets(command, &is_dir).ok()?;
    let arts: Vec<Artifact> = targets
        .iter()
        .filter(|t| !t.starts_with('$') && !t.starts_with('~'))
        .filter_map(|t| cfg.classify(t, cwd))
        .collect();
    for art in &arts {
        if let Some(deny) = judge(store, art) {
            return Some(deny);
        }
    }
    for art in &arts {
        if let Err(e) = store.forget(&art.abs) {
            eprintln!("trusty-mpm: stale-copy record not dropped (#10574): {e}");
        }
    }
    None
}

/// Record the on-disk bytes of `art` as observed; a failure is logged only,
/// because the next write will then be denied as never read.
fn observe(store: &ObservedStore, art: &Artifact) {
    match std::fs::read(&art.abs) {
        Ok(bytes) => {
            if let Err(e) = store.record(&art.abs, &bytes) {
                eprintln!("trusty-mpm: stale-copy record not written (#10574): {e}");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "trusty-mpm: stale-copy guard could not read {} (#10574): {e}",
            art.abs.display()
        ),
    }
}

/// The deny for a write to `art`, or `None` when it may proceed.
fn judge(store: &ObservedStore, art: &Artifact) -> Option<StaleCopyDeny> {
    if art.locked {
        return Some(StaleCopyDeny {
            policy: &guard_policy::LOCKED_PATH_WRITE,
            reason: format!(
                "{LOCKED}: `{}` is a locked artifact in this project's `[stale_copy] locked` \
                 list (#10574); no tool may write it. Ask the owner to edit it or to unlock it.",
                art.rel
            ),
        });
    }
    let why = match store.freshness(&art.abs) {
        Freshness::Fresh | Freshness::Absent => return None,
        Freshness::NeverRead => "has no observed version: it was never read in this project",
        Freshness::Changed { .. } => "changed on disk since it was last read",
        Freshness::Unreadable(ref e) => {
            return Some(stale(art, &format!("cannot be judged: {e}")));
        }
    };
    Some(stale(art, why))
}

fn stale(art: &Artifact, why: &str) -> StaleCopyDeny {
    StaleCopyDeny {
        policy: &guard_policy::STALE_INPUT_HASH,
        reason: format!(
            "{STALE_INPUT_HASH}: `{}` {why} (#10574). A replacement built from an older copy \
             loses edits made since. Read the file again, merge any change you did not make \
             (`git merge-file <current> <sent-copy> <your-version>`), then write.",
            art.rel
        ),
    }
}

/// Update the record for an allowed edit-tool write to `art`.
fn after_edit_allow(store: &ObservedStore, art: &Artifact, tool: &str, input: Option<&Value>) {
    let next = match tool {
        "Write" => input
            .and_then(|i| i.get("content"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        "Edit" | "MultiEdit" => std::fs::read_to_string(&art.abs)
            .ok()
            .and_then(|current| apply_edits(&current, tool, input?)),
        _ => None,
    };
    let result = match next {
        Some(text) => store.record(&art.abs, text.as_bytes()).map(|_| ()),
        None => store.forget(&art.abs),
    };
    if let Err(e) = result {
        eprintln!("trusty-mpm: stale-copy record not updated (#10574): {e}");
    }
}

/// The text after an `Edit`/`MultiEdit`, or `None` when an edit does not apply.
///
/// Test: `apply_edits_follows_replace_all_and_misses`.
pub(crate) fn apply_edits(current: &str, tool: &str, input: &Value) -> Option<String> {
    let edits: Vec<&Value> = if tool == "MultiEdit" {
        input.get("edits")?.as_array()?.iter().collect()
    } else {
        vec![input]
    };
    let mut text = current.to_string();
    for edit in edits {
        let old = edit.get("old_string")?.as_str()?;
        let new = edit.get("new_string")?.as_str()?;
        if old.is_empty() || !text.contains(old) {
            return None;
        }
        let all = edit
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        text = if all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
    }
    Some(text)
}

#[cfg(test)]
#[path = "pm_guard_stale_copy_tests.rs"]
mod tests;
