//! End-to-end proof of `tm hook --turn-task` (#11398).
//!
//! Why: only the built binary proves the hook reads datg's `current` field,
//! answers Claude Code in the `additionalContext` shape, and stays silent and
//! exit-0 on every fail-open path.
//! What: spawns the hook with a scratch `$HOME`, `CLAUDE_PROJECT_DIR` naming a
//! temp project holding `.trusty-mpm/datg/pm-a/state.json`, `DATG_SESSION=pm-a`
//! and no daemon (the hook never relays), and pipes a `UserPromptSubmit` payload in.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_turn_task_11398::`.

use crate::common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// Write the datg state for session `pm-a` under `project`.
fn datg(project: &Path, current: Option<&str>) {
    let dir = project.join(".trusty-mpm/datg/pm-a");
    std::fs::create_dir_all(&dir).expect("mkdir datg");
    let mut session = serde_json::json!({ "task": "free text" });
    if let Some(c) = current {
        session["current"] = serde_json::json!(c);
    }
    let state = serde_json::json!({
        "v": 1,
        "code": "tt",
        "session": session,
        "tasks": [
            { "id": "tt-T0b", "status": "open", "created": "2026-11-16T07:00Z" },
            { "id": "tt-T0c", "status": "done", "created": "2026-11-02T07:00Z" },
        ],
    });
    std::fs::write(dir.join("state.json"), state.to_string()).expect("write state");
}

/// Run the hook in `project` with `payload`; return (stdout, stderr). Asserts exit 0.
fn run(project: &Path, payload: &serde_json::Value, profile: Option<&str>) -> (String, String) {
    let home = tempfile::tempdir().expect("home");
    let mut cmd = common::tm_command_in(home.path());
    cmd.args(["hook", "--turn-task"])
        .current_dir(project)
        .env("CLAUDE_PROJECT_DIR", project)
        .env("DATG_SESSION", "pm-a")
        .env("DBATG_SESSION", "pm-a")
        .env_remove("TMUX_PANE")
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT");
    if let Some(stamp) = profile {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", stamp);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --turn-task`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the hook exits 0: {out:?}");
    (
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

/// A `UserPromptSubmit` payload.
fn prompt() -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-11398",
        "prompt": "next step",
    })
}

#[test]
fn a_pm_turn_is_stamped_with_the_current_open_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    datg(dir.path(), Some("tt-T0b"));
    let (stdout, stderr) = run(dir.path(), &prompt(), None);
    let out: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "Task: tt-T0b@1116"
    );
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn every_fail_open_path_is_silent_and_exits_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sub = prompt();
    sub["agent_id"] = serde_json::json!("agent-7");
    let cases: [(&str, Option<&str>, serde_json::Value); 4] = [
        ("current absent", None, prompt()),
        ("closed task", Some("tt-T0c"), prompt()),
        ("subagent", Some("tt-T0b"), sub),
        ("malformed payload", Some("tt-T0b"), serde_json::json!("{")),
    ];
    for (why, current, payload) in cases {
        datg(dir.path(), current);
        let (stdout, stderr) = run(dir.path(), &payload, None);
        assert!(
            stdout.is_empty() && stderr.is_empty(),
            "{why}: {stdout:?} {stderr:?}"
        );
    }
    std::fs::write(
        dir.path().join(".trusty-mpm/datg/pm-a/state.json"),
        "{not json",
    )
    .expect("corrupt");
    let (stdout, _) = run(dir.path(), &prompt(), None);
    assert!(stdout.is_empty(), "malformed state: {stdout:?}");
    let bare = tempfile::tempdir().expect("tempdir");
    let (stdout, _) = run(bare.path(), &prompt(), None);
    assert!(stdout.is_empty(), "no state file: {stdout:?}");
}

/// Write the dbatg store for session `pm-a` under `project`, shaped like the
/// dbatg MCP's own write: `trusty-architect`'s `State` (tasks carry no
/// `created`; the PM's task is the free-text `session.task` line).
fn dbatg(project: &Path, session_task: &str) {
    let dir = project.join(".trusty-mpm/dbatg/pm-a");
    std::fs::create_dir_all(&dir).expect("mkdir dbatg");
    let state = serde_json::json!({
        "code": "tt",
        "v": 9,
        "config": { "mode": "warn" },
        "goals": [],
        "items": [],
        "tasks": [
            { "id": "tt-T0b", "goal": "tt-G01", "title": "t", "status": "open", "to": "", "outcome": "" },
            { "id": "tt-T0c", "goal": "tt-G01", "title": "t", "status": "done", "to": "", "outcome": "" },
        ],
        "next": { "G": "02", "D": "01", "A": "01", "T": "0d" },
        "session": {
            "written_at": "2026-11-19T01:40Z",
            "written_epoch": 1_795_052_400,
            "task": session_task,
            "next": "dolor sit",
        },
    });
    std::fs::write(dir.join("state.json"), state.to_string()).expect("write state");
}

#[test]
fn a_dbatg_session_task_stamps_the_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    dbatg(dir.path(), "tt-T0b: wiring the stamp");
    let (stdout, stderr) = run(dir.path(), &prompt(), None);
    let out: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "Task: tt-T0b"
    );
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn dbatg_takes_precedence_over_datg_and_never_shows_a_stale_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    datg(dir.path(), Some("tt-T0b"));
    dbatg(dir.path(), "tt-T0b: both stores");
    let (stdout, _) = run(dir.path(), &prompt(), None);
    // dbatg's answer (no `@MMDD`: its tasks have no `created`), not datg's.
    assert!(stdout.contains("\"Task: tt-T0b\""), "{stdout:?}");
    // A dbatg store with no task id is final: datg's older `current` is stale.
    dbatg(dir.path(), "waiting on the owner");
    let (stdout, stderr) = run(dir.path(), &prompt(), None);
    assert!(
        stdout.is_empty() && stderr.is_empty(),
        "{stdout:?} {stderr:?}"
    );
    // A done dbatg task shows nothing either.
    dbatg(dir.path(), "tt-T0c");
    let (stdout, _) = run(dir.path(), &prompt(), None);
    assert!(stdout.is_empty(), "{stdout:?}");
}
