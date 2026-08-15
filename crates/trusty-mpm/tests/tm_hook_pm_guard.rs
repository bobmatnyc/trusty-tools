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
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add .claude/worktrees/wt-x"}}"#,
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
    let payload = r#"{"hook_event_name":"PreToolUse","agent_id":"agent-xyz789","tool_name":"Bash","tool_input":{"command":"git worktree add .claude/worktrees/wt-x"}}"#;
    assert_eq!(
        run_pm_guard(payload, &[]).trim(),
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
    // list/remove/prune must never be blocked, and ordinary temp usage
    // (mktemp, temp-file writes, cargo build artifacts) must keep working.
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
    // The load-bearing arm: the PM carries neither marker, so its dispatches
    // must pass silently. A regression here halts every delegation in the
    // system, which is strictly worse than the fan-out this guard prevents.
    for tool in ["Agent", "Task"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{{"subagent_type":"rust-engineer","prompt":"go"}}}}"#
        );
        assert_eq!(
            run_pm_guard(&payload, &[]).trim(),
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
    for payload in [
        r#"{"hook_event_name":"PreToolUse","agent_id":"","tool_name":"Agent","tool_input":{"prompt":"go"}}"#,
        r#"{"hook_event_name":"PreToolUse","agent_type":"rust-engineer","tool_name":"Agent","tool_input":{"prompt":"go"}}"#,
    ] {
        assert_eq!(
            run_pm_guard(payload, &[]).trim(),
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
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut command = Command::new(bin);
    command
        .args(["--url", url, "hook", "--pm-guard"])
        .current_dir(cwd)
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
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "tm hook --pm-guard must always exit 0 (fail-open): stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
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
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    url
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

#[test]
fn pm_guard_allows_when_the_daemon_answer_is_malformed() {
    // Declared fail-open branch: every unusable answer — a 5xx, a body that is
    // not JSON, and well-formed JSON of the wrong shape — must ALLOW. A false
    // deny here lands on the PM and halts every dispatch in the system.
    let cases: [(&'static str, &'static str); 4] = [
        (
            "500 Internal Server Error",
            r#"{"agents":[{"agent":"rust-engineer"}]}"#,
        ),
        ("200 OK", "not json at all"),
        ("200 OK", r#"{"agents":"rust-engineer"}"#),
        ("200 OK", r#"{"unexpected":"shape"}"#),
    ];
    for (status_line, body) in cases {
        let url = spawn_writers_mock_with(status_line, body);
        let cwd = tempfile::tempdir().expect("tempdir");
        let payload = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_bad","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","prompt":"go"}}"#;
        assert_eq!(
            run_pm_guard_at(payload, &url, cwd.path()).trim(),
            "",
            "a {status_line} / {body} answer must fail OPEN"
        );
    }
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
    let (url, _dir) = serve_delegation_router_behind_a_barrier(2);
    let cwd = tempfile::tempdir().expect("tempdir");

    let mut children = Vec::new();
    for tool_use_id in ["toolu_race_a", "toolu_race_b"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"{tool_use_id}","tool_name":"Agent","tool_input":{{"subagent_type":"rust-engineer","prompt":"go"}}}}"#
        );
        let url = url.clone();
        let dir = cwd.path().to_path_buf();
        children.push(std::thread::spawn(move || {
            run_pm_guard_at(&payload, &url, &dir)
        }));
    }

    let verdicts: Vec<String> = children
        .into_iter()
        .map(|h| h.join().expect("guard thread").trim().to_string())
        .collect();

    let allowed = verdicts.iter().filter(|v| v.is_empty()).count();
    let denied: Vec<&String> = verdicts.iter().filter(|v| !v.is_empty()).collect();
    assert_eq!(
        allowed, 1,
        "exactly one of two simultaneous dispatches may be admitted, got: {verdicts:?}"
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
fn pm_guard_denies_a_pull_in_a_shared_main_checkout() {
    // THE regression test (ADR-0048 decision 10). Against pre-fix code this
    // payload is ALLOWED with no output at all: `git pull` matched no verb
    // table in `pm_guard_bash`, the destructive rule stopped at
    // `reset --hard`/`clean -fdx`/`checkout -- <pathspec>`, and nothing routed
    // a Bash call through the writer query. The deny below can only come from
    // the new classifier plus that query.
    let url = spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(&head_move_payload("git pull", &repo, ""), &url, &repo);

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
        "git pull --rebase",
        "git merge origin/main",
        "git rebase origin/main",
        // Composition must not hide the verb behind a benign first segment —
        // `git fetch && git pull` is the shape this actually arrives in.
        "git fetch origin && git pull",
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let (_dir, repo) = main_checkout_fixture();
        let stdout = run_pm_guard_at(&head_move_payload(command, &repo, ""), &url, &repo);
        assert_denied(&stdout);
    }
}

#[test]
fn pm_guard_denies_a_pull_from_a_dispatched_agent_in_a_shared_main_checkout() {
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
            &head_move_payload("git pull", &repo, extra_fields),
            &url,
            &repo,
            &env,
        );
        assert_denied(&stdout);
        assert!(stdout.contains("ADR-0048"), "{stdout}");
    }
}

#[test]
fn pm_guard_allows_a_pull_in_a_main_checkout_nobody_else_is_writing_in() {
    // The false-positive boundary #5356 is the reminder for, and the reason the
    // rule asks the daemon at all instead of denying on the directory alone. A
    // solo session updating its own checkout is ordinary work.
    let url = spawn_writers_mock(r#"{"agents":[],"total":0}"#);
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(&head_move_payload("git pull", &repo, ""), &url, &repo);
    assert_eq!(
        stdout.trim(),
        "",
        "a pull with no other writer in the tree must be allowed, got: {stdout}"
    );
}

#[test]
fn pm_guard_allows_a_pull_inside_a_worktree_beside_a_live_writer() {
    // A worktree's HEAD belongs to the one session that owns it, so a pull
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
    // needs no enforcement — it writes remote-tracking refs and never HEAD. The
    // `--abort`/`--continue` family resolves an operation that already started;
    // denying those would park a shared checkout mid-rebase and the deny would
    // carry no remedy that works from there.
    let (_dir, repo) = main_checkout_fixture();
    for command in [
        "git fetch",
        "git fetch --all --prune",
        "git rebase --abort",
        "git rebase --continue",
        "git merge --abort",
        "git merge-base main HEAD",
        "git status --short",
    ] {
        let url =
            spawn_writers_mock(r#"{"agents":[{"agent":"rust-engineer","count":1}],"total":1}"#);
        let stdout = run_pm_guard_at(&head_move_payload(command, &repo, ""), &url, &repo);
        assert_eq!(
            stdout.trim(),
            "",
            "`{command}` must stay allowed even beside a live writer, got: {stdout}"
        );
    }
}

#[test]
fn pm_guard_allows_a_pull_when_the_daemon_is_unreachable() {
    // Declared fail-open branch, and the one that differs from this module's
    // other main-checkout rules: those consult no daemon, so nothing can weaken
    // them. This one decides on the daemon's answer, so a down daemon must
    // answer "nobody else is here" rather than deny. `--url` points at
    // http://127.0.0.1:1, where nothing listens.
    let (_dir, repo) = main_checkout_fixture();
    let stdout = run_pm_guard_at(
        &head_move_payload("git pull", &repo, ""),
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
fn pm_guard_allows_a_pull_when_the_daemon_answer_is_malformed() {
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
        let stdout = run_pm_guard_at(&head_move_payload("git pull", &repo, ""), &url, &repo);
        assert_eq!(
            stdout.trim(),
            "",
            "a {status_line} / {body} answer must fail OPEN, got: {stdout}"
        );
    }
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
            &head_move_payload("git pull", &repo, ""),
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
