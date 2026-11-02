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
//! refused by this floor. #10665 r3: every row runs as the PM and as an
//! engineer subagent (`agent_id`, `agent_type`), and covers the fail-closed
//! rows, the inert-argument rows and the sandbox script (df-D77).
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

/// The two callers every row runs as: the PM, and an engineer subagent.
const CALLERS: [Option<&str>; 2] = [None, Some("engineer")];

/// The guard's stdout for a Bash `command` from the PM, or from a subagent
/// of `agent` type, with `$TMUX` naming a socket under `root` and `env` added.
fn guard(root: &Path, command: &str, env: &[(&str, &str)], agent: Option<&str>) -> String {
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
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-1",
        "cwd": project.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    // #10665 r3: a subagent payload carries its id and type.
    if let Some(agent) = agent {
        payload["agent_id"] = serde_json::json!("a-123");
        payload["agent_type"] = serde_json::json!(agent);
    }
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
        "tmux -L default kill-server".to_string(),
        // #10665 r3 (df-D77): kill words no judged tmux command accounts for.
        format!(
            "cat > {s} <<'EOF'\ntmux kill-server\nEOF\nbash {s}",
            s = script.display()
        ),
        format!("tee {} <<< 'tmux kill-server' && sh {0}", script.display()),
        "awk 'BEGIN{system(\"tmux kill-server\")}'".to_string(),
        "tclsh <<< 'exec tmux kill-server'".to_string(),
        "alias t=tmux\nt kill-server".to_string(),
        "ssh localhost tmux kill-server".to_string(),
        "tmux kill-session -t =tm-ws-x".to_string(),
    ] {
        for bypass in BYPASSES {
            for agent in CALLERS {
                let env: Vec<(&str, &str)> = bypass.into_iter().collect();
                let out = guard(&root, &command, &env, agent);
                assert!(
                    out.contains("\"deny\""),
                    "{command} {bypass:?} {agent:?}: {out}"
                );
                assert!(
                    out.contains("GP-050"),
                    "{command} {bypass:?} {agent:?}: {out}"
                );
                assert!(out.contains("-S <absolute scratch socket>"), "{out}");
            }
        }
    }
}

/// #10665 r3 (#10548): a command that only prints or searches for the words,
/// and the sandbox script, are not refused by this floor, as either caller.
#[test]
fn inert_mentions_and_the_sandbox_script_are_not_refused_by_this_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sandbox = repo.join("scripts/sandbox_daemon.sh");
    for command in [
        "grep -rn kill-server crates/".to_string(),
        "rg 'tmux kill-server' docs/".to_string(),
        "echo \"never run tmux kill-server\"".to_string(),
        "git commit -m \"guard tmux kill-server\"".to_string(),
        "sed -n '/kill-server/p' file".to_string(),
        "cat docs/x.md".to_string(),
        format!("bash {} --help", sandbox.display()),
    ] {
        for agent in CALLERS {
            let out = guard(&root, &command, &[], agent);
            assert!(!out.contains("#10665"), "{command} {agent:?}: {out}");
        }
    }
}

/// An explicit scratch `-S` outside the tmux socket directory is not this
/// floor's refusal.
#[test]
fn an_explicit_scratch_socket_is_not_refused_by_this_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical tempdir");
    let sock = root.join("s.sock").display().to_string();
    // #10665 r3 (df-D77): `env -u TMUX` with an explicit scratch socket or a
    // scratch `TMUX_TMPDIR`, and an `-L` label that is not the default.
    for command in [
        format!("tmux -S {sock} kill-server"),
        format!("env -u TMUX tmux -S {sock} kill-server"),
        format!(
            "env -u TMUX TMUX_TMPDIR={} tmux kill-server",
            root.display()
        ),
        "tmux -L dfg01 kill-server".to_string(),
    ] {
        for agent in CALLERS {
            let out = guard(&root, &command, &[], agent);
            assert!(!out.contains("#10665"), "{command} {agent:?}: {out}");
        }
    }
}
