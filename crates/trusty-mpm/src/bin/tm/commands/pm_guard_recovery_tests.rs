//! Tests for the pm-guard recovery allowlist (#10641).

use serde_json::json;
use trusty_mpm::core::guard_policy;

use super::*;

fn call(tool: &str, input: Value) -> Value {
    json!({ "session_id": "s-1", "cwd": "/repo", "tool_name": tool, "tool_input": input })
}

fn bash(command: &str) -> Value {
    call("Bash", json!({ "command": command }))
}

#[test]
fn only_plain_recovery_commands_qualify() {
    for command in [
        "tm doctor",
        "tm install",
        "tm install --force",
        "tm install --reset-agents engineer,qa",
        "cargo install trusty-mpm",
        "cargo install trusty-mpm --version 0.36.4 --locked",
        "cargo install trusty-mpm --version=0.36.4 --locked --force",
        "  tm  doctor  ",
    ] {
        assert!(is_recovery_command(command), "{command}");
    }
    for command in [
        "",
        "tm",
        "tm doctor --fix",
        "tm session list",
        "cargo install other-crate",
        "cargo install trusty-mpm --path .",
        "cargo install trusty-mpm --git https://x.example/r",
        "cargo install trusty-mpm --root /tmp/r",
        "cargo install trusty-mpm --version",
        "/usr/local/bin/tm doctor",
    ] {
        assert!(!is_recovery_command(command), "{command}");
    }
}

#[test]
fn laundering_shapes_fall_back_to_deny() {
    // Ruling 263: no metacharacter, env prefix or wrapper word carries a command.
    for command in [
        "tm doctor; rm -rf x",
        "tm doctor && curl https://x.example",
        "tm doctor || curl https://x.example",
        "tm doctor | sh",
        "tm install $(curl https://x.example)",
        "tm install `id`",
        "tm doctor > /etc/hosts",
        "tm doctor < /dev/null",
        "tm doctor &",
        "tm doctor\nrm -rf x",
        "tm\tdoctor",
        "FOO=1 tm doctor",
        "timeout 5 cargo install trusty-mpm",
        "env tm doctor",
        "sudo tm install",
        "tm install 'x'",
        "tm install \"x\"",
        "tm install ~/x",
        "tm install *",
    ] {
        assert!(!is_recovery_command(command), "{command:?}");
        assert!(!recovery_allows(&bash(command)), "{command:?}");
    }
}

#[test]
fn read_only_tools_pass_unless_they_name_a_secret() {
    for tool in READ_ONLY_TOOLS {
        let input = json!({ "file_path": "/repo/README.md", "pattern": "x", "path": "/repo" });
        assert!(recovery_allows(&call(tool, input)), "{tool}");
    }
    assert!(!recovery_allows(&call(
        "Read",
        json!({ "file_path": "/repo/.env" })
    )));
    assert!(!recovery_allows(&call(
        "Read",
        json!({ "file_path": "/repo/infra/terraform.tfvars" })
    )));
}

#[test]
fn writes_never_qualify() {
    for (tool, input) in [
        (
            "Write",
            json!({ "file_path": "/repo/a.md", "content": "x" }),
        ),
        ("Edit", json!({ "file_path": "/repo/a.md" })),
        ("MultiEdit", json!({ "file_path": "/repo/a.md" })),
        ("NotebookEdit", json!({ "notebook_path": "/repo/a.ipynb" })),
        ("Agent", json!({ "subagent_type": "qa", "prompt": "go" })),
        ("Bash", json!({ "command": "rm -rf x" })),
        ("Bash", json!({})),
        ("mcp__slack__send", json!({})),
    ] {
        assert!(!recovery_allows(&call(tool, input)), "{tool}");
    }
    assert!(
        !recovery_allows(&json!({ "tool_input": {} })),
        "no tool name"
    );
}

#[test]
fn a_failure_denies_a_write_and_allows_a_recovery_command() {
    let policy = &guard_policy::GUARD_FAILURE;
    let write = call(
        "Write",
        json!({ "file_path": "/repo/a.md", "content": "x" }),
    );
    let deny = failure_verdict(policy, "it panicked", Some(&write)).expect("a write is refused");
    assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
    let reason = deny["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("it panicked"), "{reason}");
    assert!(
        reason.contains(RECOVERY_NOTE),
        "the recovery note: {reason}"
    );
    assert!(reason.ends_with(&policy.tag()), "{reason}");
    assert_eq!(
        failure_verdict(policy, "it panicked", Some(&bash("tm doctor"))),
        None
    );
    // A payload the guard never read gets no allowlist.
    assert!(failure_verdict(policy, "it panicked", None).is_some());
}
