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
///
/// It sets NO working directory, so the child inherits the test binary's, and a
/// payload carrying no `cwd` field lets that decide every rule keyed on
/// `hook_cwd` (`pm_guard.rs`) — a main checkout in CI, a linked worktree on a
/// developer machine. Never use this for a dispatch-tool payload or any other
/// cwd-sensitive rule; reach for [`run_pm_guard_outside_a_checkout`] or
/// [`run_pm_guard_at`], which pin the directory. See #5708.
fn run_pm_guard(stdin_json: &str, extra_env: &[(&str, &str)]) -> String {
    finish_pm_guard(spawn_pm_guard(
        stdin_json,
        UNREACHABLE_DAEMON,
        None,
        extra_env,
    ))
}

/// The daemon URL the URL-insensitive helpers pin: nothing listens on port 1,
/// so a daemon call fails open on a refused connection, never on a timeout.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// Spawn `tm hook --pm-guard` and hand back the running child (#5914).
///
/// Why: the concurrency test needs both children STARTED before either is
/// collected, so it cannot use a helper that waits. Splitting spawn from
/// collect also gives this file one place that scrubs the environment — five
/// `env_remove` calls had been copied into three spawn sites, which is how one
/// of them drifts into inheriting an operator escape hatch from the runner and
/// quietly asserting nothing.
/// What: builds the child, writes `stdin_json`, closes stdin, and returns
/// without waiting. `cwd` of `None` inherits the runner's directory, which is
/// [`run_pm_guard`]'s documented behaviour.
/// Test: every helper below routes through it, and
/// `pm_guard_denies_the_second_of_two_simultaneous_dispatches` is the one
/// caller that needs the un-waited child.
fn spawn_pm_guard(
    stdin_json: &str,
    url: &str,
    cwd: Option<&std::path::Path>,
    extra_env: &[(&str, &str)],
) -> std::process::Child {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut command = Command::new(bin);
    command
        .args(["--url", url, "hook", "--pm-guard"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
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
    child
}

/// Collect a child started by [`spawn_pm_guard`], asserting the fail-open
/// contract (`exit 0`, always) and returning its stdout.
fn finish_pm_guard(child: std::process::Child) -> String {
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
    // `sed -i` edits a file in place — a P1/P5 circumvention via Bash. It
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
fn pm_guard_readonly_heredoc_turn_never_touches_the_file_change_budget() {
    // #5356: a read-only `python3 <<'PY'` script whose body compares with `>`
    // was classified as a shell file edit, so three such reads were allowed
    // silently while consuming the budget and the fourth was denied as
    // "PM file-change budget 3/3 used this turn" with zero files changed.
    // Pre-fix this test fails twice over: the fourth call denies, and the
    // counter file exists.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"issue-5356","tool_name":"Bash","tool_input":{"command":"python3 <<'PY'\nimport json\nd = json.load(open('/tmp/x.json'))\nprint([k for k in d if len(k) > 3])\nPY"}}"#;
    for n in 1..=4 {
        assert_eq!(
            run_pm_guard(payload, &[("HOME", &home_s)]).trim(),
            "",
            "call {n} reads a file and writes none — it must be allowed"
        );
    }
    let counter = home.path().join(".trusty-mpm/state/pm_guard_turn_budget");
    assert!(
        !counter.exists(),
        "a read-only turn must not record a file change at {}",
        counter.display()
    );

    // The still-denies arm: the same heredoc with a real redirect on its
    // operator line is a file write, and exhausts the budget as before.
    let writing = r#"{"hook_event_name":"PreToolUse","session_id":"issue-5356","tool_name":"Bash","tool_input":{"command":"python3 <<'PY' > src/lib.rs\nprint(1)\nPY"}}"#;
    for _ in 1..=3 {
        assert_eq!(run_pm_guard(writing, &[("HOME", &home_s)]).trim(), "");
    }
    assert_denied(&run_pm_guard(writing, &[("HOME", &home_s)]));
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
    // #5708: pinned outside any checkout because the `Task` arm is a dispatch —
    // in a main checkout ADR-0048 denies it for want of an `isolation` field,
    // which is its own rule's business and asserted separately.
    let git = run_pm_guard_outside_a_checkout(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
    );
    assert_eq!(git.trim(), "", "git status must be allowed");

    let task = run_pm_guard_outside_a_checkout(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Task","tool_input":{"subagent_type":"rust-engineer","prompt":"do it"}}"#,
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
fn pm_guard_claude_mpm_sub_agent_env_still_allows_forbidden_bash_verb() {
    // Round 3 (code-critic MEDIUM on PR #3978): the round-2 reorder moved
    // Guard 1 (`CLAUDE_MPM_SUB_AGENT`) to AFTER the worktree-tmp guard, but
    // no test previously covered a plain forbidden-Bash-verb case under that
    // env var post-reorder —
    // `pm_guard_claude_mpm_sub_agent_env_still_allows_everything_else` only
    // used an in-project `git worktree add`, which `evaluate_bash_command`
    // would allow even with NO exemption at all, making it a weak proxy.
    // Mirrors `pm_guard_native_subagent_dispatch_allows_bash`: `sed -i` is
    // denied unconditionally for the PM's own shell, so it must still be
    // exempt here to prove Guard 1's original "exempt everything else"
    // behavior survived the reorder intact.
    let sed = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"sed -i s/a/b/ f"}}"#,
        &[("CLAUDE_MPM_SUB_AGENT", "1")],
    );
    assert_eq!(
        sed.trim(),
        "",
        "a nested MPM sub-agent's sed -i call must still be allowed post-reorder"
    );

    let make = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"make build"}}"#,
        &[("CLAUDE_MPM_SUB_AGENT", "1")],
    );
    assert_eq!(
        make.trim(),
        "",
        "a nested MPM sub-agent's make build call must still be allowed post-reorder"
    );
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

/// A working directory outside every root in `WORKTREE_TMP_DENYLIST_ROOTS`
/// (`pm_guard_bash/mod.rs`), used to pin the two ALLOW cases below (#5914).
///
/// Why: `resolves_under_denylisted_tmp` is purely lexical and never touches the
/// filesystem, so this path does not have to exist — and must not, since a
/// directory that existed as a git checkout would drag the ADR-0048 rules into
/// a test about the worktree-tmp rule.
const NON_DENYLISTED_CWD: &str = "/projects/example-repo";

/// A `git worktree add` payload whose target is the in-project convention and
/// whose working directory is stated rather than inherited (#5914).
///
/// Why: the guard resolves a RELATIVE worktree target against `hook_cwd` —
/// the payload's own `cwd` field, falling back to the guard process's
/// directory. With no `cwd` in the payload the verdict became the RUNNER's:
/// run from a scratch worktree under `/private/tmp`, the two ALLOW assertions
/// below both saw a DENY, because the resolved target landed under a
/// denylisted root. That is the test's location deciding the test's outcome.
/// What: emits the payload with `cwd` set to [`NON_DENYLISTED_CWD`], and
/// `agent_id` present only when the caller is exercising the native-subagent
/// shape.
/// Test: `pm_guard_allows_worktree_add_under_project_dir_via_subagent_payload`,
/// `pm_guard_claude_mpm_sub_agent_env_still_allows_everything_else`.
fn in_project_worktree_add_payload(agent_id: Option<&str>) -> String {
    let agent = agent_id
        .map(|id| format!(r#""agent_id":"{id}","#))
        .unwrap_or_default();
    format!(
        r#"{{"hook_event_name":"PreToolUse",{agent}"cwd":"{NON_DENYLISTED_CWD}","tool_name":"Bash","tool_input":{{"command":"git worktree add .claude/worktrees/wt-x"}}}}"#
    )
}

#[test]
fn pm_guard_blocks_worktree_add_under_tmp_for_pm_own_call() {
    // The PM's own (non-subagent) `git worktree add /tmp/...` must be denied
    // — this is the baseline the subagent case (below) is compared against.
    for target in [
        "/tmp/wt-x",
        "/private/tmp/wt-x",
        "/var/folders/x1/abc/T/wt-x",
    ] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"git worktree add {target}"}}}}"#
        );
        let stdout = run_pm_guard(&payload, &[]);
        assert_denied(&stdout);
        assert!(
            stdout.contains("3955"),
            "deny message must cite issue #3955: {stdout}"
        );
        assert!(
            stdout.contains(".claude/worktrees"),
            "deny message must name the correct location: {stdout}"
        );
    }
}

#[test]
fn pm_guard_blocks_worktree_add_under_scratchpad_for_pm_own_call() {
    // The harness scratchpad is the subtle case: agents are told to prefer it
    // "instead of /tmp", so it looks compliant while still landing under the
    // denylisted /private/tmp root.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /private/tmp/claude-502/some-session/scratchpad/wt-x"}}"#;
    assert_denied(&run_pm_guard(payload, &[]));
}

#[test]
fn pm_guard_blocks_worktree_add_under_tmp_via_subagent_payload() {
    // THE case the whole task turns on: a native Task/Agent-dispatched
    // subagent (agent_id present) attempting `git worktree add` under /tmp
    // must ALSO be denied. Guard 4's subagent exemption would normally allow
    // an arbitrary Bash call from this payload shape (see
    // `pm_guard_native_subagent_dispatch_allows_bash` above) — proving this
    // case denies proves the worktree-tmp check fires BEFORE that exemption,
    // not through the ordinary `evaluate_tool`/`evaluate_bash_command` path.
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-xyz789","agent_type":"rust-engineer","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#;
    let stdout = run_pm_guard(payload, &[]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("3955"),
        "deny message must cite issue #3955: {stdout}"
    );
}

#[test]
fn pm_guard_blocks_worktree_add_under_tmp_via_claude_mpm_sub_agent_env() {
    // Round 2 (code-critic BLOCK on PR #3978): `CLAUDE_MPM_SUB_AGENT=1` — the
    // automatic marker trusty-agents' own subprocess/runner spawn helpers
    // stamp on every nested subagent process (spawn.rs:189-192,
    // claude_code_runner/run.rs:211-215) — must NOT exempt a `git worktree
    // add /tmp/...` call, exactly like the `agent_id` case above. Before this
    // fix, Guard 1 short-circuited to ALLOW before the stdin payload was even
    // read, so this call was silently allowed. This is THE regression test
    // for that finding.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#;
    let stdout = run_pm_guard(payload, &[("CLAUDE_MPM_SUB_AGENT", "1")]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("3955"),
        "deny message must cite issue #3955: {stdout}"
    );
}

#[test]
fn pm_guard_claude_mpm_sub_agent_env_still_allows_everything_else() {
    // Companion to the above: Guard 1's original "exempt everything else for
    // a nested MPM subagent" behavior must be unchanged for calls that are
    // NOT `git worktree add` under a denylisted root — including source-code
    // Edit, which `pm_guard_sub_agent_env_allows_all` already covers, and a
    // worktree add targeting an in-project path.
    let ok_worktree = run_pm_guard(
        // #5914: the payload names its own working directory, so the relative
        // target resolves the same wherever the suite runs.
        &in_project_worktree_add_payload(None),
        &[("CLAUDE_MPM_SUB_AGENT", "1")],
    );
    assert_eq!(
        ok_worktree.trim(),
        "",
        "an in-project worktree target under CLAUDE_MPM_SUB_AGENT must stay allowed"
    );
}

#[test]
fn pm_guard_disable_hooks_env_still_allows_worktree_add_under_tmp() {
    // `TRUSTY_MPM_DISABLE_HOOKS` is a genuine operator-set escape hatch (never
    // set programmatically anywhere in this codebase) — unlike
    // `CLAUDE_MPM_SUB_AGENT` above, it is deliberately NOT pierced: an
    // operator who disables the hook entirely gets exactly that, including
    // for the worktree-tmp guard.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#,
        &[("TRUSTY_MPM_DISABLE_HOOKS", "1")],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "the disable-hooks operator escape hatch must still allow everything, \
         including the worktree-tmp guard"
    );
}

#[test]
fn pm_guard_unrestricted_env_still_allows_worktree_add_under_tmp() {
    // `TRUSTY_MPM_PM_UNRESTRICTED=1` — the explicit "the operator said you do
    // it this time" override — is likewise a genuine human escape hatch and
    // must still lift the worktree-tmp guard along with everything else.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#,
        &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")],
    );
    assert_eq!(
        stdout.trim(),
        "",
        "the unrestricted operator bypass must still allow everything, \
         including the worktree-tmp guard"
    );
}

