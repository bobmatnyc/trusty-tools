//! Integration test for `tm hook`'s `PreToolUse` Bash command-rewrite spike
//! (issue #1956, Option 0).
//!
//! Why: `commands::hook_rewrite`'s unit tests cover the rewrite/exclusion
//! decision logic in isolation; `commands::misc::hook`'s own tests cover the
//! sub-agent/disable-hooks guard branches. Neither exercises the actual
//! stdin-read → rewrite → stdout-print path end to end through the real
//! binary, which is the behavior Claude Code will actually depend on. This
//! file closes that gap.
//! What: Runs the built `tm` binary (via `common::tm_command`) as
//! `tm --url http://127.0.0.1:1 hook` with `CLAUDE_HOOK_EVENT=PreToolUse` and
//! a `{"tool_name":"Bash","tool_input":{"command":"..."}}` stdin payload,
//! then asserts the printed `hookSpecificOutput.updatedInput.command` matches
//! the expected rewrite (or that nothing is printed for excluded commands).
//! The daemon URL is pointed at an unreachable address so the best-effort
//! POST fails fast without a real daemon running.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pretooluse_rewrite::`.
//!
//! Note: the `hookSpecificOutput`/`updatedInput` JSON shape asserted against
//! below, and the stdin field names consumed (`hook_event_name`,
//! `tool_name`, `tool_input.command`), are confirmed against the live Claude
//! Code hooks reference (<https://code.claude.com/docs/en/hooks>, confirmed
//! 2026-07-03) — see `commands::hook_rewrite::build_pretooluse_rewrite_response`'s
//! doc comment for the citation. This module previously carried a "not
//! validated against a live Claude Code instance" caveat; that has been
//! resolved, though these tests still only prove our own code is internally
//! consistent with the documented protocol, not a substitute for testing
//! against a real running Claude Code session.

use crate::common;

use std::io::Write;

/// What `tm hook` rewrites `cargo test` into.
///
/// #7384 put the command inside a brace group whose trailing `printf` reports
/// its exit status, because a pipeline hands its filter stdout and nothing
/// else. `commands::compress::wrap_command_reporting_exit` is the producer and
/// `split_exit_sentinel` the consumer; this file is outside the binary, so the
/// spelling is repeated here — the unit tests build theirs from the producer,
/// which is what keeps the two from drifting apart unnoticed.
const EXPECTED_CARGO_TEST_REWRITE: &str = "{ cargo test; printf '\\n__tm_compress_exit=%s__\\n' \"$?\"; } | tm compress --tool \"cargo test\"";

/// #8261: `cargo test` is a heavy build, so `tm hook` also leases it — the
/// same `<hook binary> build-lease -- ` insertion `tm hook --pm-guard` emits,
/// inside the compression wrapper. Returns the command with that one insertion
/// removed, for comparison with [`EXPECTED_CARGO_TEST_REWRITE`], and panics if
/// the insertion is missing.
fn without_the_lease_insertion(command: &serde_json::Value) -> String {
    let command = command.as_str().expect("the rewritten command is a string");
    let Some(rest) = command.strip_prefix("{ ") else {
        panic!("the compression wrapper comes first: {command}");
    };
    let Some((_program, build)) = rest.split_once(" build-lease --wait-secs 60 -- ") else {
        panic!("a heavy build must be leased: {command}");
    };
    format!("{{ {build}")
}

use std::process::Stdio;

