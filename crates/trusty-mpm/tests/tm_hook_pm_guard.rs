//! Integration test for `tm hook --pm-guard` — PM PreToolUse enforcement
//! (issue #1977; native sub-agent exemption issue #2014; opt-in deny-by-default
//! persona gate issue #2231).
//!
//! Why: `commands::pm_guard`'s unit tests cover the pure policy (which tools /
//! Bash verbs deny) in isolation, but none exercises the actual
//! stdin-read → classify → stdout-print path end to end through the real
//! binary — including the env short-circuits, the native sub-agent `agent_id`
//! exemption, and fail-open behaviour Claude Code will actually depend on.
//! This file closes that gap.
//! What: runs the built `tm` binary as `tm --url http://127.0.0.1:1 hook
//! --pm-guard` with a `PreToolUse` stdin payload, then asserts the printed
//! `hookSpecificOutput.permissionDecision` (or that nothing is printed, meaning
//! ALLOW). The daemon URL is pointed at an unreachable address so the
//! best-effort audit POST on the deny path fails fast without a real daemon —
//! proving the deny still emits regardless.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard`.
//!
//! Note: the deny JSON shape asserted below
//! (`hookSpecificOutput.{hookEventName, permissionDecision, permissionDecisionReason}`)
//! and the stdin field names consumed (`hook_event_name`, `tool_name`,
//! `tool_input`, `agent_id`, `agent_type`) are confirmed against the live
//! Claude Code hooks reference
//! (<https://code.claude.com/docs/en/hooks>, confirmed 2026-07-03).
//!
//! Per-turn file-change budget (issue #2918): a "file change" deny
//! (source-code Edit/Write, or a shell-based file edit) is no longer an
//! absolute single-call prohibition — it is allowed up to 3 times per
//! turn-window (`pm_guard_budget::DEFAULT_FILE_CHANGE_BUDGET`) before hard-
//! blocking. The budget counter is a file under `<HOME>/.trusty-mpm/state/
//! pm_guard_turn_budget/`, so every test that exercises budget-eligible
//! denials spawns its `tm hook --pm-guard` calls with an isolated per-test
//! `HOME` (via [`isolated_home`]) — otherwise parallel test threads would
//! share (and race on) the same counter file, and would also pollute the
//! real developer `$HOME`.

use std::io::Write;
use std::process::{Command, Stdio};

/// A fresh, isolated `$HOME` for a test that exercises the per-turn
/// file-change budget (issue #2918) — see the module doc for why this is
/// required (shared/racy counter file otherwise).
fn isolated_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Spawn `tm hook --pm-guard` with the given stdin JSON and optional extra env,
/// returning stdout as a string. Asserts a clean `exit 0` (fail-open contract).
fn run_pm_guard(stdin_json: &str, extra_env: &[(&str, &str)]) -> String {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut command = Command::new(bin);
    command
        .args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        command.env(k, v);
    }
    let mut child = command
        .spawn()
        .expect("failed to spawn `tm hook --pm-guard`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child
        .wait_with_output()
        .expect("wait for tm hook --pm-guard");
    assert!(
        output.status.success(),
        "tm hook --pm-guard must always exit 0 (fail-open): status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

/// Parse the stdout into a JSON value and assert it is a `deny` decision.
fn assert_denied(stdout: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "a deny must print exactly one line (the JSON object), got: {stdout:?}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("deny stdout must be valid JSON");
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "expected a deny decision, got: {stdout}"
    );
    assert!(
        parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "deny must carry a non-empty reason"
    );
}

#[test]
fn pm_guard_allows_edit_tool_within_budget_then_denies() {
    // Issue #2918: a source-code Edit is no longer an absolute single-call
    // prohibition — it is allowed up to the 3-per-turn budget, then denied.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#;
    for n in 1..=3 {
        let stdout = run_pm_guard(payload, &[("HOME", &home_s)]);
        assert_eq!(
            stdout.trim(),
            "",
            "call {n} of 3 should be within budget and allowed"
        );
    }
    let stdout = run_pm_guard(payload, &[("HOME", &home_s)]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("budget 3/3"),
        "exhausted deny must state the budget count: {stdout}"
    );
    assert!(
        stdout.contains("rust-engineer"),
        "a .rs target must route to rust-engineer: {stdout}"
    );
}

#[test]
fn pm_guard_allows_write_tool_within_budget_then_denies() {
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"/x/a.rs","content":"x"}}"#;
    for n in 1..=3 {
        let stdout = run_pm_guard(payload, &[("HOME", &home_s)]);
        assert_eq!(
            stdout.trim(),
            "",
            "call {n} of 3 should be within budget and allowed"
        );
    }
    let stdout = run_pm_guard(payload, &[("HOME", &home_s)]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("budget 3/3"),
        "must state the count: {stdout}"
    );
}

