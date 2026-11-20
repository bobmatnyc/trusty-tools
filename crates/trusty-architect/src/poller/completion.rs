//! Task-complete refresh: one `ctx` alert, fingerprint `complete`, when a
//! session's task is done (df-T7i P1b).
//!
//! Why: Bob 2026-11-05 18:59Z, "if a task is complete, pre-emptively refresh
//! with the assumption that we don't need live context any more", even below
//! the context bar.
//! What: [`completion_alerts`] fires on the first poll where the prompt is
//! empty or a suggestion, nothing runs (no turn, agent or dialog), there is
//! no usage-limit notice, the session's Intent Ledger shows nothing open
//! ([`il_task_complete_for`], read-only), and the context is at least
//! [`COMPLETE_MIN_CTX_PCT`]. `state["complete"][session]` keeps it from
//! refiring until the session stops being complete. The IL check reads the
//! schedule (`IL_SCHEDULE`) entry named for the session and its `state.json`;
//! anything unreadable is "not complete" (fail closed).
//! Test: `test_completion_fires_once_per_completion`,
//! `test_completion_never_with_draft_agent_or_open_il` (tests/p1b_ported.rs),
//! `completion_matches_python_goldens` (tests/parity_p1b.rs),
//! `il_check_fails_closed_on_unreadable_records`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::classify::{Alert, BoxReading, PaneInfo, alert};
use crate::input::InputState;

/// The least context a completion refresh needs (150k held; ctx-watch's
/// floor), so a just-cleared session does not loop.
pub const COMPLETE_MIN_CTX_PCT: u64 = 15;

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Python truthiness of an optional JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `ctx-watch.state_path_for(entry)`: `state_path`, else
/// `<project>/.trusty-mpm/datg/<session>/state.json` when that file exists,
/// else `<project>/.trusty-mpm/datg/state.json`.
fn state_path_for(entry: &Map<String, Value>) -> Option<PathBuf> {
    if let Some(p) = entry.get("state_path").filter(|v| truthy(Some(v))) {
        return p.as_str().map(PathBuf::from);
    }
    let project = entry.get("project")?.as_str()?;
    let name = entry.get("name")?.as_str()?;
    let base = Path::new(project).join(".trusty-mpm/datg");
    let per = base.join(name);
    let dir = if per.join("state.json").is_file() {
        per
    } else {
        base
    };
    Some(dir.join("state.json"))
}

/// `il_complete_for(session)`: true when the schedule names `session` and
/// its IL shows no session task, owed item or agent and every task is
/// `done`. Anything missing or malformed is false.
pub fn il_task_complete_for(schedule: &Path, session: &str) -> bool {
    let Some(sched) = read_json(schedule) else {
        return false;
    };
    let entries = sched
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(entry) = entries
        .iter()
        .filter_map(Value::as_object)
        .find(|e| e.get("name").and_then(Value::as_str) == Some(session))
    else {
        return false;
    };
    let Some(Value::Object(st)) = state_path_for(entry).and_then(|p| read_json(&p)) else {
        return false;
    };
    let empty = Map::new();
    let ses = st
        .get("session")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if ["task", "owed", "agents"]
        .iter()
        .any(|k| truthy(ses.get(*k)))
    {
        return false;
    }
    let tasks = match st.get("tasks") {
        Some(Value::Array(t)) => t.clone(),
        v if !truthy(v) => Vec::new(),
        _ => return false,
    };
    tasks
        .iter()
        .all(|t| t.get("status").and_then(Value::as_str) == Some("done"))
}

/// `completion_alerts(state, session, pane, info, now, il_check)`.
pub fn completion_alerts(
    other: &mut Map<String, Value>,
    session: &str,
    pane: &str,
    info: &PaneInfo,
    now: f64,
    il_check: &dyn Fn(&str) -> bool,
) -> Vec<Alert> {
    let mut book = match other.remove("complete") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let idle_box = matches!(
        info.box_reading,
        BoxReading::Input(InputState::Empty | InputState::Suggestion)
    );
    let done = info.has_box
        && idle_box
        && !info.busy
        && info.agents == 0
        && info.dialog.is_none()
        && info.limit.is_none()
        && il_check(session);
    let mut out = Vec::new();
    if !done {
        book.remove(session);
    } else if !truthy(book.get(session))
        && let Some(ctx) = info.ctx.filter(|c| *c >= COMPLETE_MIN_CTX_PCT)
    {
        book.insert(session.to_owned(), Value::Bool(true));
        let detail = format!(
            "task complete (IL clean, prompt empty, no agents), ctx {ctx}%: refresh from checkpoint"
        );
        out.push(alert("ctx", "complete", &detail, session, pane, now));
    }
    other.insert("complete".to_owned(), Value::Object(book));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_check_fails_closed_on_unreadable_records() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let sched = dir.path().join("il-schedule.json");
        assert!(!il_task_complete_for(&sched, "tm-a"), "no schedule");
        fs::write(&sched, "not json")?;
        assert!(!il_task_complete_for(&sched, "tm-a"), "bad schedule");
        let state = dir.path().join("state.json");
        let entry = format!(
            "{{\"sessions\": [{{\"name\": \"tm-a\", \"state_path\": {:?}}}]}}",
            state.display().to_string()
        );
        fs::write(&sched, entry)?;
        assert!(!il_task_complete_for(&sched, "tm-a"), "no state file");
        fs::write(
            &state,
            "{\"session\": {}, \"tasks\": [{\"status\": \"done\"}]}",
        )?;
        assert!(il_task_complete_for(&sched, "tm-a"));
        assert!(!il_task_complete_for(&sched, "tm-b"), "not scheduled");
        fs::write(&state, "{\"session\": {\"owed\": [\"x\"]}}")?;
        assert!(!il_task_complete_for(&sched, "tm-a"), "owed item");
        Ok(())
    }
}
