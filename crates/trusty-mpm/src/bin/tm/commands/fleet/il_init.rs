//! The Architect's own Intent Ledger and its hooks, on install and upgrade
//! (#10571, #10579).
//!
//! Why: Intent Ledger continuity is standard Architect behaviour. A fresh
//! install, and an upgrade of an install made before the ledger, must end
//! with a ledger in `local/il` and the two hooks that keep it current, without
//! touching ledger state or hooks someone already has.
//! What: [`steps`] runs after `tm fleet init` seeds the scripts.
//! (1) `local/il/.trusty-mpm/datg/state.json` absent: `datg.py init`.
//! (2) `.claude/settings.json` of the Architect directory: a `Stop` and a
//! `SessionStart` entry, no matcher, running
//! `/bin/sh <dir>/scripts/datg-hook.sh <session> <dir>/local/il` with paths
//! written out (the guard refuses computed ones); merged, never replaced; an
//! event that already runs `datg-hook.sh` is left as it is.
//! (3) migration: a `HOLD:` line in `local/state.md` moves into the ledger's
//! typed `hold` field when that field is empty; the old checkpoint wording in
//! `local/usage-policy.md` is replaced, that phrase only.
//! Nothing here gates a context clear.
//! Test: `il_init_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::Step;

/// The Architect's ledger root, below its directory.
pub(crate) const IL_DIR: &str = "local/il";
/// The ledger code for the Architect's own ids (`ar-G01`).
pub(crate) const CODE: &str = "ar";
/// The obsolete checkpoint phrase and its Intent Ledger replacement.
const OLD_CHECKPOINT: &str = "pause snapshot or palace note";
const NEW_CHECKPOINT: &str = "write the IL with datg.py session set / digest";
/// The two placeholders a migration seeds when `local/state.md` names no task
/// or next (df-D5f); `datg.py ready` rejects their `needs owner input:` marker.
const PLACEHOLDER_TASK: &str = "needs owner input: what is the Architect working on?";
const PLACEHOLDER_NEXT: &str = "needs owner input: what is the next step?";
/// The two events the ledger hooks serve.
pub(crate) const EVENTS: [&str; 2] = ["Stop", "SessionStart"];

/// The ledger's state file.
pub(crate) fn state_path(dir: &Path) -> PathBuf {
    dir.join(IL_DIR).join(".trusty-mpm/datg/state.json")
}

/// The exact hook command line for `dir` and `session`.
pub(crate) fn hook_command(dir: &Path, session: &str) -> String {
    let q = |p: PathBuf| {
        let s = p.display().to_string();
        if s.contains([' ', '\'', '"']) {
            format!("'{}'", s.replace('\'', r"'\''"))
        } else {
            s
        }
    };
    format!(
        "/bin/sh {} {session} {}",
        q(dir.join("scripts/datg-hook.sh")),
        q(dir.join(IL_DIR))
    )
}

/// Every step, in order; see the module doc.
pub(crate) fn steps(dir: &Path, session: &str) -> Vec<Step> {
    // #10579: read the HOLD first; hold-like text that cannot be read stops here,
    // before a ledger exists, so no ledger is ever seeded without its HOLD.
    // #10579: only a missing state.md reads as empty; an unreadable one stops here.
    let text = match std::fs::read_to_string(dir.join("local/state.md")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return vec![Step::Failed(format!(
                "local/state.md could not be read ({e}), so setup cannot tell whether a HOLD is \
                 recorded there. Fix the file (it must be readable UTF-8 text) and run tm fleet \
                 init again; the Intent Ledger was not created"
            ))];
        }
    };
    let work = work_in_state_md(&text);
    let holds = match holds_in_state_md(&text) {
        Ok(holds) => holds,
        Err((line, text)) => {
            return vec![Step::Failed(format!(
                "local/state.md line {line} mentions a hold that setup cannot read (\"{text}\"). \
                 Write it as `- HOLD: <what is held>` or `- usage state: HOLD <details>` and run \
                 tm fleet init again; the Intent Ledger was not created, so nothing lost the hold"
            ))];
        }
    };
    // df-D5f: a HOLD with no task/next still migrates, with marked placeholders that
    // `datg.py ready` rejects, so a clear stays blocked until the owner fills them in.
    let mut work = work;
    let mut placeholder_note = None;
    // Critic HIGH: placeholders only for a ledger this run creates; an existing one keeps its own task and next.
    let new_ledger = !state_path(dir).is_file();
    if new_ledger && holds.any() && (work.0.is_none() || work.1.is_none()) {
        work.0.get_or_insert_with(|| PLACEHOLDER_TASK.to_owned());
        work.1.get_or_insert_with(|| PLACEHOLDER_NEXT.to_owned());
        // df-D5f: the owner runs no command; the repair line is operator diagnostics only.
        eprintln!(
            "tm fleet init: {IL_DIR} task/next seeded as placeholders; the Architect sets them \
             with: python3 scripts/datg.py --project {IL_DIR} session set --task ... --next ..."
        );
        placeholder_note = Some(Step::Skipped(
            "the Intent Ledger does not know the current task and next step yet, so clearing \
             the Architect's context stays blocked until the Architect fills them in from your \
             goals and records on its next pass. Nothing is needed from you."
                .to_owned(),
        ));
    }
    let mut out = vec![ledger_step(dir)];
    out.push(hooks_step(dir, session));
    out.extend(holds.any().then(|| hold_step(dir, &holds, &work)).flatten());
    out.extend(placeholder_note);
    out.extend(usage_policy_step(dir));
    out
}