#[test]
fn pm_guard_file_change_budget_shared_across_edit_and_bash() {
    // The budget counts ALL budget-eligible file changes together, whether
    // via an Edit-tool call or a shell-based edit (Bash sed/redirect) — not
    // a separate 3-per-tool allowance.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let edit = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#;
    let sed = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ app/main.py"}}"#;

    assert_eq!(run_pm_guard(edit, &[("HOME", &home_s)]).trim(), "");
    assert_eq!(run_pm_guard(sed, &[("HOME", &home_s)]).trim(), "");
    assert_eq!(run_pm_guard(edit, &[("HOME", &home_s)]).trim(), "");
    // 4th combined file change denies, routed by the 4th call's own target
    // (the sed's app/main.py -> python-engineer).
    let stdout = run_pm_guard(sed, &[("HOME", &home_s)]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("budget 3/3"),
        "must state the count: {stdout}"
    );
    assert!(
        stdout.contains("python-engineer"),
        "a .py target must route to python-engineer: {stdout}"
    );
}

#[test]
fn pm_guard_budget_exhausted_routes_docs_target_to_documentation_agent() {
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    // .md writes are always allowed (non-source), so exhaust the budget via
    // three shell-based edits, then trip a shell-based edit on a docs file.
    let sed_rs = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ src/lib.rs"}}"#;
    let redirect_md = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo x > docs/reference/notes.md"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(sed_rs, &[("HOME", &home_s)]).trim(), "");
    }
    let stdout = run_pm_guard(redirect_md, &[("HOME", &home_s)]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("documentation agent"),
        "a docs/ .md target must route to the documentation agent: {stdout}"
    );
}

#[test]
fn pm_guard_allows_pm_session_snapshot_write() {
    // Regression for issue #2604: the PM's own `/tm-session-pause` snapshot write
    // to `.trusty-mpm/sessions/session-*.md` must be ALLOWED (no output). This is
    // the exact path from Bob's live-dogfooding bug report. Orchestration state
    // under `.trusty-mpm/` is PM-owned and must never be blocked.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":".trusty-mpm/sessions/session-20260714-064556.md","content":"session snapshot"}}"#,
        &[],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "PM session-pause snapshot write under .trusty-mpm/ must be allowed"
    );
}

#[test]
fn pm_guard_allows_non_source_single_file_write() {
    // Bob's directive (issue #2604): the PM can write single non-source files
    // (docs / notes / config), not just delegate every write.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"NOTES.md","content":"x"}}"#,
        &[],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "a single non-source file write must be allowed"
    );
}

#[test]
fn pm_guard_denies_forbidden_bash_verb_after_budget_exhausted() {
    // `sed -i` edits a file in place — a P1/P1-P3 circumvention via Bash. It
    // is budget-eligible (issue #2918): allowed up to 3 times, then denied.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ src/lib.rs"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(payload, &[("HOME", &home_s)]).trim(), "");
    }
    let stdout = run_pm_guard(payload, &[("HOME", &home_s)]);
    assert_denied(&stdout);
}

#[test]
fn pm_guard_allows_read_tool() {
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}"#,
        &[],
    );
    assert_eq!(stdout.trim(), "", "Read must be allowed (no output)");
}

#[test]
fn pm_guard_allows_git_status_and_task() {
    let git = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
        &[],
    );
    assert_eq!(git.trim(), "", "git status must be allowed");

    let task = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Task","tool_input":{"subagent_type":"rust-engineer","prompt":"do it"}}"#,
        &[],
    );
    assert_eq!(task.trim(), "", "Task must be allowed");
}

#[test]
fn pm_guard_fails_open_on_malformed_input() {
    // Malformed / empty stdin must degrade to ALLOW (no output), never block.
    assert_eq!(run_pm_guard("not json at all", &[]).trim(), "");
    assert_eq!(run_pm_guard("", &[]).trim(), "");
    // A well-formed object with no tool_name also fails open.
    assert_eq!(
        run_pm_guard(r#"{"hook_event_name":"PreToolUse"}"#, &[]).trim(),
        ""
    );
}

#[test]
fn pm_guard_bypass_env_allows_all() {
    // TRUSTY_MPM_PM_UNRESTRICTED=1 lifts enforcement — even an Edit is allowed.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs"}}"#,
        &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "the unrestricted bypass must allow everything"
    );
}

#[test]
fn pm_guard_sub_agent_env_allows_all() {
    // A nested MPM sub-agent (CLAUDE_MPM_SUB_AGENT set) is doing the delegated
    // work — it must never be blocked.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"/x/a.rs","content":"x"}}"#,
        &[("CLAUDE_MPM_SUB_AGENT", "1")],
    );
    assert_eq!(stdout.trim(), "", "sub-agents must not be blocked");
}

#[test]
fn pm_guard_native_subagent_dispatch_allows_edit() {
    // Native Task/Agent-tool dispatch (issue #2014): Claude Code stamps
    // `agent_id` on the PreToolUse payload when the hook fires inside a
    // sub-agent. No CLAUDE_MPM_SUB_AGENT env var and no
    // TRUSTY_MPM_PM_UNRESTRICTED bypass is set here — the exemption must come
    // purely from the payload.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","agent_type":"rust-engineer","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#,
        &[],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "a native sub-agent's Edit call must be allowed"
    );
}

