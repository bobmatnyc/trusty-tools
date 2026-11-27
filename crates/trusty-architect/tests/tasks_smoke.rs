//! #12028 P1d: the shipped task checker binaries run.
//!
//! Each binary reads its inputs below the working directory (the Architect
//! checkout), the paths the replay cannot record because the scripts build
//! them beside their own checkout. Also: a failed read is an error, never an
//! empty list, and a deeply nested input is refused, not a crash.

use std::path::Path;
use std::process::{Command, Output};

const STALE: &str = env!("CARGO_BIN_EXE_trusty-architect-stale-tasks");
const STALLED: &str = env!("CARGO_BIN_EXE_trusty-architect-stalled-tasks");
const WAIT: &str = env!("CARGO_BIN_EXE_trusty-architect-task-wait-check");

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn run(bin: &str, cwd: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let o: Output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_remove("DASHBOARD_ROOT")
        .env_remove("DASHBOARD_FORMAT")
        .env_remove("DATG_STATE_DIR")
        .env_remove("DATG_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    )
}

#[test]
fn shipped_stale_tasks_command_reads_the_working_directory_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(
        root,
        "local/owner-queue/items/T01.json",
        r#"{"kind": "task", "status": "open", "title": "idle"}"#,
    );
    let (code, stdout, stderr) = run(STALE, root, &[]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout,
        "T01\t\tnever\t-\t-\tidle\t-\tstale\tno_progress,no_session\n"
    );

    // A document that does not parse fails the run; it never lists nothing.
    put(root, "local/owner-queue/items/T02.json", "{");
    let (code, stdout, stderr) = run(STALE, root, &["--json"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty());
    assert!(
        stderr.starts_with("stale-tasks: cannot read store: unreadable document "),
        "{stderr}"
    );

    // Nesting past the decoder's bound is refused, not a stack overflow.
    put(
        root,
        "local/owner-queue/items/T02.json",
        &format!("{}{}", "[".repeat(100_000), "]".repeat(100_000)),
    );
    let (code, _, stderr) = run(STALE, root, &[]);
    assert_eq!(code, Some(2), "{stderr}");

    let (code, _, stderr) = run(STALE, root, &["--hours", "x"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.starts_with("usage: trusty-architect-stale-tasks "),
        "{stderr}"
    );
}

#[test]
fn shipped_task_commands_read_the_working_directory_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(
        root,
        "local/owner-queue/items/D12.json",
        r#"{"status": "answered", "answered_at": "2026-11-26T10:11Z"}"#,
    );
    let state = r#"{"code": "ar", "v": 1, "next": {}, "goals": [], "items": [], "tasks": [
        {"id": "ar-T01", "status": "open", "priority": 1, "created": "2000-01-01T00:00Z",
         "remaining": "waits on D12", "done_when": "Bob confirms"}]}"#;
    put(root, "local/il/.trusty-mpm/datg/state.json", state);

    let (code, stdout, stderr) = run(STALLED, root, &[]);
    assert_eq!(code, Some(0), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].starts_with("STALLED (no progress past priority limit): ar-T01 P1 "),
        "{stdout}"
    );
    assert_eq!(lines[1], "none");

    let (code, stdout, _) = run(WAIT, root, &[]);
    assert_eq!(code, Some(0));
    assert_eq!(
        stdout,
        "STALE-WAIT ar-T01 names D12 in remaining, but D12 is answered (2026-11-26T10:11Z)\n\
         UNCHECKABLE ar-T01 done_when has no observable: \"Bob confirms\"\n"
    );

    // An unreadable ledger is reported, never "none".
    put(
        root,
        "local/il/.trusty-mpm/datg/state.json",
        &"[".repeat(100_000),
    );
    let (code, _, stderr) = run(STALLED, root, &[]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.starts_with("datg.DatgError: state.json malformed ("),
        "{stderr}"
    );
    let (_, stdout, _) = run(WAIT, root, &[]);
    assert!(
        stdout.starts_with("task_wait_check failed: DatgError: state.json malformed ("),
        "{stdout}"
    );
}