/// Run `datg.py --project <dir>/local/il <args>`; Err with its stderr.
fn datg(dir: &Path, args: &[&str]) -> Result<String, String> {
    let il = dir.join(IL_DIR);
    let out = Command::new("python3")
        .arg(dir.join("scripts/datg.py"))
        .arg("--project")
        .arg(&il)
        .args(args)
        .output()
        .map_err(|e| format!("python3 could not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Create the ledger when it is absent; never touch an existing one.
fn ledger_step(dir: &Path) -> Step {
    if state_path(dir).is_file() {
        return Step::Unchanged(format!("Intent Ledger in {IL_DIR} (kept as is)"));
    }
    if let Err(e) = std::fs::create_dir_all(dir.join(IL_DIR)) {
        return Step::Failed(format!("could not create {IL_DIR}: {e}"));
    }
    match datg(dir, &["init", "--code", CODE]) {
        Ok(_) => Step::Changed(format!("created the Intent Ledger in {IL_DIR}")),
        Err(e) => Step::Failed(format!(
            "could not create the Intent Ledger in {IL_DIR} ({e})"
        )),
    }
}

/// Merge the two ledger hooks into `<dir>/.claude/settings.json`.
fn hooks_step(dir: &Path, session: &str) -> Step {
    let path = dir.join(".claude/settings.json");
    let mut root = match std::fs::read_to_string(&path) {
        Ok(t) if t.trim().is_empty() => serde_json::json!({}),
        Ok(t) => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) if v.is_object() => v,
            _ => {
                return Step::Skipped(format!(
                    ".claude/settings.json is not a JSON object, so the Intent Ledger hooks were \
                     not added; add `{}` under hooks.Stop and hooks.SessionStart by hand",
                    hook_command(dir, session)
                ));
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Step::Failed(format!("cannot read {}: {e}", path.display())),
    };
    let added = merge_hooks(&mut root, &hook_command(dir, session));
    if added.is_empty() {
        return Step::Unchanged("Intent Ledger hooks in .claude/settings.json".to_owned());
    }
    let text = match serde_json::to_string_pretty(&root) {
        Ok(t) => t + "\n",
        Err(e) => return Step::Failed(format!("cannot render .claude/settings.json: {e}")),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match trusty_common::atomic_file::write_atomic(&path, text.as_bytes()) {
        Ok(()) => Step::Changed(format!(
            "added the Intent Ledger hooks ({}) to .claude/settings.json",
            added.join(", ")
        )),
        Err(e) => Step::Failed(format!("cannot write {}: {e}", path.display())),
    }
}

/// Add `command` to each of [`EVENTS`] that does not already run
/// `datg-hook.sh`; returns the events changed. Other keys and entries stay.
pub(crate) fn merge_hooks(root: &mut serde_json::Value, command: &str) -> Vec<&'static str> {
    let mut added = Vec::new();
    let Some(obj) = root.as_object_mut() else {
        return added;
    };
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        return added; // someone else's shape: never replaced
    }
    for event in EVENTS {
        let list = hooks
            .as_object_mut()
            .expect("checked above")
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let Some(arr) = list.as_array_mut() else {
            continue;
        };
        let present = arr.iter().any(|entry| {
            entry["hooks"].as_array().is_some_and(|hs| {
                hs.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("datg-hook.sh"))
                })
            })
        });
        if !present {
            arr.push(serde_json::json!({"hooks": [{"type": "command", "command": command}]}));
            added.push(event);
        }
    }
    added
}