#[test]
fn pm_guard_native_subagent_dispatch_allows_bash() {
    // The exemption is not edit-tool-specific — it applies before any
    // tool-specific classification, so a sub-agent's forbidden-verb Bash call
    // (which the PM itself would be denied) is exempt too.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","agent_id":"agent-xyz789","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ f"}}"#,
        &[],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "a native sub-agent's Bash call must be allowed"
    );
}

#[test]
fn pm_guard_deny_by_default_unset_allows_subagent_edit_unchanged() {
    // Issue #2231, case (i): TRUSTY_MPM_PM_DENY_BY_DEFAULT unset (default) — a
    // native sub-agent's Edit call must be allowed exactly as before, with no
    // TM_MANAGED_SESSION_ID at all (not a managed session).
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#,
        &[],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "deny-by-default unset must leave the sub-agent exemption unchanged"
    );
}

#[test]
fn pm_guard_deny_by_default_allows_subagent_edit_when_not_managed() {
    // Issue #2231, case (iv, unmanaged branch): TRUSTY_MPM_PM_DENY_BY_DEFAULT=1
    // but no TM_MANAGED_SESSION_ID (ad-hoc/`tm connect`/vanilla session) — the
    // permissive #2172-lesson default must still allow.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#,
        &[("TRUSTY_MPM_PM_DENY_BY_DEFAULT", "1")],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "an unmanaged session must be allowed even in strict mode"
    );
}

#[test]
fn pm_guard_deny_by_default_allows_subagent_edit_when_daemon_unreachable() {
    // Issue #2231, case (iv, ambiguous branch): TRUSTY_MPM_PM_DENY_BY_DEFAULT=1
    // AND a managed-session id set, but the daemon (fixed at an unreachable
    // address by `run_pm_guard`) cannot confirm the session's state — ambiguous
    // must allow, never hang and never deny.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#,
        &[
            ("TRUSTY_MPM_PM_DENY_BY_DEFAULT", "1"),
            (
                "TM_MANAGED_SESSION_ID",
                "11111111-1111-1111-1111-111111111111",
            ),
        ],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "an unresolvable (ambiguous) persona status must allow, not deny"
    );
}

#[test]
fn pm_guard_empty_agent_id_does_not_exempt() {
    // Defensive: an empty agent_id string must not count as a dispatch
    // signal — the call goes through the SAME budgeted policy as the
    // top-level PM (issue #2918), so it eventually denies once the budget is
    // exhausted. A genuinely exempted call would never deny, no matter how
    // many times it is repeated — that's what distinguishes "not exempted"
    // from "exempted" now that a single call alone always allows.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(payload, &[("HOME", &home_s)]).trim(), "");
    }
    assert_denied(&run_pm_guard(payload, &[("HOME", &home_s)]));
}

#[test]
fn pm_guard_agent_type_alone_does_not_exempt() {
    // `agent_type` alone (e.g. a top-level session launched with `--agent`)
    // must NOT be treated as a sub-agent dispatch — only `agent_id` does. See
    // the previous test's comment for why exhausting the budget is now the
    // way to prove "not exempted".
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","agent_type":"rust-engineer","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(payload, &[("HOME", &home_s)]).trim(), "");
    }
    assert_denied(&run_pm_guard(payload, &[("HOME", &home_s)]));
}

#[test]
fn pm_guard_disable_hooks_env_allows_all() {
    // TRUSTY_MPM_DISABLE_HOOKS is the universal opt-out for CI / build shells —
    // even a direct Edit must pass when it is set.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#,
        &[("TRUSTY_MPM_DISABLE_HOOKS", "1")],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "the disable-hooks bypass must allow everything"
    );
}

#[test]
fn pm_guard_denies_composition_hidden_verb() {
    // Shell composition must not let a benign leading verb hide a forbidden one
    // in a later segment (the composition bypass fixed with PR #1985).
    // `make build` (BUILD_TEST_REASON) is NOT budget-eligible (issue #2918) —
    // it stays an absolute, single-call prohibition.
    let make = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"true; make build"}}"#,
        &[],
    );
    assert_denied(&make);

    // `sed -i` and a real file-write redirect are budget-eligible — allowed
    // up to 3 times, then denied.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let sed = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cd repo && sed -i s/a/b/ f"}}"#;
    let redirect = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo hi > out.txt"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(sed, &[("HOME", &home_s)]).trim(), "");
    }
    assert_denied(&run_pm_guard(sed, &[("HOME", &home_s)]));

    let home2 = isolated_home();
    let home2_s = home2.path().to_string_lossy().to_string();
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(redirect, &[("HOME", &home2_s)]).trim(), "");
    }
    assert_denied(&run_pm_guard(redirect, &[("HOME", &home2_s)]));
}

#[test]
fn pm_guard_allows_benign_pipes_and_dev_null() {
    // Composition with no forbidden segment must still allow.
    let piped = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git log | head"}}"#,
        &[],
    );
    assert_eq!(piped.trim(), "", "benign pipe must be allowed");

    // Discarding output to /dev/null is not a file write.
    let dev_null = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"which cargo 2>/dev/null"}}"#,
        &[],
    );
    assert_eq!(dev_null.trim(), "", "2>/dev/null discard must be allowed");
}
