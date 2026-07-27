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
fn pm_guard_raw_pm_unrestricted_env_no_longer_bypasses() {
    // Issue #3981: TRUSTY_MPM_PM_UNRESTRICTED is no longer read from live
    // process env — Guards 2/3 now resolve exclusively from the daemon's
    // held session record, keyed by TM_MANAGED_SESSION_ID (deliberately
    // removed by `run_pm_guard`'s env_remove list, so no daemon lookup is
    // even attempted here). Setting the raw env var directly on this
    // subprocess — exactly what a PM-writable `.claude/settings.json`
    // `env` block would do — must have NO EFFECT. Uses a build/test Bash
    // verb (NOT budget-eligible, an absolute single-call prohibition) so
    // this test's verdict cannot be muddied by the per-turn file-change
    // budget the way a bare Edit denial could be (issue #2918).
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"make build"}}"#,
        &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")],
    );
    assert_ne!(
        stdout.trim(),
        "",
        "a raw TRUSTY_MPM_PM_UNRESTRICTED env var must no longer bypass enforcement (#3981)"
    );
    assert_denied(&stdout);
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
fn pm_guard_raw_disable_hooks_env_no_longer_bypasses() {
    // Issue #3981: TRUSTY_MPM_DISABLE_HOOKS is no longer read from live
    // process env by pm_guard's Guards 2/3 (the general `tm hook` shim's
    // OWN separate DISABLE_HOOKS_ENV check in `commands::misc` is untouched
    // and out of scope — this test exercises `--pm-guard` specifically).
    // Setting the raw env var directly must have NO EFFECT. Uses a
    // build/test Bash verb (not budget-eligible — see the sibling
    // `pm_unrestricted` test's comment for why an Edit alone would be a
    // weaker proxy here).
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"pytest"}}"#,
        &[("TRUSTY_MPM_DISABLE_HOOKS", "1")],
    );
    assert_ne!(
        stdout.trim(),
        "",
        "a raw TRUSTY_MPM_DISABLE_HOOKS env var must no longer bypass pm_guard (#3981)"
    );
    assert_denied(&stdout);
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
fn pm_guard_raw_disable_hooks_env_no_longer_bypasses_worktree_guard() {
    // Issue #3981: since Guards 2/3 no longer read live process env at all, a
    // raw TRUSTY_MPM_DISABLE_HOOKS env var no longer lifts ANYTHING —
    // including the absolute worktree-tmp guard (#3977), which stays denied.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#,
        &[("TRUSTY_MPM_DISABLE_HOOKS", "1")],
    );
    assert_denied(&stdout);
}

#[test]
fn pm_guard_raw_pm_unrestricted_env_no_longer_bypasses_worktree_guard() {
    // Same as above for TRUSTY_MPM_PM_UNRESTRICTED (#3981).
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#,
        &[("TRUSTY_MPM_PM_UNRESTRICTED", "1")],
    );
    assert_denied(&stdout);
}

#[test]
fn pm_guard_denies_when_daemon_unreachable_even_with_managed_session_id() {
    // Verification target (issue #3981 fix): with a TM_MANAGED_SESSION_ID SET
    // (so guard_flags actually attempts the daemon round trip, unlike the
    // rest of this file's tests, which run with no session id and so
    // short-circuit before any network attempt) against the same
    // unreachable `http://127.0.0.1:1` every test in this file uses, the
    // guard must FAIL TOWARD ACTIVE. Uses a build/test Bash verb (not
    // budget-eligible) so the verdict cannot be muddied by the per-turn
    // file-change budget the way a bare Edit denial could be (#2918) — a
    // down/unreachable daemon must never be silently indistinguishable from
    // "still within this turn's file-change budget".
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm test"}}"#,
        &[(
            "TM_MANAGED_SESSION_ID",
            "11111111-1111-1111-1111-111111111111",
        )],
    );
    assert_denied(&stdout);
}

#[test]
fn pm_guard_worktree_guard_still_denies_with_no_env_bypass_available() {
    // Verification target (issue #3981 fix): the worktree-tmp guard (#3977)
    // is a DIFFERENT threat model and must remain untouched by this
    // redesign — proven here with no env vars at all, the ordinary case.
    let stdout = run_pm_guard(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git worktree add /tmp/wt-x"}}"#,
        &[],
    );
    assert_denied(&stdout);
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