#[test]
fn pm_guard_allows_worktree_add_under_project_dir_via_subagent_payload() {
    // The companion positive case: the SAME subagent payload shape, but
    // targeting the documented in-project convention, must be allowed.
    // #5914: the payload names its own working directory, so the relative
    // target resolves the same wherever the suite runs.
    let payload = in_project_worktree_add_payload(Some("agent-xyz789"));
    assert_eq!(
        run_pm_guard(&payload, &[]).trim(),
        "",
        "an in-project worktree target must be allowed even unbudgeted \
         (worktree add is not a budget-eligible file-change deny)"
    );
}

#[test]
fn pm_guard_worktree_add_denial_is_not_budget_eligible() {
    // Unlike source-code Edit/Write or shell file edits, a worktree-tmp deny
    // must be an ABSOLUTE prohibition, not consumed against/gated by the
    // per-turn file-change budget — repeating the call must deny every time.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#;
    for _ in 1..=4 {
        assert_denied(&run_pm_guard(payload, &[("HOME", &home_s)]));
    }
}

#[test]
fn pm_guard_allows_non_add_worktree_subcommands_and_ordinary_temp_usage() {
    // list/remove/prune must never be blocked BY THE WORKTREE-ADD RULE, and
    // ordinary temp usage (mktemp, temp-file writes, cargo build artifacts)
    // must keep working. These payloads carry no subagent marker, so they are
    // the PM's own calls; #5791 denies `remove` only to a subagent, and that
    // arm has its own tests below.
    for command in [
        "git worktree list",
        "git worktree remove /tmp/wt-x",
        "git worktree prune",
        "mktemp -d",
        "cargo build --target-dir /tmp/build-cache",
    ] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#
        );
        assert_eq!(
            run_pm_guard(&payload, &[]).trim(),
            "",
            "expected allow for: {command}"
        );
    }
}

#[test]
fn pm_guard_denies_destructive_delete_of_filesystem_roots() {
    // Issue #4031: no `rm`/`rmdir`/`unlink`/`find -delete` verb existed
    // anywhere in the classifier. Every case here is a native
    // Task/Agent-dispatched subagent (`agent_id` present) — proving these deny
    // proves the rule fires BEFORE Guard 4's subagent exemption, the same
    // property `pm_guard_blocks_worktree_add_under_tmp_via_subagent_payload`
    // proves for the sibling worktree-tmp guard.
    for command in ["rm -rf /", "rm -rf /root", "unlink /root"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","agent_id":"agent-xyz789","agent_type":"rust-engineer","tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#
        );
        let stdout = run_pm_guard(&payload, &[]);
        assert_denied(&stdout);
        assert!(
            stdout.contains("4031"),
            "deny message must cite issue #4031: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_destructive_delete_of_repo_root_and_dot_git() {
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        format!("rm -rf {}", repo.display()),
        "rm -rf .git".to_string(),
        "rmdir .git".to_string(),
        "find . -delete".to_string(),
        "cargo build && rm -rf .git".to_string(),
    ] {
        let stdout = run_pm_guard(&bash_payload_at(&command, &repo, ""), &[]);
        assert_denied(&stdout);
    }
}

#[test]
fn pm_guard_denies_destructive_delete_of_a_worktree_root() {
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /projects/example-repo/.claude/worktrees/agent-x"}}"#;
    assert_denied(&run_pm_guard(payload, &[]));
}

#[test]
fn pm_guard_allows_ordinary_delete_cleanup() {
    // The other half of #4031's scope: none of these target a denylisted
    // root, so they must stay allowed — the same fixture used for the deny
    // cases above proves this isn't passing merely because the cwd resolves
    // to nothing.
    // `git clean -fd` is deliberately excluded here: ADR-0037 already denies
    // it in a MAIN CHECKOUT (a pre-existing, unrelated rule), so asserting
    // ALLOW for it against `main_checkout_fixture` would test that guard's
    // absence rather than this one's.
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        "rm stale.txt",
        "rm -rf crates/trusty-mpm/src",
        "rmdir empty-dir",
        "cargo clean",
    ] {
        assert_eq!(
            run_pm_guard(&bash_payload_at(command, &repo, ""), &[]).trim(),
            "",
            "expected allow for: {command}"
        );
    }
}

#[test]
fn pm_guard_destructive_delete_denial_is_not_budget_eligible() {
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /root"}}"#;
    for _ in 1..=4 {
        assert_denied(&run_pm_guard(payload, &[("HOME", &home_s)]));
    }
}

#[test]
fn pm_guard_denies_pwd_expansion_of_the_current_worktree_root() {
    // #4031 review, CRITICAL 1: `$PWD` reached the guard unexpanded, so a
    // dispatched subagent's `rm -rf $PWD` from inside its OWN worktree root
    // matched no denylist entry. `agent_id` is present so this also proves
    // the rule still pierces Guard 4.
    let worktree = std::path::Path::new("/projects/example-repo/.claude/worktrees/agent-x");
    for command in ["rm -rf $PWD", "rm -rf ${PWD}"] {
        let payload = bash_payload_at(
            command,
            worktree,
            r#""agent_id":"agent-xyz789","agent_type":"rust-engineer","#,
        );
        assert_denied(&run_pm_guard(&payload, &[]));
    }
}

#[test]
fn pm_guard_allows_pwd_expansion_of_a_subdirectory() {
    // The companion allow case: `$PWD/target` is an ordinary subdirectory of
    // the worktree, not its root.
    let worktree = std::path::Path::new("/projects/example-repo/.claude/worktrees/agent-x");
    let payload = bash_payload_at("rm -rf $PWD/target", worktree, "");
    assert_eq!(run_pm_guard(&payload, &[]).trim(), "");
}

#[test]
fn pm_guard_denies_glob_suffixed_deletes_of_home() {
    // #4031 review, CRITICAL 2: this classifier never expands a glob — a
    // literal `$HOME/*` or a `./*` run from `$HOME` must still deny by
    // evaluating the glob's PARENT (here, `$HOME` itself) against the
    // denylist. `HOME` is pinned via `isolated_home` so the assertion does
    // not depend on the runner's real home directory.
    let home = isolated_home();
    let home_s = home.path().to_string_lossy().to_string();
    for command in ["rm -rf $HOME/*", "rm -rf ./*"] {
        let payload = bash_payload_at(command, home.path(), "");
        assert_denied(&run_pm_guard(&payload, &[("HOME", &home_s)]));
    }
}

#[test]
fn pm_guard_allows_a_glob_inside_a_worktree_subdirectory() {
    // The companion allow case: a glob whose parent is an ORDINARY
    // subdirectory (not a denylisted root) is not this rule's business.
    let worktree = std::path::Path::new("/projects/example-repo/.claude/worktrees/agent-x");
    let payload = bash_payload_at("rm -rf ./target/*", worktree, "");
    assert_eq!(run_pm_guard(&payload, &[]).trim(), "");
}

#[test]
fn pm_guard_denies_backslash_and_command_wrapper_bypasses() {
    // #4031 review, HIGH 3/4: `\rm` and `command rm` are the standard POSIX
    // alias-bypass idioms — both run the real `rm` exactly as `rm` does.
    // Built via `serde_json::json!` rather than string interpolation so the
    // literal backslash is JSON-escaped correctly (a hand-interpolated
    // `"\rm …"` decodes as a carriage return, not a backslash).
    for command in [r"rm -rf /root", r"\rm -rf /root", "command rm -rf /root"] {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command}
        })
        .to_string();
        assert_denied(&run_pm_guard(&payload, &[]));
    }
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

// ---------------------------------------------------------------------------
// Subagent fan-out denial (issue #4784) — `commands::pm_guard_fanout`.
//
// The pure policy is unit-tested in that module; these cases prove the two
// caller-context markers actually resolve through the real binary (env is
// controlled per-process here, which a threaded unit test cannot do safely)
// and that the decision reaches stdout in the documented deny shape.
// ---------------------------------------------------------------------------

/// Assert the printed deny names the fan-out rule and its remedy, not just
/// "denied" — the message is the whole point of the guard for the agent that
/// hits it.
fn assert_fanout_denied(stdout: &str) {
    assert_denied(stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(
        reason.contains("#4784") && reason.contains("report back to the PM"),
        "the fan-out deny must name the rule and the remedy, got: {reason}"
    );
}

#[test]
fn pm_guard_denies_agent_dispatch_from_native_subagent() {
    // A native Task/Agent-dispatched subagent (agent_id present) trying to
    // dispatch again. This is the case #4784 exists for, and it must fire
    // AHEAD of Guard 4, which would otherwise exempt this exact payload.
    for tool in ["Agent", "Task"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","agent_type":"rust-engineer","tool_name":"{tool}","tool_input":{{"subagent_type":"qa","prompt":"go"}}}}"#
        );
        assert_fanout_denied(&run_pm_guard(&payload, &[]));
    }
}

#[test]
fn pm_guard_denies_task_dispatch_from_mpm_subagent_env() {
    // trusty-agents spawns subagents as their own top-level Claude Code
    // sessions, which carry no `agent_id` — `CLAUDE_MPM_SUB_AGENT` is the only
    // marker available there. Without this arm the guard is a no-op for that
    // whole class. It must also fire ahead of Guard 1, which exempts this
    // process for everything else.
    for tool in ["Agent", "Task"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{{"subagent_type":"qa","prompt":"go"}}}}"#
        );
        assert_fanout_denied(&run_pm_guard(&payload, &[("CLAUDE_MPM_SUB_AGENT", "1")]));
    }
}

#[test]
fn pm_guard_allows_agent_dispatch_from_pm() {
    // The arm every delegation depends on: the PM carries neither marker, so
    // its dispatches must pass silently. A regression here halts every
    // delegation in the system, which is strictly worse than the fan-out this
    // guard prevents.
    //
    // #5708: pinned outside any checkout so this asserts the FAN-OUT rule
    // alone. In a main checkout ADR-0048 rewrites the same payload to add
    // `isolation`, which is a different rule with its own tests.
    for tool in ["Agent", "Task"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{{"subagent_type":"rust-engineer","prompt":"go"}}}}"#
        );
        assert_eq!(
            run_pm_guard_outside_a_checkout(&payload).trim(),
            "",
            "the PM's own {tool} dispatch must be allowed"
        );
    }
}

#[test]
fn pm_guard_fanout_fails_open_on_indeterminate_caller() {
    // An empty-string `agent_id` and a payload with no marker at all are both
    // INDETERMINATE. Indeterminate must ALLOW (#4784): a false deny against
    // the PM halts orchestration; a false allow reproduces prior behaviour.
    //
    // #5708: pinned outside any checkout because these payloads carry no
    // `subagent_type` and are therefore also UNTYPED dispatches, which ADR-0048
    // deliberately isolates in a main checkout rather than failing open. The two
    // rules disagree by design; this one is asserted where only it can fire.
    for payload in [
        r#"{"hook_event_name":"PreToolUse","agent_id":"","tool_name":"Agent","tool_input":{"prompt":"go"}}"#,
        r#"{"hook_event_name":"PreToolUse","agent_type":"rust-engineer","tool_name":"Agent","tool_input":{"prompt":"go"}}"#,
    ] {
        assert_eq!(
            run_pm_guard_outside_a_checkout(payload).trim(),
            "",
            "an indeterminate caller context must fail OPEN: {payload}"
        );
    }
}

#[test]
fn pm_guard_never_denies_send_message() {
    // SendMessage is how a fan-out-denied agent reports back and gets resumed.
    // Denying it would strand the agent this guard redirects, so it is asserted
    // in every caller context — including both subagent markers.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"SendMessage","tool_input":{"agent_id":"x","message":"done"}}"#;
    let sub_payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"SendMessage","tool_input":{"agent_id":"x","message":"done"}}"#;
    assert_eq!(run_pm_guard(payload, &[]).trim(), "");
    assert_eq!(run_pm_guard(sub_payload, &[]).trim(), "");
    assert_eq!(
        run_pm_guard(payload, &[("CLAUDE_MPM_SUB_AGENT", "1")]).trim(),
        "",
        "SendMessage must never be denied, in any caller context"
    );
}

