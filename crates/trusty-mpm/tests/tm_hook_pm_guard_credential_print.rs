//! End-to-end proof that `tm hook --pm-guard` refuses a dispatched agent's
//! credential-printing command (#8596, #8248).
//!
//! Why: both incidents were dispatched agents (`local-ops`, `gcp-ops`), and a
//! rule reached only after the subagent exemptions would be a no-op for them.
//! The unit rows in `pm_guard_bash::credential_print` prove the policy; only
//! the real binary proves the rule fires for an agent payload.
//! What: spawns the built `tm` against an unreachable daemon URL and asserts
//! DENY (one JSON line carrying `permissionDecision: "deny"`) or ALLOW (empty
//! stdout). Every command uses a fake service name and is never executed.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_credential_print::`.

use crate::common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// Nothing listens on port 1, so a deny's best-effort audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// Run `tm hook --pm-guard` over one Bash command from `agent`; return stdout.
fn run_pm_guard(agent: &str, command: &str, cwd: &Path) -> String {
    let home = tempfile::tempdir().expect("home");
    common::write_disk_threshold(home.path(), 100);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "agent_id": "agent-8596",
        "agent_type": agent,
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = common::tm_command_in(home.path())
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .current_dir(cwd)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "tm hook --pm-guard must exit 0: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout
}

#[test]
fn pm_guard_refuses_an_agent_printing_a_credential() {
    let cwd = tempfile::tempdir().expect("cwd");
    for (agent, command) in [
        // #8596: the local-ops existence check that printed two values.
        (
            "local-ops",
            "security find-generic-password -s fake-svc -a fake-acct -w | head -c 50",
        ),
        // #8248: the gcp-ops "does it work" run.
        (
            "gcp-ops",
            "gcloud auth application-default print-access-token",
        ),
        // #8676: a value captured into a variable and printed a stage later.
        (
            "gcp-ops",
            "T=$(gcloud auth print-access-token); echo \"${T:0:10}\"",
        ),
        (
            "local-ops",
            "export K=$(security find-generic-password -s fake-svc -w) && printenv K",
        ),
        (
            "gcp-ops",
            "for t in $(gcloud auth print-access-token); do echo $t; done",
        ),
        // #8677: the three reproductions, and a sibling CLI.
        (
            "local-ops",
            "echo 'find-generic-password -s s -w' | security -i",
        ),
        (
            "gcp-ops",
            "curl -H \"Authorization: Bearer $(gcloud auth print-access-token)\" -v",
        ),
        (
            "local-ops",
            "security find-generic-password -s fake-svc -w > /dev/./tty",
        ),
        ("local-ops", "gh auth token"),
    ] {
        let stdout = run_pm_guard(agent, command, cwd.path());
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 1, "a deny prints one JSON line: {stdout:?}");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("deny is JSON");
        assert_eq!(
            parsed["hookSpecificOutput"]["permissionDecision"], "deny",
            "{command}: {stdout}"
        );
        assert!(stdout.contains("#8596"), "{command}: {stdout}");
    }
}

/// #8676 round 2, row 4: a word nested ~100k `${` deep once overflowed the
/// hook's stack, which aborts past `catch_unwind` and fails open.
#[test]
fn pm_guard_denies_a_deeply_nested_expansion_without_crashing() {
    let cwd = tempfile::tempdir().expect("cwd");
    let depth = 100_000;
    let command = format!(
        "T=$(gcloud auth print-access-token); echo {}T{}",
        "${a".repeat(depth),
        "}".repeat(depth)
    );
    let stdout = run_pm_guard("gcp-ops", &command, cwd.path());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("deny is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "{stdout}"
    );
}

#[test]
fn pm_guard_allows_an_agent_consuming_a_credential_without_printing_it() {
    let cwd = tempfile::tempdir().expect("cwd");
    for (agent, command) in [
        (
            "local-ops",
            "security find-generic-password -s fake-svc >/dev/null 2>&1 && echo present",
        ),
        (
            "gcp-ops",
            "curl -sS -H \"Authorization: Bearer $(gcloud auth print-access-token)\" https://example.test/v1",
        ),
        // #8676: a carried variable that is only measured or passed on.
        ("gcp-ops", "T=$(gcloud auth print-access-token); echo ${#T}"),
        (
            "gcp-ops",
            "T=$(gcloud auth print-access-token); python3 upload.py --token \"$T\"",
        ),
        // #8677: the nearest benign forms of the three reproductions.
        ("gcp-ops", "curl -v https://example.test"),
        ("local-ops", "security -h"),
        (
            "gcp-ops",
            "tmp=$(mktemp); gcloud auth print-access-token > \"$tmp\"",
        ),
    ] {
        let stdout = run_pm_guard(agent, command, cwd.path());
        assert!(stdout.is_empty(), "{command}: {stdout}");
    }
}
