//! E2E repro for issue #3046: the embedded 31-agent `DEFAULT_AGENTS` roster
//! (Slice E1-E3, #2958) has a working in-memory composer, but before this
//! fix FIVE production call sites (`runner::in_process::load_agent`,
//! `run_task::execute_run_task`'s and `task::executor`'s PM-config loads,
//! `run_task::resolve_agent_model_slug`, and
//! `tools::delegate::DelegateToAgentTool`'s pre-flight/hint) read
//! `<agents_dir>/<name>.md` directly off disk with no embedded fallback at
//! all — so `tcode run-task engineer ...` (an ORIGINAL default, predating
//! #2958) and `tcode run-task rust-engineer ...` (a Slice E3 roster agent)
//! both failed with "agent source not found" on a fresh project with no
//! `.claude/agents/`. `crate::agents::resolve_agent` (#3046) is the shared
//! fix; this file is the literal real-binary repro the issue names as the
//! acceptance bar.
//!
//! Why: every offline unit test in `agents::tests`/`runner::tests` proves
//! the fix at the library level, but only spawning the REAL `tcode` binary
//! (as a subprocess, exactly like `tests/cli_e2e.rs`) proves the fix is
//! actually WIRED into the CLI dispatch paths a user drives.
//! What: (a)/(b) point `tcode run-task <agent> ... --project <bare-tmp>
//! --json` (`TCODE_MOCK_LLM=echo`) at a tempdir with NO `.claude/agents/`
//! directory at all (unlike `support::project_with_agents`, which writes
//! `.claude/agents/{pm,python-engineer}.md` — the opposite of what #3046
//! needs to prove) and assert the run reaches `status: "finished"`. (c)
//! proves disk-override-wins by driving the daemon protocol directly
//! (`support::StdioSession`, mirroring `tests/task_e2e.rs`) so the run's
//! transcript (not exposed by the outer CLI's `--json`, which only prints
//! the terminal `Session` snapshot) can be inspected: a disk
//! `rust-engineer.md` with a distinguishing `model:` must drive the
//! top-level loop even though its delegated `python-engineer` sub-agent
//! resolves purely via the embedded fallback in the SAME run.
//! Test: this file IS the test.

mod support;

use std::process::Command;

use serde_json::json;

/// A bare project tempdir with NO `.claude/agents/` directory — the exact
/// fresh-project shape #3046 requires, deliberately NOT
/// `support::project_with_agents()` (which writes `.claude/agents/*.md` and
/// would defeat the point of this repro).
fn bare_project() -> tempfile::TempDir {
    tempfile::tempdir().expect("project tempdir")
}

/// (a) Direct repro: `tcode run-task engineer ...` on a fresh project with
/// no `.claude/agents/` must dispatch successfully via
/// `EmbeddedAgent::Direct`.
///
/// Why: `engineer` is one of tcode's three ORIGINAL defaults (predating
/// #2958) and exercises `run_task::execute_run_task`'s PM-config load (the
/// #run_task-mod-mod-fix) directly. `EchoLlmClient`'s fixed script then
/// self-delegates to `python-engineer` — also not on disk, also embedded
/// (`EmbeddedAgent::Composed`) — so this single run additionally exercises
/// `runner::in_process::load_agent` and
/// `tools::delegate::DelegateToAgentTool`'s pre-flight gate.
/// What: spawns the real `tcode` binary with `TCODE_MOCK_LLM=echo`; asserts
/// exit 0 and `status: "finished"` in the `--json` output.
/// Test: this test.
#[test]
fn run_task_engineer_direct_embedded_agent_dispatches_successfully() {
    let project = bare_project();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "engineer",
            "say hi",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task engineer");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tcode run-task engineer must exit 0 on a fresh project with no \
         .claude/agents/ (#3046), got {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    assert_eq!(parsed["status"], "finished");
}