#[test]
fn pm_guard_fanout_yields_to_operator_escape_hatches() {
    // Guards 2/3 are human escape hatches and stay ahead of every rule in the
    // file, this one included — an operator who explicitly lifts enforcement
    // gets exactly that. Pinned so a future reordering is a deliberate choice.
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Agent","tool_input":{"prompt":"go"}}"#;
    assert_eq!(
        run_pm_guard(payload, &[("TRUSTY_MPM_DISABLE_HOOKS", "1")]).trim(),
        ""
    );
    assert_eq!(
        run_pm_guard(payload, &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")]).trim(),
        ""
    );
}

#[test]
fn pm_guard_subagent_keeps_its_working_tool_surface() {
    // Only fan-out is cut. A subagent's ordinary work — the Edit/Write calls
    // Guard 4 exists to exempt (#2014) — must still be allowed, proving the
    // new guard narrowed nothing else.
    let edit = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Edit","tool_input":{"file_path":"/x/a.rs","old_string":"a","new_string":"b"}}"#;
    assert_eq!(run_pm_guard(edit, &[]).trim(), "");
    let read = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}"#;
    assert_eq!(run_pm_guard(read, &[]).trim(), "");
}

// ---------------------------------------------------------------------------
// Agent-side worktree removal denial (issue #5791) —
// `commands::pm_guard_bash::worktree_remove`.
//
// The pure policy is unit-tested in that module; these cases prove the caller
// context resolves through the real binary and that the ruling's remedy
// reaches the agent in the documented deny shape.
// ---------------------------------------------------------------------------

/// Assert the printed deny names the ruling and the command that replaces it.
fn assert_worktree_remove_denied(stdout: &str) {
    assert_denied(stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(
        reason.contains("#5791") && reason.contains("tm session prune-worktrees"),
        "the worktree-removal deny must name the ruling and the PM's command, got: {reason}"
    );
}

#[test]
fn pm_guard_denies_worktree_remove_from_native_subagent() {
    // The case the owner ruling exists for: an agent cleaning up after a merge
    // it completed. It must fire AHEAD of Guard 4, which would otherwise exempt
    // this exact payload.
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","agent_type":"version-control","tool_name":"Bash","tool_input":{"command":"git worktree remove --force .claude/worktrees/agent-x"}}"#;
    assert_worktree_remove_denied(&run_pm_guard(payload, &[]));
}

#[test]
fn pm_guard_denies_worktree_remove_from_mpm_subagent_env() {
    // trusty-agents spawns subagents as their own top-level Claude Code
    // sessions, which carry no `agent_id`; without this arm the rule is a no-op
    // for that whole class, and it must fire ahead of Guard 1.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree remove /some/tree"}}"#;
    assert_worktree_remove_denied(&run_pm_guard(payload, &[("CLAUDE_MPM_SUB_AGENT", "1")]));
}

#[test]
fn pm_guard_allows_worktree_remove_from_pm() {
    // The ruling puts the PM in charge of the removal, so the PM's own call
    // must pass silently — a regression here breaks the sanctioned path.
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree remove --force .claude/worktrees/agent-x"}}"#;
    assert_eq!(run_pm_guard(payload, &[]).trim(), "");
}

#[test]
fn pm_guard_leaves_a_subagents_read_only_worktree_verbs_alone() {
    // Only removal is cut. Reading and repairing the registry stay available,
    // so an agent can still report what it found.
    for command in ["git worktree list", "git worktree prune"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#
        );
        assert_eq!(
            run_pm_guard(&payload, &[]).trim(),
            "",
            "expected allow for: {command}"
        );
    }
}

#[test]
fn pm_guard_worktree_remove_yields_to_operator_escape_hatches() {
    // Guards 2/3 are human escape hatches and stay ahead of every rule in the
    // file, this one included.
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-abc123","tool_name":"Bash","tool_input":{"command":"git worktree remove /some/tree"}}"#;
    assert_eq!(
        run_pm_guard(payload, &[("TRUSTY_MPM_DISABLE_HOOKS", "1")]).trim(),
        ""
    );
    assert_eq!(
        run_pm_guard(payload, &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")]).trim(),
        ""
    );
}

// ---------------------------------------------------------------------------
// #4480 — concurrent shared-working-tree dispatch denial
// ---------------------------------------------------------------------------

/// Spawn `tm hook --pm-guard` against a chosen daemon URL, from a chosen cwd.
///
/// Why: the #4480 guard's verdict depends on two things
/// [`run_pm_guard`] fixes by design — the daemon URL (it pins an unreachable
/// one, which is the fail-open case) and the process working directory (the
/// guard compares it against what the daemon recorded). Both have to be
/// controllable to exercise the DENY arm end to end.
/// What: as [`run_pm_guard`], plus an explicit `--url` and `current_dir`.
fn run_pm_guard_at(stdin_json: &str, url: &str, cwd: &std::path::Path) -> String {
    run_pm_guard_at_with_env(stdin_json, url, cwd, &[])
}

/// Spawn `tm hook --pm-guard` standing in a directory that is no git checkout.
///
/// Why (#5708): [`run_pm_guard`] inherits the runner's working directory, and
/// the ADR-0048 worktree grant fires only in a main checkout. Three tests
/// asserting a silent allow therefore passed from a worktree and failed from
/// CI's `actions/checkout` clone — one denied, two got an `updatedInput`
/// rewrite — off the same binary at the same commit. Pinning a directory that
/// is no checkout makes the verdict the payload's rather than the runner's.
/// What: [`run_pm_guard_at`] against the same unreachable daemon URL
/// [`run_pm_guard`] fixes, in a fresh empty tempdir — no `.git` entry of either
/// shape, so `is_main_checkout` answers false wherever the suite runs. For
/// payloads whose rule is cwd-INSENSITIVE by intent only; an assertion ABOUT a
/// tree belongs in [`run_pm_guard_at`] with a fixture naming the tree it wants.
/// Test: `pm_guard_allows_git_status_and_task`,
/// `pm_guard_allows_agent_dispatch_from_pm`,
/// `pm_guard_fanout_fails_open_on_indeterminate_caller`.
fn run_pm_guard_outside_a_checkout(stdin_json: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    run_pm_guard_at(stdin_json, UNREACHABLE_DAEMON, dir.path())
}

/// [`run_pm_guard_at`] returning STDERR instead of stdout.
///
/// Why (#5769): one guard signal is deliberately not a verdict. When the daemon
/// answers but records nothing — an older daemon 404ing the granted-worktree
/// route is the realistic shape — the grant is still emitted, because failing
/// closed on version skew would block every dispatch from a main checkout. The
/// only trace is a stderr line, and stdout-only helpers cannot see it, so a
/// silent regression there would leave every test green.
/// What: as [`run_pm_guard_at`], but hands back the child's stderr.
fn run_pm_guard_at_stderr(stdin_json: &str, url: &str, cwd: &std::path::Path) -> String {
    let child = spawn_pm_guard(stdin_json, url, Some(cwd), &[]);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "the guard must always exit 0");
    String::from_utf8(output.stderr).expect("stderr is utf8")
}

/// [`run_pm_guard_at`] with extra environment, for the rules that must fire
/// THROUGH an automatic subagent marker (`CLAUDE_MPM_SUB_AGENT`) or yield to an
/// operator escape hatch. Those cases need the daemon URL as well, which
/// [`run_pm_guard`] fixes at an unreachable address.
fn run_pm_guard_at_with_env(
    stdin_json: &str,
    url: &str,
    cwd: &std::path::Path,
    extra_env: &[(&str, &str)],
) -> String {
    finish_pm_guard(spawn_pm_guard(stdin_json, url, Some(cwd), extra_env))
}

/// A one-shot HTTP stand-in for the daemon's shared-tree-dispatch route.
///
/// Why: the DENY arm cannot be reached without a daemon that answers, and a
/// real daemon is far too much machinery for one route's wire contract. A raw
/// [`std::net::TcpListener`] is the lightest dependency-free mock and mirrors
/// the technique `pm_guard_deny_by_default`'s tests already use.
/// What: binds an ephemeral port, serves ONE request with the given JSON body,
/// and returns the `http://127.0.0.1:PORT` base URL. The accept loop runs on a
/// detached thread; the process exits with the test binary.
fn spawn_writers_mock(body: &'static str) -> String {
    spawn_capturing_writers_mock(body).0
}

/// [`spawn_writers_mock`] that also hands back what the guard actually sent.
///
/// Why (#5769): a mock answering a canned body regardless of the request cannot
/// tell a correct query from a broken one. The HEAD-move rule keys its query by
/// DIRECTORY, and the directory it sends has to be one a delegation record can
/// carry — nothing pinned that, which is why a `cd`-into-a-subdirectory query
/// keyed a path no record matches and allowed the move with the guard silently
/// off. Capturing the body makes the key assertable.
/// What: as [`spawn_writers_mock`], plus a handle whose `posted_cwd()` blocks
/// briefly for the request and returns its `payload.cwd`. `None` means no
/// request arrived, which is itself an assertable outcome.
fn spawn_capturing_writers_mock(body: &'static str) -> (String, CapturedRequest) {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            // Read until the body is complete: `Content-Length` bounds it, and a
            // single `read` is not guaranteed to return the whole request.
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let Some((head, rest)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length: ")
                            .or(l.strip_prefix("Content-Length: "))
                    })
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if rest.len() >= len {
                    let _ = tx.send(rest.to_string());
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    (url, CapturedRequest(rx))
}

/// The request body one [`spawn_capturing_writers_mock`] received.
struct CapturedRequest(std::sync::mpsc::Receiver<String>);

impl CapturedRequest {
    /// The forwarded hook payload the guard sent, or `None` if nothing arrived.
    ///
    /// The five seconds are a deadlock backstop, not a wait, so they carry no
    /// load sensitivity (#5914). Every caller collects the guard child first,
    /// and the mock sends on this channel BEFORE it writes the response the
    /// child is waiting for — so the value is already queued by the time any
    /// caller asks, and the receive returns without blocking.
    fn posted(&self) -> Option<serde_json::Value> {
        let raw = self
            .0
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("the guard must POST valid JSON: {e}: {raw}"));
        Some(parsed["payload"].clone())
    }
}

#[test]
fn pm_guard_denies_a_second_unisolated_engineer_dispatch() {
    // Causality (#4480): against PRE-FIX code this payload is allowed with no
    // output at all — `evaluate_tool` allows `Agent` unconditionally and no
    // branch ever consults the daemon. The deny below can only come from the
    // new guard.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"python-engineer","count":1}],"total":1}"#);
    let cwd = tempfile::tempdir().expect("tempdir");
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_second","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","prompt":"go"}}"#;

    let stdout = run_pm_guard_at(payload, &url, cwd.path());
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"].as_str(),
        Some("deny")
    );
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(
        reason.contains("#4480") && reason.contains("python-engineer"),
        "the deny must name the rule and the agent already in the tree, got: {reason}"
    );
}

#[test]
fn pm_guard_allows_an_isolated_engineer_dispatch() {
    // The remedy the deny asks for must work even with a sibling in the tree,
    // and it must not even reach the daemon — the mock is deliberately never
    // consumed here.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"python-engineer","count":1}],"total":1}"#);
    let cwd = tempfile::tempdir().expect("tempdir");
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_iso","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","isolation":"worktree","prompt":"go"}}"#;
    assert_eq!(run_pm_guard_at(payload, &url, cwd.path()).trim(), "");
}

#[test]
fn pm_guard_allows_a_read_only_dispatch_beside_a_running_engineer() {
    // Research/review fan-out beside a running engineer is the ordinary
    // workflow; denying it would be a far worse regression than the race.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let cwd = tempfile::tempdir().expect("tempdir");
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_res","tool_name":"Agent","tool_input":{"subagent_type":"research","prompt":"go"}}"#;
    assert_eq!(run_pm_guard_at(payload, &url, cwd.path()).trim(), "");
}

#[test]
fn pm_guard_allows_an_engineer_dispatch_into_an_empty_tree() {
    // The first dispatch of a session — the overwhelmingly common case.
    let url = spawn_writers_mock(r#"{"agents":[],"total":0}"#);
    let cwd = tempfile::tempdir().expect("tempdir");
    let payload = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_first","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","prompt":"go"}}"#;
    assert_eq!(run_pm_guard_at(payload, &url, cwd.path()).trim(), "");
}

/// A one-shot HTTP mock returning an arbitrary status line and body.
///
/// Why: [`spawn_writers_mock`] always answers 200 with a well-formed body, so
/// it cannot drive the guard's malformed-response fail-open branch.
/// What: as [`spawn_writers_mock`], but the caller supplies the status line
/// (e.g. `"500 Internal Server Error"`) and the raw body bytes.
fn spawn_writers_mock_with(status_line: &'static str, body: &'static str) -> String {
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    url
}

/// A mock that ACCEPTS the connection and never answers (#5923).
///
/// Why: the reported fail-open rides in on a request that timed out, and a
/// refused port cannot stand in for it — the guard's distinction is precisely
/// between a socket nobody is listening on and a daemon that took the
/// connection and went quiet. Only a real accepted-then-silent listener
/// exercises the arm.
/// What: as [`spawn_writers_mock_with`], but it reads the request and holds the
/// socket open past the guard's 2 s total budget. The detached thread ends with
/// the test binary.
fn spawn_silent_mock() -> String {
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf);
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });
    url
}