/// Spawn `tm hook` with the given `CLAUDE_HOOK_EVENT` and stdin JSON,
/// returning stdout as a string.
///
/// The command comes from `common::tm_command`, so the child runs against a
/// scratch `$HOME` (#7568).
fn run_hook_with_stdin(event: &str, stdin_json: &str) -> String {
    let mut child = common::tm_command()
        .args(["--url", "http://127.0.0.1:1", "hook"])
        // #7477: a payload without `cwd` falls back to the hook's own cwd, and
        // cargo runs this target from an agent's isolation worktree, where the
        // hook rightly never rewrites. Pin it outside any worktree.
        .current_dir(common::tm_spawn_home())
        .env("CLAUDE_HOOK_EVENT", event)
        .env("CLAUDE_SESSION_ID", "test-session")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm hook");
    assert!(
        output.status.success(),
        "tm hook exited non-zero: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

#[test]
fn hook_rewrites_plain_bash_command_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON when rewriting");
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(
        without_the_lease_insertion(&parsed["hookSpecificOutput"]["updatedInput"]["command"]),
        EXPECTED_CARGO_TEST_REWRITE
    );
}

#[test]
fn hook_never_emits_the_refused_shape_in_an_isolation_worktree() {
    // #7477: the harness classifier refuses the brace-group pipeline inside an
    // isolation worktree. #10973 compresses there again, but only as one simple
    // command. The payload `cwd` is what Claude Code reports.
    let cwd = "/repo/.claude/worktrees/agent-a/crates/trusty-mpm";
    for command in ["git diff", "ls -la"] {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": cwd,
        });
        let stdout = run_hook_with_stdin("PreToolUse", &payload.to_string());
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one object");
        let rewritten = parsed["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            rewritten.ends_with(&format!(" -- {command}")),
            "{rewritten}"
        );
        for refused in ["{", "|", ";", "printf", "$?"] {
            assert!(!rewritten.contains(refused), "{refused} in {rewritten}");
        }
    }
    // #8261: a heavy build there is still leased — the builds run in isolation
    // worktrees — but never compression-wrapped (#7477).
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
        "cwd": cwd,
    });
    let stdout = run_hook_with_stdin("PreToolUse", &payload.to_string());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one JSON object");
    let command = parsed["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap_or_default();
    assert!(
        command.ends_with(" build-lease --wait-secs 60 -- cargo test"),
        "{command}"
    );
    assert!(!command.contains("tm compress"), "{command}");
}

/// #10973: delegated agents run almost entirely inside isolation worktrees, so
/// a hook that never compresses there recorded almost no `compress` rows.
#[test]
fn hook_compresses_a_filter_covered_command_inside_an_isolation_worktree() {
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "git diff" },
        "cwd": "/repo/.claude/worktrees/agent-a/crates/trusty-mpm",
    });
    let stdout = run_hook_with_stdin("PreToolUse", &payload.to_string());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("a worktree call must be rewritten ({e}): {stdout:?}"));
    let command = parsed["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap_or_default();
    assert!(
        command.contains("tm compress --tool \"git diff\""),
        "{command}"
    );
}

#[test]
fn hook_stays_silent_for_orchestrator_command_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"make build"}}"#,
    );
    assert_eq!(
        stdout.trim(),
        "",
        "excluded orchestrator command must produce no stdout output, got: {stdout}"
    );
}

#[test]
fn hook_stays_silent_for_non_bash_tool_on_pretooluse() {
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}"#,
    );
    assert_eq!(stdout.trim(), "");
}

#[test]
fn hook_stays_silent_without_stdin_payload() {
    let stdout = run_hook_with_stdin("PreToolUse", "");
    assert_eq!(stdout.trim(), "");
}

#[test]
fn hook_rewrite_stdout_contains_only_the_json_object() {
    // Per the live Claude Code hooks protocol, "your hook's stdout must
    // contain only the JSON object" for a successful (exit 0) response —
    // this is a direct regression guard for the early `return Ok(())` after
    // printing in `misc::hook` (trusty-review finding, PR #1968): before the
    // fix, execution fell through to build and (best-effort) send an
    // observability POST after already printing to stdout for this branch.
    let stdout = run_hook_with_stdin(
        "PreToolUse",
        r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one line of stdout (the JSON object), got: {stdout:?}"
    );
    serde_json::from_str::<serde_json::Value>(lines[0])
        .expect("the single stdout line must be valid JSON");
}