#[path = "il_holds.rs"]
mod holds;
pub(crate) use holds::{Holds, holds_in_state_md};

/// The active usage hold alone (see [`holds_in_state_md`]).
#[cfg(test)]
pub(crate) fn hold_in_state_md(text: &str) -> Result<Option<String>, (usize, String)> {
    holds_in_state_md(text).map(|h| h.usage)
}

/// The `- task:` and `- next:` values in `local/state.md` (also `current task:`,
/// `next action:`, `next step:`), each the first non-empty one.
/// Test: `the_state_md_fixture_of_record_classifies_every_hold`.
pub(crate) fn work_in_state_md(text: &str) -> (Option<String>, Option<String>) {
    let mut task = None;
    let mut next = None;
    for line in text.lines() {
        let t = line.trim_start_matches([' ', '-', '*', '\t']);
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        if value.is_empty() {
            continue;
        }
        if task.is_none() && matches!(key.as_str(), "task" | "current task") {
            task = Some(value.chars().take(160).collect());
        }
        if next.is_none() && matches!(key.as_str(), "next" | "next action" | "next step") {
            next = Some(value.chars().take(200).collect());
        }
    }
    (task, next)
}

/// Move the active holds (with the task and next found beside them) into the
/// ledger: the usage hold into `hold`, the merge hold into `merge_hold`, each
/// only when that field is empty (an existing value wins).
fn hold_step(dir: &Path, holds: &Holds, work: &(Option<String>, Option<String>)) -> Option<Step> {
    let read = std::fs::read_to_string(state_path(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let Some(st) = read else {
        return Some(Step::Failed(
            "could not read the Intent Ledger to record the holds, so they were not recorded; \
             tm fleet init stopped here"
                .to_owned(),
        ));
    };
    let empty = |key: &str| st["session"][key].as_str().is_none_or(str::is_empty);
    // A task or next field the ledger already holds is never overwritten from state.md.
    let open = |key: &str| {
        st["session"][key]
            .as_str()
            .is_none_or(|v| v.trim().is_empty() || v.starts_with("needs owner input:"))
    };
    let mut args = vec!["session", "set"];
    let mut moved = Vec::new();
    if let Some(usage) = holds.usage.as_deref().filter(|_| empty("hold")) {
        args.extend(["--hold", usage]);
        moved.push("the usage HOLD");
    }
    if let Some(merge) = holds.merge.as_deref().filter(|_| empty("merge_hold")) {
        args.extend(["--merge-hold", merge]);
        moved.push("the merge hold");
    }
    if moved.is_empty() {
        return None;
    }
    if let Some(task) = work.0.as_deref().filter(|_| open("task")) {
        args.extend(["--task", task]);
    }
    if let Some(next) = work.1.as_deref().filter(|_| open("next")) {
        args.extend(["--next", next]);
    }
    Some(match datg(dir, &args) {
        Ok(_) => Step::Changed(format!(
            "moved {} from local/state.md into the Intent Ledger (local/state.md is unchanged)",
            moved.join(" and ")
        )),
        Err(e) => Step::Failed(format!(
            "could not record the holds in the Intent Ledger ({e})"
        )),
    })
}

/// Replace the obsolete checkpoint phrase in `local/usage-policy.md`, that phrase only.
fn usage_policy_step(dir: &Path) -> Option<Step> {
    let path = dir.join("local/usage-policy.md");
    let text = std::fs::read_to_string(&path).ok()?;
    if !text.contains(OLD_CHECKPOINT) {
        return None;
    }
    let new = text.replace(OLD_CHECKPOINT, NEW_CHECKPOINT);
    Some(
        match trusty_common::atomic_file::write_atomic(&path, new.as_bytes()) {
            Ok(()) => Step::Changed(
                "local/usage-policy.md: replaced the old checkpoint wording with the Intent Ledger \
                 wording; nothing else in it changed"
                    .to_owned(),
            ),
            Err(e) => Step::Failed(format!("cannot write {}: {e}", path.display())),
        },
    )
}