/// The dispatch payload every #5923 case below sends: an unisolated engineer.
const UNISOLATED_ENGINEER_DISPATCH: &str = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_bad","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","prompt":"go"}}"#;

#[test]
fn pm_guard_denies_when_a_running_daemon_does_not_answer() {
    // #5923: a daemon that accepted the connection and did not answer inside
    // the 2 s budget may already hold another writer's record, so the guard
    // cannot read its silence as an empty tree. Against the pre-fix binary all
    // three of these print nothing at all — the ALLOW this closes.
    let cases: [(&'static str, String); 3] = [
        ("a request that timed out", spawn_silent_mock()),
        (
            "a 500 from a daemon that has the route",
            spawn_writers_mock_with("500 Internal Server Error", r#"{"error":"boom"}"#),
        ),
        (
            "a body that is not JSON",
            spawn_writers_mock_with("200 OK", "not json at all"),
        ),
    ];
    for (case, url) in cases {
        let cwd = tempfile::tempdir().expect("tempdir");
        let stdout = run_pm_guard_at(UNISOLATED_ENGINEER_DISPATCH, &url, cwd.path());
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{case} must deny with JSON on stdout: {e}: {stdout:?}"));
        assert_eq!(
            parsed["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny"),
            "{case} must fail CLOSED, got: {stdout}"
        );
        let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("reason is a string");
        assert!(
            reason.contains("#5923") && reason.contains("isolation"),
            "{case}: the deny must name the rule and the remedy that needs no daemon, got: {reason}"
        );
    }
}

#[test]
fn pm_guard_allows_when_the_daemon_answer_has_the_wrong_shape() {
    // The boundary of the deny above, and a declared fail-open branch #5923
    // deliberately keeps: an answer that PARSES but carries no readable writer
    // list is still an answer. `warn_on_eligibility_divergence` covers it, and
    // denying there would block every dispatch against a daemon whose reply
    // shape drifted. A 404 belongs here too — it is a daemon older than this
    // `tm`, not a daemon that failed.
    let cases: [(&'static str, &'static str); 3] = [
        ("200 OK", r#"{"agents":"rust-engineer"}"#),
        ("200 OK", r#"{"unexpected":"shape"}"#),
        ("404 Not Found", r#"{"error":"no such route"}"#),
    ];
    for (status_line, body) in cases {
        let url = spawn_writers_mock_with(status_line, body);
        let cwd = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            run_pm_guard_at(UNISOLATED_ENGINEER_DISPATCH, &url, cwd.path()).trim(),
            "",
            "a {status_line} / {body} answer must fail OPEN"
        );
    }
}

#[test]
fn pm_guard_warns_when_no_daemon_answers_the_claim() {
    // #5923: an absent daemon still ALLOWS — denying would stop every dispatch
    // on a machine with no daemon running — but it must not do so silently.
    // The silence is what let the fail-open sit unnoticed: an operator running
    // without a daemon saw a guard that looked like it was working.
    let cwd = tempfile::tempdir().expect("tempdir");
    let stderr = run_pm_guard_at_stderr(
        UNISOLATED_ENGINEER_DISPATCH,
        "http://127.0.0.1:1",
        cwd.path(),
    );
    assert!(
        stderr.contains("#5923") && stderr.contains("NOT being enforced"),
        "an unreachable daemon must say the guard is off, got: {stderr:?}"
    );
    assert_eq!(
        run_pm_guard_at(
            UNISOLATED_ENGINEER_DISPATCH,
            "http://127.0.0.1:1",
            cwd.path()
        )
        .trim(),
        "",
        "and it must still allow the dispatch"
    );
}

/// Serve the REAL delegation router, releasing requests only once `expected` of
/// them have arrived (#5324).
///
/// Why: the race this probes is "two dispatches both reach the daemon before
/// either is recorded". Creating that window with a sleep would make the test's
/// outcome depend on scheduling; holding every request at a barrier until all
/// `expected` are in makes it a property of the harness's control flow instead.
/// No clock, virtual or real, is involved — deliberately, since tokio's
/// `start_paused` auto-advances virtual time whenever all tasks are idle and
/// would make an inserted delay evaporate (#3494).
///
/// Why the real router and not a canned mock: before #5324 the whole decision
/// lived in the guard, so a mock that always answered "nobody here" was enough
/// to pin it. The fix moved the deciding half into the daemon — the answer and
/// the record are now one operation — so a mock could only re-implement the
/// thing under test. This stands up `delegation_routes::router()` over a real
/// [`DaemonState`], which is what actually holds the claim.
///
/// What: binds an ephemeral port, serves the delegation sub-router behind a
/// [`tokio::sync::Barrier`] applied with `route_layer` — matched routes only, so
/// the deny path's best-effort audit POST to the unrouted `/hooks` 404s
/// immediately instead of waiting on a barrier no one else will reach. Returns
/// the base URL and the temp dir backing the hermetic state.
fn serve_delegation_router_behind_a_barrier(expected: usize) -> (String, tempfile::TempDir) {
    use std::future::IntoFuture;
    use std::sync::Arc;

    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::daemon::{delegation_routes, state::DaemonState};

    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::with_paths(&FrameworkPaths::under(dir.path())));
    let barrier = Arc::new(tokio::sync::Barrier::new(expected));

    let app = delegation_routes::router()
        .route_layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let barrier = Arc::clone(&barrier);
                async move {
                    // Barrier passed: every guard's claim has reached the
                    // daemon and none has been served. This is the exact state
                    // the race needs; only the daemon's own critical section
                    // decides what happens next.
                    barrier.wait().await;
                    next.run(req).await
                }
            },
        ))
        .with_state(state);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("addr");
    let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
    tokio::spawn(axum::serve(listener, app).into_future());
    (format!("http://{addr}"), dir)
}

/// Run one throwaway `tm hook --pm-guard` so the next exec is not the cold one
/// (#5914).
///
/// Why: the concurrency test below is the only test whose correctness depends
/// on how long a child takes to START, because the barrier holds one child's
/// request open across the other's startup and the guard's HTTP client gives up
/// after 2 s. Paging a ~100 MB debug binary in is the whole of that cost, and it
/// is paid once per machine, not once per exec — so moving it ahead of the
/// barrier removes it from the window entirely rather than budgeting for it.
/// What: the cheapest complete path through the same binary — a `Read` payload,
/// which no rule gates and which never dials the daemon. The verdict is
/// discarded; only the page cache it leaves behind is wanted.
fn prewarm_pm_guard_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}"#;
    let verdict = run_pm_guard_at(payload, UNREACHABLE_DAEMON, dir.path());
    assert_eq!(
        verdict.trim(),
        "",
        "the warm-up payload must be one no rule gates"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pm_guard_denies_the_second_of_two_simultaneous_dispatches() {
    // #5324, the window #4480 left open. Two `Agent` dispatches issued in one PM
    // turn — the framework's own documented pattern for parallel work — both
    // reach the daemon before either is recorded. Pre-fix the daemon only
    // ANSWERED, so both saw an empty set and both were ALLOWED, which is the
    // collision the guard exists to prevent. The claim is now taken inside the
    // same critical section that produced the answer, so exactly one is
    // admitted.
    //
    // The interleaving is forced, not timed: the barrier holds both claims until
    // both have arrived, so the pre-fix code cannot pass by winning a race.
    // Which of the two is denied is genuinely arbitrary — that is the point of a
    // mutual exclusion — so the assertion counts verdicts rather than ordering
    // them.
    //
    // What this pins, precisely: that a claim is taken at all, end to end,
    // through the real binary and the real router. It does NOT pin atomicity.
    // The barrier releases both requests into the handler and stops there, so
    // how far each gets before the other resumes is the scheduler's business —
    // moving `record` back outside the mutex fails this test at a 300 ms
    // handler gap but PASSED at 50 ms. Atomicity is pinned deterministically by
    // `shared_tree_dispatch_route_denies_the_second_claim`
    // (`src/daemon/delegation_routes_tests.rs`), which drives the two claims
    // through `claim_shared_tree_dispatch` directly with no scheduler in the
    // way. Treat the two as a pair: this one proves the wiring, that one proves
    // the mutual exclusion.
    // #5914: the barrier holds the FIRST child's HTTP request open until the
    // SECOND child's arrives, and `post_shared_tree` gives up after 2 s and
    // fails OPEN. So the whole of the second child's process startup sits
    // inside a 2 s budget. A COLD first exec of `tm` measured 1.5 s on this
    // machine against 40 ms warm, and under a parallel `cargo build` it crossed
    // the budget: the held request timed out, its guard read the timeout as
    // "nobody else is here", and BOTH dispatches were admitted — the reported
    // `got: ["", ""]`. Two changes take startup out of the window, neither of
    // them a tuned delay: `prewarm_pm_guard_binary` pays the cold-exec cost
    // before the barrier is armed, and both children are forked back to back
    // from this one thread, so what remains between their arrivals is two warm
    // execs, not a thread hand-off plus a page-in.
    let (url, _dir) = serve_delegation_router_behind_a_barrier(2);
    let cwd = tempfile::tempdir().expect("tempdir");
    prewarm_pm_guard_binary();

    let started = std::time::Instant::now();
    let children: Vec<std::process::Child> = ["toolu_race_a", "toolu_race_b"]
        .iter()
        .map(|tool_use_id| {
            let payload = format!(
                r#"{{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"{tool_use_id}","tool_name":"Agent","tool_input":{{"subagent_type":"rust-engineer","prompt":"go"}}}}"#
            );
            spawn_pm_guard(&payload, &url, Some(cwd.path()), &[])
        })
        .collect();

    let verdicts: Vec<String> = children
        .into_iter()
        .map(|child| finish_pm_guard(child).trim().to_string())
        .collect();
    let elapsed = started.elapsed();

    let allowed = verdicts.iter().filter(|v| v.is_empty()).count();
    let denied: Vec<&String> = verdicts.iter().filter(|v| !v.is_empty()).collect();
    assert_eq!(
        allowed, 1,
        "exactly one of two simultaneous dispatches may be admitted, got: {verdicts:?} \
         (both children ran in {elapsed:?}; at or past the guard's 2 s client budget in \
         `post_shared_tree` the held request timed out and failed open — that is the \
         machine, not this rule regressing, see #5914)"
    );
    assert_eq!(denied.len(), 1, "and exactly one must be denied");
    assert_denied(denied[0]);
    let reason: serde_json::Value =
        serde_json::from_str(denied[0]).expect("deny stdout must be valid JSON");
    let reason = reason["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(
        reason.contains("#4480") && reason.contains("rust-engineer"),
        "the deny must name the rule and the sibling already in the tree, got: {reason}"
    );
}

// ── Main-checkout destructive-git guard (ADR-0037) ──────────────────────────
//
// The rule these exercise pierces Guard 1 and Guard 4 exactly like the
// worktree-tmp guard above, so the load-bearing case is a SUBAGENT-marked
// payload: without the piercing, Guard 4 returns ALLOW before the Bash
// classifier is ever reached and the guard does nothing for the incident it
// was built from. Every case drives the working directory through the
// payload's `cwd` field, which `pm_guard` reads ahead of the process
// directory — the test binary's own cwd is inside a real checkout and would
// otherwise decide the verdict.

/// A directory that classifies as a project MAIN CHECKOUT: `.git` is a
/// directory. Returns the `TempDir` guard (which must outlive the test) and
/// the checkout path inside it.
fn main_checkout_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(repo.join("crates/trusty-mpm/src")).expect("mkdir src");
    (dir, repo)
}

/// A `PreToolUse` Bash payload running `command` in `cwd`, with any extra
/// top-level JSON fields (e.g. an `agent_id`) spliced in.
fn bash_payload_at(command: &str, cwd: &std::path::Path, extra_fields: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PreToolUse",{extra_fields}"cwd":"{}","tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#,
        cwd.display()
    )
}

