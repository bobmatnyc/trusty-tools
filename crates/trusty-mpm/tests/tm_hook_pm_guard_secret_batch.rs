//! End-to-end proof of the MPM-SEC-1 secret-guard batch through the real
//! `tm hook --pm-guard` binary (#8523, #8483, #7648, #7557).
//!
//! Why: each issue is an exposure path the #7266 secret-bearing-file guard
//! missed, and a rule the hook never reaches protects nothing. The unit rows
//! in `pm_guard_secret_read`, `pm_guard_secret_env_files`,
//! `pm_guard_secret_positions` and `pm_guard_bash::pod_env_dump` prove the
//! policy; only the real binary proves the rule is wired for an agent payload.
//! What: spawns the built `tm` against an unreachable daemon URL with a
//! `local-ops` payload and asserts DENY (one JSON line carrying
//! `permissionDecision: "deny"`) or ALLOW (empty stdout). Nothing is executed;
//! every fixture value is an obviously fake placeholder.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_secret_batch`.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// Nothing listens on port 1, so a deny's best-effort audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// A launchd plist whose `EnvironmentVariables` carries a credential-keyed
/// entry. The value is a placeholder, never a real-looking secret.
const PLIST_WITH_CREDENTIAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.example.fake</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/bin:/bin</string>
    <key>FAKE_SERVICE_TOKEN</key><string>placeholder-not-a-secret</string>
  </dict>
</dict>
</plist>
"#;

/// The same plist with only non-credential environment keys.
const PLIST_WITHOUT_CREDENTIAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.example.fake</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/bin:/bin</string>
    <key>HOME</key><string>/Users/example</string>
  </dict>
</dict>
</plist>
"#;

/// Run `tm hook --pm-guard` over one tool call from a `local-ops` agent.
fn run_pm_guard(tool_name: &str, tool_input: serde_json::Value, cwd: &Path) -> String {
    let home = tempfile::tempdir().expect("home");
    common::write_disk_threshold(home.path(), 100);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "agent_id": "agent-mpm-sec-1",
        "agent_type": "local-ops",
        "cwd": cwd.display().to_string(),
        "tool_name": tool_name,
        "tool_input": tool_input,
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

/// Run a Bash command through the hook.
fn run_bash(command: &str, cwd: &Path) -> String {
    run_pm_guard("Bash", serde_json::json!({ "command": command }), cwd)
}

/// Assert `stdout` is one deny line whose reason cites `issue`.
fn assert_denied_citing(stdout: &str, issue: &str, what: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "{what}: a deny prints one JSON line: {stdout:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("deny is JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "{what}: {stdout}"
    );
    assert!(stdout.contains(issue), "{what} must cite {issue}: {stdout}");
}

/// 🔴 REGRESSION (#8523): a pm2 dump holds every managed process's env.
#[test]
fn pm_guard_refuses_a_pm2_dump_read_8523() {
    let cwd = tempfile::tempdir().expect("cwd");
    for command in [
        "cat ~/.pm2/dump.pm2",
        "python3 -c 'import json; print(json.load(open(\"/Users/example/.pm2/dump.pm2\")))'",
        "jq . ~/.pm2/dump.pm2.bak",
    ] {
        assert_denied_citing(&run_bash(command, cwd.path()), "#7266", command);
    }
    let read = serde_json::json!({ "file_path": "/Users/example/.pm2/dump.pm2" });
    assert_denied_citing(
        &run_pm_guard("Read", read, cwd.path()),
        "#7266",
        "Read dump.pm2",
    );
}

/// 🔴 REGRESSION (#8523): a launchd plist whose `EnvironmentVariables` carries
/// a credential-keyed value is refused by Bash and by `Read`; a plist whose
/// environment holds only ordinary keys still reads.
#[test]
fn pm_guard_refuses_a_launchd_plist_carrying_a_credential_8523() {
    let cwd = tempfile::tempdir().expect("cwd");
    let agents = cwd.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    let secret = agents.join("com.example.fake.plist");
    let clean = agents.join("com.example.clean.plist");
    std::fs::write(&secret, PLIST_WITH_CREDENTIAL).expect("write fixture");
    std::fs::write(&clean, PLIST_WITHOUT_CREDENTIAL).expect("write fixture");

    let cat = format!("cat {}", secret.display());
    assert_denied_citing(&run_bash(&cat, cwd.path()), "#8523", &cat);
    let plutil = format!("plutil -p {}", secret.display());
    assert_denied_citing(&run_bash(&plutil, cwd.path()), "#8523", &plutil);
    // #8523 round 3: a Bash content search over the directory, not only `Grep`.
    for command in [
        format!("grep -r . {}", agents.display()),
        format!("rg . {}", agents.display()),
    ] {
        assert_denied_citing(&run_bash(&command, cwd.path()), "#8523", &command);
    }
    let read = serde_json::json!({ "file_path": secret.display().to_string() });
    assert_denied_citing(
        &run_pm_guard("Read", read, cwd.path()),
        "#8523",
        "Read plist",
    );

    // No false positive: a clean plist, and a listing of the directory.
    for command in [
        format!("cat {}", clean.display()),
        format!("ls -la {}", agents.display()),
    ] {
        let stdout = run_bash(&command, cwd.path());
        assert!(stdout.is_empty(), "{command} must allow: {stdout}");
    }
}

