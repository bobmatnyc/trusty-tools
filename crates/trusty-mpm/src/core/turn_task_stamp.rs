//! The per-turn Intent Ledger task stamp for a PM session (#11398).
//!
//! Why: the Tasks view attributes a turn to a task by a `Task: <id>` line in
//! the transcript. The owner ruled that the PM harness stamps the current IL
//! task into each turn, and that the PM sets that task through datg when it
//! switches. This module reads datg's state; it never writes it.
//! What: [`state_path`] resolves the datg `state.json` the way `datg.py`
//! does (`$DATG_SESSION`, then the tmux session of `$TMUX_PANE`, then the
//! legacy single directory). [`stamp_from_state`] turns the session
//! record's `current` field into `Task: <id>@<MMDD>` when that id names an
//! active (`open` or `running`) task. [`hook_output`] wraps the stamp as
//! `UserPromptSubmit` `additionalContext`. Every failure yields `None`.
//! Test: `turn_task_stamp_tests.rs`, `tests/tm_hook_turn_task_11398.rs`.

use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};

/// The only hook event the stamp is emitted on.
pub const TURN_TASK_EVENT: &str = "UserPromptSubmit";

/// The datg session-record field naming the PM's current task (#11398).
pub const CURRENT_FIELD: &str = "current";

/// Task statuses datg counts as active (`datg_task.py` `ACTIVE`).
const ACTIVE: [&str; 2] = ["open", "running"];

/// The most of `state.json` the hook reads. datg refuses to write past 64 KB.
const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The bound on the `tmux display-message` lookup.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// datg's `SESSION_RE`: a per-PM directory name.
static SESSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").expect("static session regex")
});

/// `task_tag.py`'s `IL_ID`, anchored: `<code>-T<two base-34 digits>`.
static IL_TASK_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z][a-z0-9]{1,7}-T[0-9a-km-np-z]{2}$").expect("static task-id regex")
});

/// The `<project>/.trusty-mpm/datg` base directory.
fn datg_base(project: &Path) -> PathBuf {
    project.join(".trusty-mpm").join("datg")
}

/// Whether `name` is a session `datg.py` accepts and `base/name/state.json` exists.
fn session_state(base: &Path, name: &str) -> Option<PathBuf> {
    let path = base.join(name).join("state.json");
    (SESSION_RE.is_match(name) && path.is_file()).then_some(path)
}

/// The datg `state.json` for this session, resolved as `datg.py` resolves it.
///
/// Why: the hook must read the same ledger the PM's own `datg.py` calls write,
/// or the stamp names another PM's task.
/// What: `$DATG_SESSION` when valid and its `state.json` exists; else, when
/// any per-PM directory exists, the tmux session `tmux` returns (called only
/// then) when valid and present; else, with no per-PM directory, the legacy
/// `datg/state.json` when present. `None` when nothing resolves.
/// Test: `the_session_resolves_like_datg`.
pub fn state_path(
    project: &Path,
    datg_session: Option<&OsStr>,
    tmux: impl FnOnce() -> Option<String>,
) -> Option<PathBuf> {
    let base = datg_base(project);
    if let Some(found) = datg_session
        .and_then(OsStr::to_str)
        .and_then(|name| session_state(&base, name))
    {
        return Some(found);
    }
    let has_subs = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .any(|entry| entry.path().is_dir() && entry.path().join("state.json").is_file());
    if has_subs {
        return tmux().and_then(|name| session_state(&base, &name));
    }
    let legacy = base.join("state.json");
    legacy.is_file().then_some(legacy)
}

/// The `MMDD` of a task's `created` stamp (`datg_task.py` `task_mmdd`).
fn task_mmdd(task: &Value) -> Option<String> {
    let created = task.get("created")?.as_str()?;
    let b = created.as_bytes();
    let shaped = b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit);
    shaped.then(|| format!("{}{}", &created[5..7], &created[8..10]))
}

/// The stamp line for a datg `state.json` text, or `None`.
///
/// Why: the stamp must name a live task, or the Tasks view books a turn to a
/// closed or invented one.
/// What: `session.current` must be a string matching the IL task-id grammar
/// and the id of a task in `tasks` whose `status` is `open` or `running`.
/// Returns `Task: <id>@<MMDD>` from that task's `created`, or `Task: <id>`
/// for a task with no `created` (both forms `task_tag.py` LOOSE accepts).
/// Malformed JSON, a missing field or a wrong type → `None`.
/// Test: `a_current_open_task_is_stamped`, `each_fail_open_case_is_silent`,
/// `the_stamp_matches_the_task_tag_grammar`.
pub fn stamp_from_state(text: &str) -> Option<String> {
    let state: Value = serde_json::from_str(text).ok()?;
    let current = state.get("session")?.get(CURRENT_FIELD)?.as_str()?;
    if !IL_TASK_ID_RE.is_match(current) {
        return None;
    }
    let task = state.get("tasks")?.as_array()?.iter().find(|t| {
        t.get("id").and_then(Value::as_str) == Some(current)
            && t.get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| ACTIVE.contains(&s))
    })?;
    Some(match task_mmdd(task) {
        Some(mmdd) => format!("Task: {current}@{mmdd}"),
        None => format!("Task: {current}"),
    })
}

/// Read at most [`MAX_STATE_BYTES`] of `path` and stamp it; `None` on any error.
pub fn read_stamp(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_STATE_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    stamp_from_state(&text)
}

/// The `UserPromptSubmit` hook response carrying `stamp` as `additionalContext`.
///
/// Test: `a_current_open_task_is_stamped`.
pub fn hook_output(stamp: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": TURN_TASK_EVENT,
            "additionalContext": stamp,
        }
    })
}

/// The tmux session name of `pane`, bounded by [`TMUX_TIMEOUT`].
///
/// Why: `datg.py` picks a per-PM directory by the tmux session; Claude Code
/// hooks run with a minimal `PATH`, so the Homebrew paths are tried too.
/// What: the first `tmux` that spawns; killed past the timeout. `None` on any
/// failure or an empty answer.
/// Test: `the_session_resolves_like_datg` (through an injected lookup).
pub fn tmux_session(pane: &OsStr) -> Option<String> {
    let mut child = ["tmux", "/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"]
        .iter()
        .find_map(|bin| {
            Command::new(bin)
                .args(["display-message", "-p", "-t"])
                .arg(pane)
                .arg("#{session_name}")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })?;
    let deadline = Instant::now() + TMUX_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(256)
        .read_to_string(&mut out)
        .ok()?;
    let name = out.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
#[path = "turn_task_stamp_tests.rs"]
mod tests;