#[test]
fn pm_guard_denies_the_incident_commands_in_a_main_checkout() {
    // The 2026-08-10 incident, verbatim. Two of the three commands are this
    // rule's business; `git reset HEAD` is deliberately NOT denied — the
    // default `--mixed` unstages and destroys nothing, and the owner's
    // boundary lists it as an explicit allow. Denying it would widen the rule
    // past "irreversible destruction" into ordinary git work.
    let (_dir, repo) = main_checkout_fixture();

    for command in [
        "git checkout a1b2c3d -- .",
        "git clean -fdx -- crates/ docs/",
    ] {
        let stdout = run_pm_guard(&bash_payload_at(command, &repo, ""), &[]);
        assert_denied(&stdout);
        assert!(
            stdout.contains("ADR-0037"),
            "the deny must cite the ADR it enforces: {stdout}"
        );
        assert!(
            stdout.contains(&repo.display().to_string()),
            "the deny must name the directory it protected: {stdout}"
        );
    }

    let reset = run_pm_guard(&bash_payload_at("git reset HEAD", &repo, ""), &[]);
    assert_eq!(
        reset.trim(),
        "",
        "`git reset HEAD` unstages without destroying anything and must stay allowed"
    );
}

#[test]
fn pm_guard_denies_every_destructive_form_in_a_main_checkout() {
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        "git reset --hard origin/main",
        "git reset --merge HEAD",
        "git reset --keep HEAD~2",
        "git checkout -- crates/",
        "git checkout .",
        "git checkout -f main",
        "git restore src/lib.rs",
        "git restore --staged --worktree src/",
        "git clean -f",
        "git clean -fdx",
        "git switch --discard-changes main",
        // Composition must not hide the verb behind a benign first segment.
        "git status --short && git reset --hard",
    ] {
        let stdout = run_pm_guard(&bash_payload_at(command, &repo, ""), &[]);
        assert_denied(&stdout);
    }
}