/// (b) Composed repro: `tcode run-task rust-engineer ...` on a fresh
/// project with no `.claude/agents/` must dispatch successfully via
/// `EmbeddedAgent::Composed` (`rust-engineer` extends `base-engineer`
/// extends `base-agent`).
///
/// Why: proves the in-memory extends-composition path also works from the
/// TOP-LEVEL (PM-config) entry point, not just the delegated-sub-agent path
/// `runner::tests::embedded_restricted_agent_dispatch_preserves_tools_allowed`
/// already covers.
/// What: same shape as (a), agent name `rust-engineer`.
/// Test: this test.
#[test]
fn run_task_rust_engineer_composed_embedded_agent_dispatches_successfully() {
    let project = bare_project();
    let output = Command::new(env!("CARGO_BIN_EXE_tcode"))
        .args([
            "run-task",
            "rust-engineer",
            "say hi",
            "--project",
            &project.path().display().to_string(),
            "--json",
        ])
        .env("TCODE_MOCK_LLM", "echo")
        .output()
        .expect("spawn tcode run-task rust-engineer");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tcode run-task rust-engineer must exit 0 on a fresh project with no \
         .claude/agents/ (#3046), got {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be valid JSON: {e}: {stdout}"));
    assert_eq!(parsed["status"], "finished");
}

/// (c) Project-override e2e: a disk `rust-engineer.md` must drive the
/// top-level loop instead of the embedded default, even in the SAME run
/// where the delegated `python-engineer` sub-agent resolves purely via the
/// embedded fallback (proving override and fallback coexist correctly).
///
/// Why: the outer `tcode run-task ... --json` CLI surface only ever prints
/// the terminal `Session` snapshot (`session.status`'s wire shape), which
/// carries no transcript field — so proving WHICH config drove the loop
/// requires inspecting `session.get_transcript`, which requires driving the
/// daemon protocol directly on the SAME session (a second `tcode`
/// invocation would spawn a fresh, empty-registry daemon — M1 sessions are
/// in-memory, non-persistent). This mirrors `tests/task_e2e.rs`'s own
/// `support::StdioSession`-based pattern instead of `tests/cli_e2e.rs`'s
/// outer-binary-only pattern.
/// What: writes ONLY `.claude/agents/rust-engineer.md` (no `python-engineer.md`)
/// in a bare tempdir, with a distinguishing `model: marker/disk-override-wins`.
/// Drives `task.run(agent_name: "rust-engineer")` -> `session.attach` ->
/// polls for `session_done` -> `session.get_transcript`, then asserts the
/// `"pm"`-role turn's `model` is the disk marker, not the embedded
/// rust-engineer's real model slug.
/// Test: this test.
#[tokio::test]
async fn disk_override_wins_over_embedded_for_top_level_dispatch() {
    let project = tempfile::tempdir().expect("project tempdir");
    let agents = project.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(
        agents.join("rust-engineer.md"),
        "---\nname: rust-engineer\nmodel: marker/disk-override-wins\n---\n\n\
         Disk override body — no extends: needed, it just has to parse.\n",
    )
    .expect("write rust-engineer.md override");
    // Deliberately no python-engineer.md on disk: the delegated sub-agent
    // must resolve via the embedded fallback in this SAME run.

    let mut daemon = support::StdioSession::spawn_with_mock_llm(project.path());

    let run_resp = daemon
        .call(
            1,
            "task.run",
            json!({"task_description": "say hi", "agent_name": "rust-engineer"}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut kinds: Vec<String> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .iter()
        .map(|e| {
            e["kind"]
                .as_str()
                .expect("kind must be a string")
                .to_string()
        })
        .collect();

    let mut iterations = 0;
    while !kinds.iter().any(|k| k == "session_done") {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; \
             kinds so far: {kinds:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {kinds:?}"
        );
        for line in &lines {
            if let Some(envelope) = support::find_session_event(line, &session_id) {
                kinds.push(
                    envelope["kind"]
                        .as_str()
                        .expect("kind must be a string")
                        .to_string(),
                );
            }
        }
    }

    let transcript_resp = daemon
        .call(
            3,
            "session.get_transcript",
            json!({"session_id": session_id}),
        )
        .await;
    assert!(
        transcript_resp["error"].is_null(),
        "get_transcript failed: {transcript_resp}"
    );
    let turns = transcript_resp["result"]["turns"]
        .as_array()
        .expect("turns must be an array");
    let pm_turn = turns
        .iter()
        .find(|t| t["role"] == "pm")
        .unwrap_or_else(|| panic!("expected a pm-role turn in the transcript: {turns:?}"));
    assert_eq!(
        pm_turn["model"], "marker/disk-override-wins",
        "the disk rust-engineer.md override must drive the top-level loop, \
         not the embedded default's own model"
    );
}
