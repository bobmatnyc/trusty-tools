//! End-to-end proof of the #9001 exact-target floor's error arm.
//!
//! Why: owner ruling 2026-10-01 — pm-guard refuses a tmux target it cannot
//! resolve exactly, and fails closed when it cannot ask tmux at all. Only the
//! built binary proves the refusal holds under each bypass.
//! What: `tm hook --pm-guard` runs as a PM with a scratch `$HOME` and no
//! `TRUSTY_MPM_ALLOW_HOST_STATE`, so the guard's tmux listing is refused
//! (#5784) and no tmux server is touched. A `send-keys` with a target denies
//! naming it; a read verb and a command with no tmux pass.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_tmux_target_9001::`.

use crate::common;

use std::io::Write;
use std::process::Stdio;

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// The guard's stdout for a PM-stamped Bash `command` under `env`.
fn guard(command: &str, env: &[(&str, &str)]) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
    std::fs::create_dir_all(&project).expect("mkdir project");
    common::write_disk_threshold(&home, 100);
    let mut cmd = common::tm_command_in(&home);
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&project)
        .env("CLAUDE_PROJECT_DIR", &project)
        .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
        .env_remove("TRUSTY_MPM_ALLOW_HOST_STATE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": project.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// Fail closed: with the tmux listing refused, a `send-keys` target cannot be
/// checked, so it is refused under each bypass, naming the target.
#[test]
fn an_unlistable_tmux_target_is_refused_under_each_bypass() {
    let send = "tmux send-keys -t =nosuch9001:0 'hi' Enter";
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        let out = guard(send, &env);
        assert!(out.contains("\"deny\""), "{bypass:?}: {out}");
        assert!(out.contains("#9001"), "{bypass:?}: {out}");
        assert!(out.contains("`=nosuch9001:0`"), "{bypass:?}: {out}");
        assert!(out.contains("cannot list"), "{bypass:?}: {out}");
    }
    // A read verb, and a command with no tmux, are not this floor's.
    for command in ["tmux capture-pane -p -t =nosuch9001:0", "echo tmux"] {
        let out = guard(command, &[]);
        assert!(!out.contains("#9001"), "{command}: {out}");
    }
}