#[test]
fn pm_guard_allows_read_only_and_near_miss_git_in_a_main_checkout() {
    // The false-positive boundary. #5356 is an OPEN P2 filed because pm_guard
    // denied a turn of purely read-only `git status`/`git log`/`ls` calls;
    // this rule must not add to it.
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        "git status",
        "git status --short",
        "git log --oneline -20",
        "git diff HEAD~1 --stat",
        "ls -la",
        "git checkout -b feature/x",
        "git checkout main",
        "git reset",
        "git reset --soft HEAD~1",
        "git restore --staged src/lib.rs",
        "git clean -n",
        "git clean --dry-run -fdx",
        "git switch main",
        "git add -A",
        "git stash list",
    ] {
        let stdout = run_pm_guard(&bash_payload_at(command, &repo, ""), &[]);
        assert_eq!(
            stdout.trim(),
            "",
            "`{command}` must be allowed in a main checkout, got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_main_checkout_destructive_git_from_a_subagent_payload() {
    // THE load-bearing case, and the shape of the actual incident: a native
    // Task/Agent-dispatched subagent (agent_id present) running the
    // destructive command in a main checkout. Guard 4 exempts this payload
    // shape from every ordinary Bash rule (see
    // `pm_guard_native_subagent_dispatch_allows_bash` above), so a deny here
    // can only come from the check running BEFORE that exemption. Remove the
    // piercing and this test is the one that goes green-to-red.
    let (_dir, repo) = main_checkout_fixture();
    let payload = bash_payload_at(
        "git clean -fdx -- crates/ docs/",
        &repo,
        r#""agent_id":"agent-xyz789","agent_type":"local-ops","#,
    );
    let stdout = run_pm_guard(&payload, &[]);
    assert_denied(&stdout);
    assert!(stdout.contains("ADR-0037"), "{stdout}");
}

#[test]
fn pm_guard_denies_main_checkout_destructive_git_from_the_mpm_subagent_env() {
    // The other automatic marker: `CLAUDE_MPM_SUB_AGENT=1`, stamped by
    // trusty-agents on every subagent process it spawns out-of-band. Guard 1
    // exempts it from everything else, and must not exempt it from this.
    let (_dir, repo) = main_checkout_fixture();
    let payload = bash_payload_at("git reset --hard origin/main", &repo, "");
    let stdout = run_pm_guard(&payload, &[("CLAUDE_MPM_SUB_AGENT", "1")]);
    assert_denied(&stdout);
    assert!(stdout.contains("ADR-0037"), "{stdout}");
}

#[test]
fn pm_guard_allows_destructive_git_inside_a_worktree() {
    // Delegated work happens in worktrees and must stay fully writable — this
    // is the exemption the whole rule is scoped around. Asserted for both
    // worktree signals, and from a subagent payload, because that is who is
    // actually working there.
    let dir = tempfile::tempdir().expect("tempdir");
    let claude_wt = dir.path().join("repo/.claude/worktrees/wt-x");
    std::fs::create_dir_all(claude_wt.join(".git")).expect("mkdir worktree");
    let linked_wt = dir.path().join("linked-wt");
    std::fs::create_dir_all(&linked_wt).expect("mkdir linked");
    std::fs::write(linked_wt.join(".git"), "gitdir: /repo/.git/worktrees/x").expect("write .git");

    for cwd in [&claude_wt, &linked_wt] {
        for command in [
            "git reset --hard origin/main",
            "git clean -fdx",
            "git checkout -- .",
        ] {
            let payload = bash_payload_at(command, cwd, r#""agent_id":"agent-xyz789","#);
            let stdout = run_pm_guard(&payload, &[]);
            assert_eq!(
                stdout.trim(),
                "",
                "`{command}` in the worktree {} must be allowed, got: {stdout}",
                cwd.display()
            );
        }
    }
}

#[test]
fn pm_guard_allows_destructive_git_outside_any_checkout() {
    // A declared fail-open arm: a directory with no `.git` ancestor is not a
    // checkout, so there is nothing for this rule to protect and it stays out
    // of the way. A path that does not exist resolves the same way.
    let dir = tempfile::tempdir().expect("tempdir");
    for cwd in [dir.path().to_path_buf(), dir.path().join("no/such/dir")] {
        let stdout = run_pm_guard(&bash_payload_at("git clean -fdx", &cwd, ""), &[]);
        assert_eq!(
            stdout.trim(),
            "",
            "a non-repository directory must be allowed, got: {stdout}"
        );
    }
}

/// A `PreToolUse` payload for any tool, with `tool_input` supplied verbatim.
///
/// Why: the main-checkout write boundary and the worktree grant are driven by
/// `Write`/`Edit` and `Agent` calls, neither of which fits the Bash-shaped
/// helper above.
fn tool_payload_at(
    tool: &str,
    tool_input: &str,
    cwd: &std::path::Path,
    extra_fields: &str,
) -> String {
    format!(
        r#"{{"hook_event_name":"PreToolUse",{extra_fields}"cwd":"{}","tool_name":"{tool}","tool_input":{tool_input}}}"#,
        cwd.display()
    )
}

#[test]
fn pm_guard_denies_a_source_write_in_a_main_checkout() {
    // ADR-0044 decision 1, the half that was never built: an ordinary `Write`
    // to a source file in the shared checkout passed every guard in the
    // process, and that is the write the reported incident was made of.
    let (_dir, repo) = main_checkout_fixture();
    let target = repo.join("crates/trusty-mpm/src/lib.rs");
    let input = format!(
        r#"{{"file_path":"{}","content":"fn main() {{}}"}}"#,
        target.display()
    );
    let stdout = run_pm_guard(&tool_payload_at("Write", &input, &repo, ""), &[]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("ADR-0044"),
        "the deny must cite the decision it enforces: {stdout}"
    );
    assert!(
        stdout.contains("isolation"),
        "the deny must say what to do instead: {stdout}"
    );
}

#[test]
fn pm_guard_denies_a_dispatched_agents_source_write_in_a_main_checkout() {
    // ADR-0044 binds "the PM and every agent it dispatches", so this rule must
    // pierce both automatic subagent markers — Guard 4's `agent_id` payload
    // field and Guard 1's `CLAUDE_MPM_SUB_AGENT` env var. Both early-return
    // ALLOW precisely for the population the rule exists to bind, so a version
    // placed after either would be a no-op for all of it.
    let (_dir, repo) = main_checkout_fixture();
    let target = repo.join("crates/trusty-mpm/src/lib.rs");
    let input = format!(r#"{{"file_path":"{}","content":"x"}}"#, target.display());

    let dispatched = run_pm_guard(
        &tool_payload_at("Write", &input, &repo, r#""agent_id":"agt_1","#),
        &[],
    );
    assert_denied(&dispatched);

    let nested = run_pm_guard(
        &tool_payload_at("Write", &input, &repo, ""),
        &[("CLAUDE_MPM_SUB_AGENT", "1")],
    );
    assert_denied(&nested);
}

#[test]
fn pm_guard_allows_documents_and_configuration_in_a_main_checkout() {
    // The other half of ADR-0044, and the half a "read-only checkout" framing
    // loses: writing projects and configuration maintenance are what a
    // main-checkout session is FOR, and framework deployment writes `.claude/`
    // and `TASK.md` on every launch.
    let (_dir, repo) = main_checkout_fixture();
    for name in [
        "README.md",
        "TASK.md",
        "Cargo.toml",
        ".claude/settings.json",
    ] {
        let target = repo.join(name);
        let input = format!(r#"{{"file_path":"{}","content":"x"}}"#, target.display());
        let stdout = run_pm_guard(&tool_payload_at("Write", &input, &repo, ""), &[]);
        assert_eq!(
            stdout.trim(),
            "",
            "{name} is a document or configuration and must stay writable"
        );
    }
}

#[test]
fn pm_guard_denies_a_commit_in_a_main_checkout() {
    // The commit is where a write becomes permanent on a branch another
    // session is standing on — the step that produced `f1da7bce` landing on a
    // branch belonging to a different workstream.
    //
    // Since ADR-0049 this fixture reaches the deny through the UNKNOWN-index
    // arm: `main_checkout_fixture` fabricates `.git` as a directory, so `git
    // diff --cached` cannot run there and no staged set can license the commit.
    // The documents-only carve-out is exercised against a real repository in
    // `pm_guard_allows_a_documents_only_commit_in_a_main_checkout`.
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard(&bash_payload_at("git commit -m 'wip'", &repo, ""), &[]);
    assert_denied(&stdout);
    assert!(stdout.contains("ADR-0044"), "{stdout}");

    // The composition the PM actually types, and a dispatched agent's commit.
    let chained = run_pm_guard(
        &bash_payload_at("git add -A && git commit -m 'wip'", &repo, ""),
        &[],
    );
    assert_denied(&chained);
    let dispatched = run_pm_guard(
        &bash_payload_at("git commit -m 'wip'", &repo, r#""agent_id":"agt_1","#),
        &[],
    );
    assert_denied(&dispatched);

    // Read-only git is never this rule's business.
    for command in ["git status --short", "git log --oneline -5", "git add -A"] {
        let stdout = run_pm_guard(&bash_payload_at(command, &repo, ""), &[]);
        assert_eq!(stdout.trim(), "", "`{command}` must stay allowed");
    }
}

// ---------------------------------------------------------------------------
// ADR-0049 — a documents-only commit is permitted in a main checkout
// ---------------------------------------------------------------------------
//
// Every case here needs a REAL repository, because the whole rule is what git
// reports as staged. `main_checkout_fixture`'s fabricated `.git` directory
// reaches the unknown-index deny and would make each of these pass for the
// wrong reason.

/// A real git repository that classifies as a project main checkout, plus a
/// `stage` closure for putting paths in its index.
///
/// Returns `None` when `git init` fails, which is the only way this can be
/// unavailable; the caller skips rather than failing, matching how the rest of
/// this workspace treats a missing git.
fn real_main_checkout() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    let ok = Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(&repo)
        .status()
        .ok()?
        .success();
    ok.then_some((dir, repo))
}

/// Write `name` under `repo` and put it in the index.
fn stage(repo: &std::path::Path, name: &str) {
    let path = repo.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, "x").expect("write");
    assert!(
        Command::new("git")
            .args(["add", "--", name])
            .current_dir(repo)
            .status()
            .expect("git add")
            .success(),
        "git add {name}"
    );
}

/// A commit payload carrying the session id the writer query needs.
fn commit_payload(command: &str, cwd: &std::path::Path, extra_fields: &str) -> String {
    bash_payload_at(
        command,
        cwd,
        &format!(
            r#"{extra_fields}"session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_commit","#
        ),
    )
}

#[test]
fn pm_guard_allows_a_documents_only_commit_in_a_main_checkout() {
    // THE regression test (ADR-0049 decision 1). Against pre-fix code this
    // payload is DENIED: `evaluate_main_checkout_commit_command` matched the
    // verb `commit`, found a main checkout, and returned a reason with no
    // reference to what was staged. The allow below can only come from the
    // staged-set classifier.
    //
    // ADR-0044 decision 1 already made every one of these files WRITABLE here;
    // before this change none of them could be landed from where they were
    // written.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    for name in [
        "docs/adr/0049-x.md",
        "CLAUDE.md",
        "Cargo.toml",
        ".claude/settings.json",
        "TASK.md",
        "Makefile",
    ] {
        stage(&repo, name);
    }
    let stdout = run_pm_guard(&commit_payload("git commit -m 'docs: x'", &repo, ""), &[]);
    assert_eq!(
        stdout.trim(),
        "",
        "a documents-only commit in a main checkout must be allowed, got: {stdout}"
    );
}

#[test]
fn pm_guard_allows_a_dispatched_agents_documents_only_commit() {
    // The rule sits ahead of Guards 1 and 4, so both automatic subagent
    // populations reach it. They must reach the ALLOW too — an agent writing a
    // changelog fragment in the shared checkout is the case this exists for.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "crates/trusty-mpm/changelog.d/5782-x.md");
    for (extra_fields, env) in [
        (
            r#""agent_id":"agent-xyz789","agent_type":"documentation","#,
            vec![],
        ),
        ("", vec![("CLAUDE_MPM_SUB_AGENT", "1")]),
    ] {
        let stdout = run_pm_guard_at_with_env(
            &commit_payload("git commit -m 'docs: x'", &repo, extra_fields),
            "http://127.0.0.1:1/",
            &repo,
            &env,
        );
        assert_eq!(
            stdout.trim(),
            "",
            "a dispatched agent's documents-only commit must be allowed, got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_a_commit_whose_staged_set_contains_source() {
    // ADR-0049 decisions 1, 2 and 6. Source-only and MIXED are one deny, and
    // it names the source paths so the remedy is mechanical — naming the
    // documents too would send the reader unstaging the files that were fine.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/keep.md");
    stage(&repo, "crates/a/src/lib.rs");
    stage(&repo, "crates/a/src/other.py");

    let stdout = run_pm_guard(&commit_payload("git commit -m 'wip'", &repo, ""), &[]);
    assert_denied(&stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(reason.contains("crates/a/src/lib.rs"), "{reason}");
    assert!(reason.contains("crates/a/src/other.py"), "{reason}");
    assert!(
        !reason.contains("docs/keep.md"),
        "the deny must name only what made the set unsafe: {reason}"
    );
    assert!(reason.contains("git restore --staged"), "{reason}");
    assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
}

#[test]
fn pm_guard_denies_a_commit_the_staged_set_does_not_describe() {
    // ADR-0049 decision 5. Each of these commits content the index does not
    // hold, so a staged set of pure documents cannot license any of them.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/x.md");
    for command in [
        "git commit -a -m 'wip'",
        "git commit -am 'wip'",
        "git commit --amend --no-edit",
        "git commit --only docs/x.md -m 'wip'",
        "git commit -m 'wip' -- docs/x.md",
    ] {
        let stdout = run_pm_guard(&commit_payload(command, &repo, ""), &[]);
        assert_denied(&stdout);
        assert!(
            stdout.contains("ADR-0049"),
            "`{command}` must name the carve-out it missed: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_a_commit_with_nothing_staged() {
    // An empty index is not evidence of a documents commit — it is `git
    // commit` about to error, or about to be retried with `-a`. Deny is the
    // pre-ADR-0049 behaviour and stays.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    let stdout = run_pm_guard(&commit_payload("git commit -m 'wip'", &repo, ""), &[]);
    assert_denied(&stdout);
}

#[test]
fn pm_guard_denies_a_documents_only_commit_beside_a_live_writer() {
    // ADR-0049 decision 3, and the hazard the owner's ruling does not repeal:
    // a commit MOVES HEAD, so a documents commit lands on whichever branch the
    // other session's uncommitted work is sitting on, exactly as a source
    // commit would. Same directory-keyed query ADR-0048 decision 10 uses.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/x.md");
    // The CAPTURING mock (#5769 added it for exactly this), so the directory
    // KEY the query is made on is asserted rather than assumed. A query keyed
    // on a path no delegation record carries matches nothing and allows, with
    // the guard silently off — which is the defect #5769 was filed for.
    let (url, captured) = spawn_capturing_writers_mock(
        r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#,
    );
    let stdout = run_pm_guard_at(
        &commit_payload("git commit -m 'docs: x'", &repo, ""),
        &url,
        &repo,
    );

    assert_denied(&stdout);
    let posted = captured.posted().expect("the guard must query the daemon");
    assert_eq!(
        posted["cwd"],
        repo.display().to_string(),
        "the query must be keyed by the checkout root the recorder stamps"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(reason.contains("ADR-0049"), "{reason}");
    assert!(
        reason.contains("rust-engineer"),
        "the deny must name the writer it protected: {reason}"
    );
    assert!(
        reason.contains("unstaging it will not help"),
        "the content was never the problem and the text must say so: {reason}"
    );
    assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
}

#[test]
fn pm_guard_keys_a_subdirectory_commit_query_by_the_checkout_root() {
    // #5788 review, MEDIUM 4. `cd <subdir> && git commit` resolves the
    // subdirectory, but `tm hook` stamps a delegation record from its own
    // process directory — the two name one HEAD and need not be the same
    // string. The root is asked FIRST, which is the same order and the same
    // reason as the HEAD-move rule (#5769).
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/x.md");
    let sub = repo.join("docs");
    let (url, captured) = spawn_capturing_writers_mock(
        r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#,
    );
    let command = format!("cd {} && git commit -m 'docs: x'", sub.display());
    let stdout = run_pm_guard_at(&commit_payload(&command, &repo, ""), &url, &repo);

    assert_denied(&stdout);
    let posted = captured.posted().expect("the guard must query the daemon");
    assert_eq!(
        posted["cwd"],
        repo.display().to_string(),
        "a subdirectory commit must key the query by the checkout root"
    );
}

#[test]
fn pm_guard_allows_a_documents_only_commit_when_nobody_else_is_writing() {
    // The other side of decision 3, and the common case: a solo session sees
    // no friction at all. The mock answers an empty roster, which is also what
    // an unreachable daemon degrades to.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/x.md");
    let url = spawn_writers_mock(r#"{"agents":[],"total":0}"#);
    let stdout = run_pm_guard_at(
        &commit_payload("git commit -m 'docs: x'", &repo, ""),
        &url,
        &repo,
    );
    assert_eq!(
        stdout.trim(),
        "",
        "a documents commit with no other writer must be allowed, got: {stdout}"
    );
}

#[test]
fn pm_guard_denies_a_docs_commit_composed_with_another_command() {
    // #5788 review, CRITICAL 1, demonstrated live against the first cut. The
    // walker returned on the FIRST commit segment, so a docs-only staged set
    // licensed every later segment in the same Bash call:
    //
    //   git commit -m docs && git add -A && git commit -a -m src   → ALLOWED
    //   git commit -m docs ; git commit --amend --no-edit          → ALLOWED
    //
    // Both were denied before ADR-0049, and the second rewrites the shared
    // branch's tip. One index read describes at most one commit, and only when
    // nothing between the read and that commit can restage.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "docs/x.md");
    std::fs::create_dir_all(repo.join("crates/a/src")).expect("mkdir src");
    std::fs::write(repo.join("crates/a/src/lib.rs"), "y").expect("write source");
    for command in [
        "git commit -m docs && git add -A && git commit -a -m src",
        "git commit -m docs ; git commit --amend --no-edit",
        // One commit segment, but `git add` restages after the reading — the
        // same hole, one segment earlier.
        "git add -A && git commit -m docs",
        "git commit -m docs && cargo test",
    ] {
        let stdout = run_pm_guard(&commit_payload(command, &repo, ""), &[]);
        assert_denied(&stdout);
        assert!(
            stdout.contains("Split the call"),
            "`{command}` must name the composition as the cause: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_a_source_commit_read_from_a_subdirectory() {
    // #5788 review, CRITICAL 2, demonstrated live. `diff.relative=true` in any
    // config file makes `git diff --cached --name-only` under `-C <subdir>`
    // report only the paths beneath it, so a staged `.rs` outside the
    // subdirectory vanished and the gate read the set as documents-only.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    assert!(
        Command::new("git")
            .args(["config", "diff.relative", "true"])
            .current_dir(&repo)
            .status()
            .expect("git config")
            .success()
    );
    stage(&repo, "docs/x.md");
    stage(&repo, "crates/a/src/lib.rs");
    let sub = repo.join("docs");

    let stdout = run_pm_guard(
        &commit_payload(
            &format!("cd {} && git commit -m docs", sub.display()),
            &repo,
            "",
        ),
        &[],
    );
    assert_denied(&stdout);
    assert!(
        stdout.contains("crates/a/src/lib.rs"),
        "the source file outside the subdirectory must still be seen: {stdout}"
    );
}

#[test]
fn pm_guard_denies_a_commit_that_renames_source_into_a_document() {
    // #5788 review, HIGH 3, demonstrated live. Rename detection is on by
    // default and `--name-only` prints only the DESTINATION, so a staged
    // `git mv crates/a/src/lib.rs docs/lib.md` reported as documents-only for a
    // commit that deletes a source file from the shared branch. `git mv` is
    // gated by nothing else — it is not an edit tool and not a destructive verb.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    stage(&repo, "crates/a/src/lib.rs");
    assert!(
        Command::new("git")
            .args(["commit", "-qm", "init"])
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .status()
            .expect("git commit")
            .success()
    );
    std::fs::create_dir_all(repo.join("docs")).expect("mkdir docs");
    assert!(
        Command::new("git")
            .args(["mv", "crates/a/src/lib.rs", "docs/lib.md"])
            .current_dir(&repo)
            .status()
            .expect("git mv")
            .success()
    );

    let stdout = run_pm_guard(&commit_payload("git commit -m docs", &repo, ""), &[]);
    assert_denied(&stdout);
    assert!(
        stdout.contains("crates/a/src/lib.rs"),
        "the deleted source side of the rename must be named: {stdout}"
    );
}

#[test]
fn pm_guard_never_gates_git_add_in_a_main_checkout() {
    // ADR-0049 decision 4, checked against the code rather than assumed:
    // staging writes the index and moves no ref, so it creates none of the
    // shared-HEAD hazard the commit and HEAD-move rules exist for. Source
    // staged is still allowed — the gate is at the commit.
    let Some((_dir, repo)) = real_main_checkout() else {
        return;
    };
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    for command in [
        "git add -A",
        "git add -- crates/a/src/lib.rs",
        "git add .",
        "git stage docs/x.md",
    ] {
        let stdout = run_pm_guard_at(&commit_payload(command, &repo, ""), &url, &repo);
        assert_eq!(
            stdout.trim(),
            "",
            "`{command}` moves no ref and must stay allowed, got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_grants_a_worktree_to_a_writer_in_a_main_checkout() {
    // ADR-0048 Part A: the dispatch is REWRITTEN, not refused — the PM does not
    // have to re-issue anything, and the rewrite applies whether or not the
    // model reads a message. The whole original input must survive, because
    // `updatedInput` replaces the arguments rather than merging into them.
    let (_dir, repo) = main_checkout_fixture();
    let input = r#"{"subagent_type":"rust-engineer","prompt":"do the thing","description":"go"}"#;
    let stdout = run_pm_guard(&tool_payload_at("Agent", input, &repo, ""), &[]);

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let out = &value["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse");
    assert_eq!(out["updatedInput"]["isolation"], "worktree");
    assert_eq!(out["updatedInput"]["prompt"], "do the thing");
    assert_eq!(out["updatedInput"]["subagent_type"], "rust-engineer");
    assert!(
        out.get("permissionDecision").is_none(),
        "a grant changes the arguments and must not touch the permission flow: {stdout}"
    );
}

#[test]
fn pm_guard_keeps_an_opted_out_project_in_the_checkout() {
    // #5814: the project declares `agent_worktree = false`, so the same dispatch
    // that is granted a worktree above is left in the checkout and told the
    // workflow that replaces isolation. Read through the real binary because the
    // config read is a filesystem step the unit tests fake.
    let (_dir, repo) = main_checkout_fixture();
    std::fs::write(repo.join(".trusty-mpm.toml"), "agent_worktree = false\n")
        .expect("write project config");
    let input = r#"{"subagent_type":"documentation","prompt":"write the memo"}"#;
    let stdout = run_pm_guard(&tool_payload_at("Agent", input, &repo, ""), &[]);

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let updated = &value["hookSpecificOutput"]["updatedInput"];
    assert!(
        updated.get("isolation").is_none(),
        "an opted-out project must not be granted a worktree: {stdout}"
    );
    let prompt = updated["prompt"].as_str().expect("prompt survives");
    assert!(prompt.starts_with("write the memo"), "{stdout}");
    assert!(prompt.contains("agent_worktree = false"), "{stdout}");
    assert!(prompt.contains("work IN PLACE"), "{stdout}");
}

#[test]
fn pm_guard_still_grants_when_the_project_config_is_unreadable() {
    // #5814's default branch: a config that does not parse leaves ADR-0048's
    // grant exactly as it was. A project that predates the key behaves today
    // as it did before it existed — covered by the sibling above, which uses
    // the same fixture with no config file at all.
    let (_dir, repo) = main_checkout_fixture();
    std::fs::write(repo.join(".trusty-mpm.toml"), "agent_worktre = false\n")
        .expect("write project config");
    let input = r#"{"subagent_type":"rust-engineer","prompt":"do the thing"}"#;
    let stdout = run_pm_guard(&tool_payload_at("Agent", input, &repo, ""), &[]);

    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(
        value["hookSpecificOutput"]["updatedInput"]["isolation"], "worktree",
        "a config that cannot be parsed must never strip isolation: {stdout}"
    );
}

#[test]
fn pm_guard_denies_a_granted_dispatch_beside_a_live_writer() {
    // #5769 finding 2: the grant used to return BEFORE the #4480 concurrency
    // check, so that verdict stopped being computed for every dispatch made from
    // a main checkout. If the harness does not apply `updatedInput` — which this
    // binary cannot verify — a second unisolated writer that used to be denied
    // was simply admitted. The grant is now emitted only after the daemon says
    // the checkout is free.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"python-engineer","count":1}],"total":1}"#);
    let (_dir, repo) = main_checkout_fixture();
    let input = r#"{"subagent_type":"rust-engineer","prompt":"do the thing"}"#;
    let payload = tool_payload_at(
        "Agent",
        input,
        &repo,
        r#""session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_grant","#,
    );
    let stdout = run_pm_guard_at(&payload, &url, &repo);
    assert_denied(&stdout);
    assert!(
        stdout.contains("ADR-0048") && stdout.contains("python-engineer"),
        "the deny must name the rule and the sibling already in the tree: {stdout}"
    );
    // It must NOT be #4480's text, whose remedy is the isolation this guard had
    // already built and then declined to emit.
    assert!(
        !stdout.contains("Concurrent shared-worktree dispatch denied"),
        "the granted path needs its own reason, not the one it contradicts: {stdout}"
    );
}

#[test]
fn pm_guard_denies_a_granted_dispatch_when_the_daemon_does_not_answer() {
    // #5923, in the second place it lived. The grant path read a daemon that
    // accepted the connection and never answered as an empty checkout and
    // emitted the worktree grant. ADR-0048 puts the #4480 verdict in this same
    // call — "a dispatch made into a checkout another writer holds is denied,
    // and only an empty answer is granted" — and the grant it would emit is a
    // rewrite of the dispatch's arguments this binary cannot confirm the
    // harness applies. Against the pre-fix binary this prints the grant JSON
    // with `updatedInput`, which is the ALLOW this closes.
    let url = spawn_silent_mock();
    let (_dir, repo) = main_checkout_fixture();
    let payload = tool_payload_at(
        "Agent",
        r#"{"subagent_type":"rust-engineer","prompt":"do the thing"}"#,
        &repo,
        r#""session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_grant","#,
    );
    let stdout = run_pm_guard_at(&payload, &url, &repo);
    assert_denied(&stdout);
    assert!(
        stdout.contains("#5923") && stdout.contains("ADR-0048"),
        "the deny must name the rule it is enforcing and the ADR that puts it here: {stdout}"
    );
    assert!(
        !stdout.contains("updatedInput"),
        "a denied dispatch must not also be granted a worktree: {stdout}"
    );
}

#[test]
fn pm_guard_records_the_granted_isolation_it_emits() {
    // #5769 finding 1: the grant is worth nothing to the daemon unless the
    // daemon hears about it — the tracker records the ORIGINAL payload, so the
    // isolation has to be posted separately or every granted writer stays named
    // as writing in the shared checkout. Two things are asserted: the POST is
    // made at all, and it carries the `tool_use_id` the record is keyed by
    // (without it the upsert cannot find the tracker's record and would create a
    // second, unisolated one).
    let (url, captured) = spawn_capturing_writers_mock(r#"{"agents":[],"total":0}"#);
    let (_dir, repo) = main_checkout_fixture();
    let payload = tool_payload_at(
        "Agent",
        r#"{"subagent_type":"rust-engineer","prompt":"x"}"#,
        &repo,
        r#""session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_grant","#,
    );
    let stdout = run_pm_guard_at(&payload, &url, &repo);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(
        value["hookSpecificOutput"]["updatedInput"]["isolation"],
        "worktree"
    );
    let posted = captured.posted().expect("the grant must be posted");
    assert_eq!(
        posted["cwd"],
        repo.display().to_string(),
        "the grant must be recorded against the checkout it was granted in"
    );
    assert_eq!(
        posted["tool_use_id"], "toolu_grant",
        "without the correlation key the upsert cannot find the tracker's record: {posted}"
    );
    assert_eq!(
        posted["input"]["isolation"], "worktree",
        "the daemon must be told the isolation that was granted: {posted}"
    );
    // The fourth eligibility input, and the one nothing else would catch: if
    // `build_hook_payload` ever stopped stamping `tool`, the route classifies
    // the POST ineligible, records nothing, and the whole fix no-ops.
    assert_eq!(
        posted["tool"], "Agent",
        "the route re-derives eligibility from `tool`: {posted}"
    );
}

#[test]
fn pm_guard_warns_when_a_granted_worktree_is_not_recorded() {
    // #5769: an empty writer list is NOT proof the grant was recorded. A daemon
    // older than the granted-worktree route 404s it, a malformed body parses to
    // nothing — and both arrive as the same empty answer the free-checkout case
    // produces. Reading only the writer list made every one of those a silent
    // no-op that put the phantom straight back. The guard still GRANTS (failing
    // closed on version skew would block every dispatch from a main checkout),
    // so the warning is the whole observable difference.
    let (_dir, repo) = main_checkout_fixture();
    let payload = tool_payload_at(
        "Agent",
        r#"{"subagent_type":"rust-engineer","prompt":"x"}"#,
        &repo,
        r#""session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_grant","#,
    );

    // Answered, empty, and recorded nothing — the divergence.
    for body in [
        r#"{"agents":[],"total":0,"claimed":false}"#,
        // A daemon too old to send the field at all reads as not-recorded.
        r#"{"agents":[],"total":0}"#,
    ] {
        let url = spawn_writers_mock(body);
        let stderr = run_pm_guard_at_stderr(&payload, &url, &repo);
        assert!(
            stderr.contains("recorded nothing") && stderr.contains("#5769"),
            "an unrecorded grant must warn, got stderr: {stderr:?}"
        );
    }

    // The ordinary case: the daemon recorded it, and the guard stays silent.
    let url = spawn_writers_mock(r#"{"agents":[],"total":0,"claimed":true}"#);
    let stderr = run_pm_guard_at_stderr(&payload, &url, &repo);
    assert!(
        !stderr.contains("recorded nothing"),
        "a recorded grant must not warn, got stderr: {stderr:?}"
    );
}

#[test]
fn pm_guard_grants_a_worktree_to_an_unknown_agent_in_a_main_checkout() {
    // The deliberate divergence from #4480's fail-open: a custom or renamed
    // agent is indeterminate, and in a main checkout indeterminate resolves
    // toward isolation. This is the agent that kept writing to the shared tree.
    let (_dir, repo) = main_checkout_fixture();
    for input in [
        r#"{"subagent_type":"some-project-custom-agent","prompt":"x"}"#,
        r#"{"prompt":"an untyped dispatch"}"#,
    ] {
        let stdout = run_pm_guard(&tool_payload_at("Agent", input, &repo, ""), &[]);
        let value: serde_json::Value =
            serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
        assert_eq!(
            value["hookSpecificOutput"]["updatedInput"]["isolation"], "worktree",
            "{input} must be isolated rather than trusted"
        );
    }
}

#[test]
fn pm_guard_leaves_read_only_and_isolated_dispatches_alone() {
    // A worktree per read-only dispatch would re-open #3455's wasted-disk
    // complaint, and re-granting an already-isolated dispatch would let the
    // guard silently downgrade `remote` to `worktree`.
    let (_dir, repo) = main_checkout_fixture();
    for input in [
        r#"{"subagent_type":"research","prompt":"x"}"#,
        r#"{"subagent_type":"code-critic","prompt":"x"}"#,
        r#"{"subagent_type":"rust-engineer","isolation":"worktree","prompt":"x"}"#,
        r#"{"subagent_type":"rust-engineer","isolation":"remote","prompt":"x"}"#,
    ] {
        let stdout = run_pm_guard(&tool_payload_at("Agent", input, &repo, ""), &[]);
        assert_eq!(stdout.trim(), "", "{input} must pass through untouched");
    }
}

#[test]
fn pm_guard_does_not_grant_a_worktree_outside_a_main_checkout() {
    // Delegated work in a worktree is where work is supposed to happen. If this
    // fired there it would try to nest a worktree inside a worktree on every
    // dispatch — and it is the branch that keeps the rule from applying to the
    // whole machine.
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).expect("mkdir");
    std::fs::write(worktree.join(".git"), "gitdir: /elsewhere").expect("write .git");

    let input = r#"{"subagent_type":"rust-engineer","prompt":"x"}"#;
    let stdout = run_pm_guard(&tool_payload_at("Agent", input, &worktree, ""), &[]);
    assert_eq!(stdout.trim(), "", "a worktree dispatch must be untouched");
}

#[test]
fn pm_guard_denies_a_task_dispatch_it_cannot_isolate() {
    // `Task` carries no `isolation` parameter, so a rewrite would produce a
    // failed tool call rather than an isolated agent. This is the one case the
    // grant refuses, and the message has to name the tool that can do it.
    let (_dir, repo) = main_checkout_fixture();
    let input = r#"{"subagent_type":"rust-engineer","prompt":"x"}"#;
    let stdout = run_pm_guard(&tool_payload_at("Task", input, &repo, ""), &[]);
    assert_denied(&stdout);
    assert!(stdout.contains("Agent"), "{stdout}");
    assert!(stdout.contains("isolation"), "{stdout}");
}

#[test]
fn pm_guard_operator_escape_hatches_still_lift_the_write_boundary() {
    // The same contract every other rule in this file keeps: an operator who
    // lifts enforcement gets exactly that. Stated as a test because the write
    // boundary pierces the two AUTOMATIC subagent markers, and the distinction
    // between those and the two human escape hatches is the whole reason the
    // piercing is permitted at all.
    let (_dir, repo) = main_checkout_fixture();
    let target = repo.join("crates/trusty-mpm/src/lib.rs");
    let write = tool_payload_at(
        "Write",
        &format!(r#"{{"file_path":"{}","content":"x"}}"#, target.display()),
        &repo,
        "",
    );
    let commit = bash_payload_at("git commit -m 'wip'", &repo, "");
    for env in [
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
    ] {
        for payload in [&write, &commit] {
            let stdout = run_pm_guard(payload, &[env]);
            assert_eq!(
                stdout.trim(),
                "",
                "{} must lift this guard along with every other",
                env.0
            );
        }
    }
}

#[test]
fn pm_guard_denies_main_checkout_destructive_git_with_the_daemon_unreachable() {
    // The fail-open check this rule most had to get right. Every other
    // daemon-consulting guard in pm_guard answers ALLOW when the daemon is
    // down; this one consults no daemon at all, so an unreachable daemon
    // cannot weaken it. `run_pm_guard` always points `--url` at
    // http://127.0.0.1:1 (nothing listens there), which is exactly that
    // condition — the deny below is produced with no daemon in the picture,
    // and the best-effort audit POST failing changes nothing.
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard(&bash_payload_at("git clean -fdx", &repo, ""), &[]);
    assert_denied(&stdout);
}

#[test]
fn pm_guard_operator_escape_hatches_still_allow_main_checkout_destructive_git() {
    // `TRUSTY_MPM_DISABLE_HOOKS` and `TRUSTY_MPM_PM_UNRESTRICTED=1` are human
    // escape hatches, never set programmatically anywhere in this codebase.
    // They are deliberately NOT pierced — an operator who lifts enforcement
    // gets exactly that, the same contract the worktree-tmp guard keeps.
    let (_dir, repo) = main_checkout_fixture();
    let payload = bash_payload_at("git clean -fdx", &repo, "");
    for env in [
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
    ] {
        let stdout = run_pm_guard(&payload, &[env]);
        assert_eq!(
            stdout.trim(),
            "",
            "{} must lift this guard along with every other",
            env.0
        );
    }
}

// ---------------------------------------------------------------------------
// ADR-0048 decision 10 — HEAD-moving git in a shared main checkout
// ---------------------------------------------------------------------------

/// A Bash payload carrying the session id the daemon query needs.
///
/// Why: the HEAD-move rule decides with the daemon, and
/// `pm_guard_dispatch::live_shared_tree_writers` addresses a session's
/// delegations — an empty `session_id` fails open before anything is dialled.
/// Every other Bash rule in this file decides alone and so needs no id.
fn head_move_payload(command: &str, cwd: &std::path::Path, extra_fields: &str) -> String {
    bash_payload_at(
        command,
        cwd,
        &format!(
            r#"{extra_fields}"session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_pull","#
        ),
    )
}

#[test]
fn pm_guard_allows_a_pull_in_a_shared_main_checkout_beside_a_live_writer() {
    // THE regression test for ADR-0053. This exact payload — a `git pull` in a
    // main checkout the daemon reports another agent writing in — was DENIED
    // under ADR-0048 decision 10, and the test that asserted that deny is the
    // one this replaces. Owner ruling of 2026-08-17: "fetch and pull operations
    // are permitted. Only direct code editing is not."
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let (_dir, repo) = main_checkout_fixture();
    for command in ["git pull", "git pull --ff-only", "git pull --rebase"] {
        let stdout = run_pm_guard_at(&head_move_payload(command, &repo, ""), &url, &repo);
        assert_eq!(
            stdout.trim(),
            "",
            "`{command}` is permitted in a main checkout (ADR-0053), got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_denies_a_merge_in_a_shared_main_checkout() {
    // THE regression test (ADR-0048 decision 10). Against pre-fix code this
    // payload is ALLOWED with no output at all: `git merge` matched no verb
    // table in `pm_guard_bash`, the destructive rule stopped at
    // `reset --hard`/`clean -fdx`/`checkout -- <pathspec>`, and nothing routed
    // a Bash call through the writer query. The deny below can only come from
    // the new classifier plus that query. ADR-0053 narrowed decision 10 to
    // `merge` and `rebase`; this case is unchanged by it.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(
        &head_move_payload("git merge origin/main", &repo, ""),
        &url,
        &repo,
    );

    assert_denied(&stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("deny stdout must be valid JSON");
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason is a string");
    assert!(reason.contains("ADR-0048"), "{reason}");
    assert!(
        reason.contains("rust-engineer"),
        "the deny must name the writer it protected: {reason}"
    );
    assert!(
        reason.contains(&repo.display().to_string()),
        "the deny must name the directory: {reason}"
    );
    // ADR-0048 decision 6: a refusal with no remedy is retried worse. Both
    // remedies must be in the text — `fetch` for the common intent, a worktree
    // for the merge or rebase itself.
    assert!(reason.contains("git fetch"), "{reason}");
    assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
}

#[test]
fn pm_guard_denies_every_head_moving_verb_in_a_shared_main_checkout() {
    for command in [
        "git merge origin/main",
        "git merge --no-ff feature/x",
        "git rebase origin/main",
        "git rebase -i HEAD~3",
        // Composition must not hide the verb behind a benign first segment —
        // `git fetch && git merge` is the shape this actually arrives in, and
        // since ADR-0053 a permitted `git pull` is a benign first segment too.
        "git fetch origin && git merge origin/main",
        "git pull && git rebase origin/main",
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let (_dir, repo) = main_checkout_fixture();
        let stdout = run_pm_guard_at(&head_move_payload(command, &repo, ""), &url, &repo);
        assert_denied(&stdout);
    }
}

#[test]
fn pm_guard_denies_a_merge_from_a_dispatched_agent_in_a_shared_main_checkout() {
    // The load-bearing case, and why the rule sits ahead of Guards 1 and 4:
    // ADR-0048 decision 4 binds the PM AND every agent it dispatches, and both
    // automatic markers return ALLOW for exactly that population. Remove either
    // piercing and one of these two goes green-to-red.
    for (extra_fields, env) in [
        (
            r#""agent_id":"agent-xyz789","agent_type":"local-ops","#,
            vec![],
        ),
        ("", vec![("CLAUDE_MPM_SUB_AGENT", "1")]),
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let (_dir, repo) = main_checkout_fixture();
        let stdout = run_pm_guard_at_with_env(
            &head_move_payload("git merge origin/main", &repo, extra_fields),
            &url,
            &repo,
            &env,
        );
        assert_denied(&stdout);
        assert!(stdout.contains("ADR-0048"), "{stdout}");
    }
}

#[test]
fn pm_guard_allows_a_merge_in_a_main_checkout_nobody_else_is_writing_in() {
    // The false-positive boundary #5356 is the reminder for, and the reason the
    // rule asks the daemon at all instead of denying on the directory alone. A
    // solo session merging in its own checkout is ordinary work.
    let url = spawn_writers_mock(r#"{"agents":[],"total":0}"#);
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(
        &head_move_payload("git merge origin/main", &repo, ""),
        &url,
        &repo,
    );
    assert_eq!(
        stdout.trim(),
        "",
        "a merge with no other writer in the tree must be allowed, got: {stdout}"
    );
}

#[test]
fn pm_guard_allows_a_head_move_inside_a_worktree_beside_a_live_writer() {
    // A worktree's HEAD belongs to the one session that owns it, so a merge
    // there races nothing — this is where delegated work happens and it must
    // stay unrestricted. The mock is deliberately never consumed: the
    // classification returns before any daemon call.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let dir = tempfile::tempdir().expect("tempdir");
    let claude_wt = dir.path().join("repo/.claude/worktrees/wt-x");
    std::fs::create_dir_all(claude_wt.join(".git")).expect("mkdir worktree");
    let linked_wt = dir.path().join("linked-wt");
    std::fs::create_dir_all(&linked_wt).expect("mkdir linked");
    std::fs::write(linked_wt.join(".git"), "gitdir: /repo/.git/worktrees/x").expect("write .git");

    for cwd in [&claude_wt, &linked_wt] {
        for command in [
            "git pull",
            "git merge origin/main",
            "git rebase origin/main",
        ] {
            let stdout = run_pm_guard_at(&head_move_payload(command, cwd, ""), &url, cwd);
            assert_eq!(
                stdout.trim(),
                "",
                "`{command}` must be allowed in {}, got: {stdout}",
                cwd.display()
            );
        }
    }
}

#[test]
fn pm_guard_allows_fetch_and_in_progress_control_in_a_shared_main_checkout() {
    // `git fetch` is the remedy the deny offers and ADR-0048 decision 9 says it
    // needs no enforcement — it writes remote-tracking refs and never HEAD.
    // `git pull` joined it under ADR-0053, including through `cd` and `-C`,
    // which are the two overrides that used to carry a pull into the deny. The
    // `--abort`/`--continue` family resolves an operation that already started;
    // denying those would park a shared checkout mid-rebase and the deny would
    // carry no remedy that works from there.
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        "git fetch".to_string(),
        "git fetch --all --prune".to_string(),
        "git pull".to_string(),
        "git pull --ff-only".to_string(),
        format!("cd {} && git pull", repo.display()),
        format!("git -C {} pull --ff-only", repo.display()),
        "git rebase --abort".to_string(),
        "git rebase --continue".to_string(),
        "git merge --abort".to_string(),
        "git merge-base main HEAD".to_string(),
        "git status --short".to_string(),
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let stdout = run_pm_guard_at(&head_move_payload(&command, &repo, ""), &url, &repo);
        assert_eq!(
            stdout.trim(),
            "",
            "`{command}` must stay allowed even beside a live writer, got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_allows_a_merge_when_the_daemon_is_unreachable() {
    // Declared fail-open branch, and the one that differs from this module's
    // other main-checkout rules: those consult no daemon, so nothing can weaken
    // them. This one decides on the daemon's answer, so a down daemon must
    // answer "nobody else is here" rather than deny. `--url` points at
    // http://127.0.0.1:1, where nothing listens.
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(
        &head_move_payload("git merge origin/main", &repo, ""),
        "http://127.0.0.1:1",
        &repo,
    );
    assert_eq!(
        stdout.trim(),
        "",
        "an unreachable daemon must fail OPEN, got: {stdout}"
    );
}

#[test]
fn pm_guard_allows_a_merge_when_the_daemon_answer_is_malformed() {
    // Every unusable answer allows: a 5xx, a body that is not JSON, and
    // well-formed JSON of the wrong shape. A false deny here would land on
    // ordinary work with no way for the session to tell why.
    for (status_line, body) in [
        ("500 Internal Server Error", r#"{"agents":[{"agent":"x"}]}"#),
        ("200 OK", "not json at all"),
        ("200 OK", r#"{"agents":"rust-engineer"}"#),
        ("200 OK", r#"{"unexpected":"shape"}"#),
    ] {
        let url = spawn_writers_mock_with(status_line, body);
        let (_dir, repo) = main_checkout_fixture();
        let stdout = run_pm_guard_at(
            &head_move_payload("git merge origin/main", &repo, ""),
            &url,
            &repo,
        );
        assert_eq!(
            stdout.trim(),
            "",
            "a {status_line} / {body} answer must fail OPEN, got: {stdout}"
        );
    }
}

#[test]
fn head_move_query_asks_the_checkout_root_and_the_command_directory() {
    // #5769 finding 4: nothing pinned WHICH directory the guard sends, so the
    // query key and the recorder's key were free to disagree — and they did. A
    // delegation's `cwd` is stamped from `tm hook`'s own process directory, so
    // the key has to be a directory a record can carry: the checkout root for
    // every form, including one aimed at a subdirectory.
    let (_dir, repo) = main_checkout_fixture();
    let outside = tempfile::tempdir().expect("tempdir");
    let sub = repo.join("crates/trusty-mpm");

    for (command, run_from) in [
        ("git merge origin/main".to_string(), repo.clone()),
        (
            format!("cd {} && git merge origin/main", repo.display()),
            outside.path().to_path_buf(),
        ),
        (
            format!("git -C {} merge origin/main", repo.display()),
            outside.path().to_path_buf(),
        ),
        // The bypass this finding is named for: a subdirectory of the checkout
        // shares its HEAD, but keys a path no record was ever written at.
        (
            format!("cd {} && git merge origin/main", sub.display()),
            repo.clone(),
        ),
    ] {
        let command = command.as_str();
        let (url, captured) = spawn_capturing_writers_mock(
            r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#,
        );
        let stdout = run_pm_guard_at(&head_move_payload(command, &run_from, ""), &url, &run_from);
        assert_denied(&stdout);
        let posted = captured.posted().expect("the guard must query the daemon");
        assert_eq!(
            posted["cwd"],
            repo.display().to_string(),
            "`{command}` must key the query by the checkout root the recorder stamps"
        );
    }
}

#[test]
fn pm_guard_denies_a_head_move_carrying_an_in_progress_flag_as_a_value() {
    // #5769 finding 7: the in-progress carve-out scanned the whole argv tail, so
    // any command mentioning one of those strings exempted itself. `git merge -m
    // "--continue" origin/main` is a real merge with a commit message.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(
        &head_move_payload("git merge -m '--continue' origin/main", &repo, ""),
        &url,
        &repo,
    );
    assert_denied(&stdout);
}

#[test]
fn pm_guard_operator_escape_hatches_still_allow_a_head_move() {
    // The contract every rule in this file keeps: an operator who lifts
    // enforcement gets exactly that. The two AUTOMATIC subagent markers are
    // pierced (above); these two human escape hatches are not.
    for env in [
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let (_dir, repo) = main_checkout_fixture();
        let stdout = run_pm_guard_at_with_env(
            &head_move_payload("git merge origin/main", &repo, ""),
            &url,
            &repo,
            &[env],
        );
        assert_eq!(
            stdout.trim(),
            "",
            "{} must lift this guard along with every other",
            env.0
        );
    }
}
