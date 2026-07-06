//! Black-box CLI end-to-end test for #2060: the M1 cut-line "replay via
//! thin CLI" verb.
//!
//! Why: #2053-#2058 proved the daemon + JSON-RPC surface works over its own
//! wire (`tests/session_e2e.rs`, `tests/task_e2e.rs`), driven either
//! directly against `tcode serve` or via the small test-only `StdioSession`
//! helper. This ticket's own deliverable — the `tcode` BINARY's subcommands
//! — needs its OWN black-box proof: spawn the compiled `tcode` binary
//! (`env!("CARGO_BIN_EXE_tcode")`) exactly as a user's shell would, let it
//! do whatever it does internally (which, per #2060, is spawn its OWN
//! nested `tcode serve --stdio` child and speak JSON-RPC to it — this test
//! never touches that nested layer directly), and assert only on the
//! OUTER process's stdout/stderr/exit code. `TCODE_MOCK_LLM=echo` keeps
//! `run-task` deterministic and offline (env propagates from this outer
//! process to the CLI's own spawned nested daemon child, since it inherits
//! the environment by default).
//! What: [`run_task_streams_live_events_and_reports_final_status`] is the
//! REQUIRED case — `tcode run-task python-engineer "<task>" --project <tmp>`
//! must print live tool events and a final `status=finished` line, exit 0.
//! `run_task_json_mode_prints_only_the_final_session` proves `--json`
//! suppresses the per-event lines and emits one parseable JSON document.
//! `run_task_mode_precedence_over_the_wire` (#2059) proves the three-tier
//! `HarnessMode` precedence — env var > `--mode` flag > `.claude/settings.json`
//! > default — end to end via the REAL CLI binary and its `--json` response.
//!
//! `session_list_on_fresh_project_reports_no_sessions`,
//! `transcript_unknown_session_errors_cleanly`,
//! `attach_unknown_session_errors_cleanly`, and
//! `cancel_unknown_session_errors_cleanly` prove the remaining subcommands
//! are wired and that daemon errors surface as clear CLI errors and a
//! nonzero exit (per the ticket's explicit requirement) — see those
//! subcommands' own module docs (`crate::cli::session`/`attach`/`cancel`/
//! `transcript` in `src/cli/`) for why a MEANINGFUL cross-invocation
//! inspection test (attach to a session a DIFFERENT process created) isn't
//! possible yet: M1 sessions are in-memory, one per ephemeral spawned
//! daemon, and this ticket's required path is spawn-stdio, not a shared
//! long-lived daemon.
//!
//! Test: this file IS the test; see `support` for the shared
//! `project_with_agents` fixture.

mod support;

use std::process::Command;

use support::project_with_agents;

/// `tcode run-task <agent> "<task>" --project <tmp>` (`TCODE_MOCK_LLM=echo`)
/// must stream the PM's `delegate_to_agent` and the engineer's `bash` tool
/// events to stdout, then report `status=finished`, exiting 0.
#[test]
fn run_task_streams_live_events_and_reports_final_status() {
    let project = project_with_agents();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "pm",
            "say hi",
            "--project",
            &project.path().display().to_string(),
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "tcode run-task must exit 0, got {:?}\nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("delegate_to_agent"),
        "expected the PM's delegate_to_agent dispatch in stdout: {stdout}"
    );
    assert!(
        stdout.contains("tool_started") && stdout.contains("bash"),
        "expected the engineer's bash dispatch in stdout: {stdout}"
    );
    assert!(
        stdout.contains("tool_finished"),
        "expected at least one tool_finished line in stdout: {stdout}"
    );
    assert!(
        stdout.contains("status=finished"),
        "expected the final status line to report finished: {stdout}"
    );
}