/// Spawn `tm hook` WITHOUT `CLAUDE_HOOK_EVENT`/`CLAUDE_SESSION_ID` set at
/// all, relying purely on the stdin JSON's `hook_event_name`/`session_id`
/// fields — this is what a real Claude Code invocation looks like per the
/// live hooks reference (confirmed 2026-07-03): Claude Code does not set
/// those as environment variables for hook subprocesses.
fn run_hook_with_stdin_only(stdin_json: &str) -> String {
    let mut child = common::tm_command()
        .args(["--url", "http://127.0.0.1:1", "hook"])
        // #7477: a payload without `cwd` falls back to the hook's own cwd, and
        // cargo runs this target from an agent's isolation worktree, where the
        // hook rightly never rewrites. Pin it outside any worktree.
        .current_dir(common::tm_spawn_home())
        .env_remove("CLAUDE_HOOK_EVENT")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm hook`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm hook");
    assert!(
        output.status.success(),
        "tm hook exited non-zero: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

#[test]
fn hook_rewrites_using_stdin_hook_event_name_without_env_var() {
    // Regression test for a trusty-review-flagged bug (PR #1968): Claude
    // Code never sets `CLAUDE_HOOK_EVENT` as an environment variable for
    // hook subprocesses — `hook_event_name` is only ever delivered via the
    // stdin JSON payload. Before the fix, `event` always resolved to the
    // `"Unknown"` placeholder in this scenario and the entire PreToolUse
    // rewrite spike silently never fired against a real Claude Code
    // instance.
    let stdout = run_hook_with_stdin_only(
        r#"{"hook_event_name":"PreToolUse","session_id":"abc-123","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON when rewriting");
    assert_eq!(
        without_the_lease_insertion(&parsed["hookSpecificOutput"]["updatedInput"]["command"]),
        EXPECTED_CARGO_TEST_REWRITE
    );
}

/// Run `script` under `/bin/sh` in `cwd` with the real `tm` on the filter
/// stage and a scratch `$HOME`: (exit code, stdout, stderr).
fn run_sh_in(cwd: &std::path::Path, script: &str) -> (Option<i32>, String, String) {
    let mut shell = std::process::Command::new("/bin/sh");
    common::isolate_spawned_tm(&mut shell, common::tm_spawn_home());
    let out = shell
        .args(["-c", script])
        .current_dir(cwd)
        .env(trusty_agents_common::compress::ENV_COMPRESS_NO_RTK, "1")
        .output()
        .expect("run /bin/sh");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// D60 (Refs #10973): `cd <dir> && <cmd>` is rewritten with the `cd` kept in
/// front, and the rewrite, run through the real `tm compress`, behaves like the
/// original: same output from inside `<dir>`, and a missing `<dir>` fails with
/// the same status and message. On main `tm hook` printed nothing for it.
#[test]
fn hook_rewrites_a_cd_prefixed_command_and_it_runs_like_the_original() {
    let root = tempfile::tempdir().expect("temp dir");
    let root = root.path().canonicalize().expect("canonical temp dir");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::write(root.join("sub/marker.txt"), "alpha\nhi there\n").expect("write");
    let bin = common::tm_bin();
    let rewrite = |command: &str| {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": root.display().to_string(),
        });
        let stdout = run_hook_with_stdin("PreToolUse", &payload.to_string());
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("`{command}` must be rewritten ({e}): {stdout:?}"));
        let rewritten = parsed["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .expect("a rewritten command")
            .to_string();
        let prefix = command.split_once("&& ").expect("a cd form").0;
        assert!(
            rewritten.starts_with(&format!("{prefix}&& {{ ")),
            "the `cd` stays in front of the group: {rewritten}"
        );
        rewritten.replace("| tm compress", &format!("| '{bin}' compress"))
    };

    let ok = "cd sub && grep -n hi marker.txt";
    let (code, out, err) = run_sh_in(&root, ok);
    let (rcode, rout, rerr) = run_sh_in(&root, &rewrite(ok));
    assert_eq!((code, out.as_str()), (Some(0), "2:hi there\n"), "{err}");
    assert_eq!(
        (rcode, rout.as_str()),
        (Some(0), "2:hi there\n"),
        "the rewrite runs in <dir>: {rerr}"
    );

    let missing = "cd missing && grep -n hi marker.txt";
    let (code, out, err) = run_sh_in(&root, missing);
    let (rcode, rout, rerr) = run_sh_in(&root, &rewrite(missing));
    assert_ne!(code, Some(0), "a missing <dir> fails: {err}");
    assert_eq!((code, out, err), (rcode, rout, rerr), "same cd failure");
}

/// D60 (Refs #10973): any chain other than one leading `cd <dir> && <cmd>`
/// passes through the binary unchanged, so `tm hook` prints nothing.
#[test]
fn hook_stays_silent_for_a_cd_chain_it_does_not_accept() {
    for command in ["cd a && cd b && git diff", "cd a && git diff\nls -la"] {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        let stdout = run_hook_with_stdin("PreToolUse", &payload.to_string());
        assert_eq!(stdout.trim(), "", "{command:?} must not be rewritten");
    }
}
