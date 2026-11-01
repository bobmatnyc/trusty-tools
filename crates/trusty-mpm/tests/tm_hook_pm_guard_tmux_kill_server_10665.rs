//! End-to-end proof of the #10665 whole-server kill floor.
//!
//! Why: on 2026-11-01 `TMUX_TMPDIR=$T tmux kill-server` ran with `$TMUX`
//! inherited and killed the fleet's tmux server. Only the built binary proves
//! the refusal holds with `$TMUX` set and under each bypass.
//! What: `tm hook --pm-guard` runs as a PM with a scratch `$HOME`, no
//! `TRUSTY_MPM_ALLOW_HOST_STATE` (so the guard's tmux listing is refused,
//! #5784), and `$TMUX` naming a socket inside a temp directory, so no tmux
//! server is touched. The incident shape and `kill-session -a` deny naming
//! the policy, inline and from a script; an explicit scratch `-S` is not
//! refused by this floor.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_tmux_kill_server_10665::`.

use crate::common;

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// The guard's stdout for a PM-stamped Bash `command`, with `$TMUX` naming a
/// socket under `root` and `env` added.
fn guard(root: &Path, command: &str, env: &[(&str, &str)]) -> String {
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(home.join(".trusty-mpm")).expect("mkdir root");
    std::fs::create_dir_all(&project).expect("mkdir project");
    common::write_disk_threshold(&home, 100);
    let tmux = format!("{},4242,0", root.join("tmux-0/default").display());
    let mut cmd = common::tm_command_in(&home);
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&project)
        .env("CLAUDE_PROJECT_DIR", &project)
        .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
        .env("TMUX", tmux)
        .env_remove("TRUSTY_MPM_ALLOW_HOST_STATE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TMUX_TMPDIR")
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

/// The incident shape and `kill-session -a` deny under each bypass, with
/// `$TMUX` set, naming the policy entry and the safe form.
#[test]
fn a_whole_server_kill_with_tmux_inherited_is_refused_under_each_bypass() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let scratch = root.join("scratch").display().to_string();
    // The incident ran from line 38 of a scratch script.
    let script = root.join("exp2.sh");
    let body = format!("#!/bin/bash\nT={scratch}\nTMUX_TMPDIR=\"$T\" tmux kill-server\n");
    std::fs::write(&script, body).expect("write script");
    for command in [
        format!("bash {}", script.display()),
        format!("TMUX_TMPDIR={scratch} tmux kill-server"),
        format!("TMUX_TMPDIR={scratch} tmux kill-session -a"),
        "tmux -L scratch kill-server".to_string(),
    ] {
        for bypass in BYPASSES {
            let env: Vec<(&str, &str)> = bypass.into_iter().collect();
            let out = guard(&root, &command, &env);
            assert!(out.contains("\"deny\""), "{command} {bypass:?}: {out}");
            assert!(out.contains("GP-050"), "{command} {bypass:?}: {out}");
            assert!(out.contains("-S <absolute scratch socket>"), "{out}");
        }
    }
}

/// An explicit scratch `-S` outside the tmux socket directory is not this
/// floor's refusal.
#[test]
fn an_explicit_scratch_socket_is_not_refused_by_this_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let command = format!("tmux -S {} kill-server", root.join("s.sock").display());
    let out = guard(&root, &command, &[]);
    assert!(!out.contains("#10665"), "{out}");
}