/// `tcode run-task ... --json` must suppress the live per-event lines and
/// print exactly one parseable JSON document with `status: "finished"`.
#[test]
fn run_task_json_mode_prints_only_the_final_session() {
    let project = project_with_agents();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "pm",
            "say hi",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task --json");

    assert!(
        output.status.success(),
        "tcode run-task --json must exit 0, got {:?}",
        output.status.code()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("tool_started"),
        "--json must suppress live event lines: {stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    assert_eq!(parsed["status"], "finished");
}

/// Run `tcode run-task ... --json`, optionally with `--mode`, a
/// `.claude/settings.json`, and/or `TRUSTY_CODE_MODE`, returning the
/// resolved `mode` string from the parsed JSON response.
fn run_task_resolved_mode(
    project: &std::path::Path,
    cli_mode: Option<&str>,
    env_mode: Option<&str>,
) -> String {
    let mut args = vec![
        "run-task".to_string(),
        "pm".to_string(),
        "say hi".to_string(),
        "--project".to_string(),
        project.display().to_string(),
        "--json".to_string(),
    ];
    if let Some(m) = cli_mode {
        args.push("--mode".to_string());
        args.push(m.to_string());
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tcode"));
    cmd.args(&args).env("TCODE_MOCK_LLM", "echo");
    if let Some(m) = env_mode {
        cmd.env("TRUSTY_CODE_MODE", m);
    } else {
        cmd.env_remove("TRUSTY_CODE_MODE");
    }
    let output = cmd.output().expect("spawn tcode run-task");
    assert!(
        output.status.success(),
        "tcode run-task must exit 0: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    parsed["mode"]
        .as_str()
        .unwrap_or_else(|| panic!("response must carry a mode string: {parsed}"))
        .to_string()
}

/// #2059's three-tier `HarnessMode` precedence, proven over the REAL wire
/// via the `tcode` CLI (not just `crate::mode::resolve_mode`'s own offline
/// unit tests, and not just `task::protocol::tests`' direct-handler-call
/// integration test): `TRUSTY_CODE_MODE` env var > `task.run`'s `mode` param
/// (here, the CLI's `--mode` flag) > `.claude/settings.json`'s
/// `code_harness.mode` > default `daily-driver`.
#[test]
fn run_task_mode_precedence_over_the_wire() {
    // 1. Nothing set anywhere -> default.
    let project = project_with_agents();
    assert_eq!(
        run_task_resolved_mode(project.path(), None, None),
        "daily-driver"
    );

    // 2. `.claude/settings.json` alone sets parity.
    std::fs::write(
        project.path().join(".claude").join("settings.json"),
        r#"{"code_harness": {"mode": "parity"}}"#,
    )
    .expect("write settings.json");
    assert_eq!(run_task_resolved_mode(project.path(), None, None), "parity");

    // 3. The CLI's `--mode` flag (task.run's `mode` param) overrides
    //    settings.json.
    assert_eq!(
        run_task_resolved_mode(project.path(), Some("daily-driver"), None),
        "daily-driver"
    );

    // 4. `TRUSTY_CODE_MODE` overrides EVERYTHING, including `--mode`.
    assert_eq!(
        run_task_resolved_mode(project.path(), Some("daily-driver"), Some("parity")),
        "parity"
    );
}

/// `tcode session list` on a project with no prior activity must report no
/// sessions and exit 0 — proving the `session.list` wiring even though this
/// ephemeral-spawn CLI invocation's daemon has an empty registry.
#[test]
fn session_list_on_fresh_project_reports_no_sessions() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "session",
            "list",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode session list");

    assert!(output.status.success(), "must exit 0: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no sessions"),
        "expected an empty-list message: {stdout}"
    );
}

/// `tcode transcript <unknown-id>` must surface the daemon's
/// `session_not_found` error as a clean CLI error on stderr and exit
/// nonzero.
#[test]
fn transcript_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "transcript",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode transcript");

    assert!(
        !output.status.success(),
        "must exit nonzero on session_not_found"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// `tcode attach <unknown-id>` must likewise surface a clean error and exit
/// nonzero rather than hanging.
#[test]
fn attach_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "attach",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode attach");

    assert!(!output.status.success(), "must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// `tcode cancel <unknown-id>` must likewise surface a clean error and exit
/// nonzero.
#[test]
fn cancel_unknown_session_errors_cleanly() {
    let project = tempfile::tempdir().expect("project tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "cancel",
            "nonexistent-id",
            "--project",
            &project.path().display().to_string(),
        ])
        .output()
        .expect("spawn tcode cancel");

    assert!(!output.status.success(), "must exit nonzero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}