/// 🔴 REGRESSION (#8483): `Edit` and `Write` on a tfvars file are refused the
/// same way as a Bash read of it.
#[test]
fn pm_guard_refuses_an_edit_or_write_of_a_secret_file_8483() {
    let cwd = tempfile::tempdir().expect("cwd");
    let target = cwd.path().join("infra/prod.tfvars");
    let path = target.display().to_string();
    let bash = run_bash(&format!("cat {path}"), cwd.path());
    assert_denied_citing(&bash, "#7266", "cat prod.tfvars");
    let edit = serde_json::json!({
        "file_path": path,
        "old_string": "image = \"a\"",
        "new_string": "image = \"b\"",
    });
    assert_denied_citing(
        &run_pm_guard("Edit", edit, cwd.path()),
        "#7266",
        "Edit prod.tfvars",
    );
    let write = serde_json::json!({ "file_path": path, "content": "region = \"x\"\n" });
    assert_denied_citing(
        &run_pm_guard("Write", write, cwd.path()),
        "#7266",
        "Write prod.tfvars",
    );
    let multi = serde_json::json!({ "file_path": path, "edits": [] });
    assert_denied_citing(
        &run_pm_guard("MultiEdit", multi, cwd.path()),
        "#7266",
        "MultiEdit prod.tfvars",
    );

    // No false positive: an ordinary file, and a dotenv placeholder.
    for name in ["notes.md", ".env.example"] {
        let write = serde_json::json!({
            "file_path": cwd.path().join(name).display().to_string(),
            "content": "KEY=\n",
        });
        let stdout = run_pm_guard("Write", write, cwd.path());
        assert!(stdout.is_empty(), "Write {name} must allow: {stdout}");
    }
}

/// 🔴 REGRESSION (#7648): an unscoped env dump inside a pod is refused; a
/// named variable is not.
#[test]
fn pm_guard_refuses_an_unscoped_pod_env_dump_7648() {
    let cwd = tempfile::tempdir().expect("cwd");
    for command in [
        "kubectl exec my-pod -- env",
        "kubectl exec -n prod my-pod -c app -- env | grep -i LOG",
        "kubectl exec deploy/api -- printenv",
        "kubectl exec my-pod -- sh -c 'env | sort'",
        // #7648 round 3: an unlisted value option, and a container exec.
        "oc rsh --as adminuser mypod env",
        "docker exec -e FOO=1 c env",
        "podman container exec c cat /proc/1/environ",
    ] {
        assert_denied_citing(&run_bash(command, cwd.path()), "#7648", command);
    }
    for command in [
        "kubectl exec my-pod -- printenv LOG_LEVEL",
        "kubectl get pods -n prod",
        "kubectl exec my-pod -- ls /app",
        "oc rsh mypod ls",
        "docker exec c ls",
    ] {
        let stdout = run_bash(command, cwd.path());
        assert!(stdout.is_empty(), "{command} must allow: {stdout}");
    }
}

/// 🔴 REGRESSION (#7557): a branch-prefix exemption judged on spelling let a
/// traversal out of the prefix through. Structure decides now.
#[test]
fn pm_guard_refuses_a_traversal_past_a_branch_prefix_7557() {
    let cwd = tempfile::tempdir().expect("cwd");
    for command in [
        // A glob component that bash (<= 5.1) expands onto `..`.
        "for f in feat/.?/secrets/prod-credentials; do cat $f; done",
        "git branch docs/.*/.aws/aws-credentials",
        // A fragment cut out of a token that lives elsewhere.
        "for f in ../x=feat/db-credentials; do cat $f; done",
    ] {
        assert_denied_citing(&run_bash(command, cwd.path()), "#7266", command);
    }
    for command in [
        "for b in docs/secrets-integration-spec; do echo $b; done",
        "git push origin HEAD:feat/7527-tm-secrets-skill",
    ] {
        let stdout = run_bash(command, cwd.path());
        assert!(stdout.is_empty(), "{command} must allow: {stdout}");
    }
}
