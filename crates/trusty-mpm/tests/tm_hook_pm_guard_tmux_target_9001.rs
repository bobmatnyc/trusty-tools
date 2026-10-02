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

/// #9001 critic r2: each bypass its probe of the built binary found ALLOWED
/// is refused under each bypass variable, and each over-denial and read verb
/// passes. With the listing refused, any judged tmux target denies, so a pass
/// here means the floor never saw the command.
#[test]
fn every_r2_bypass_is_refused_and_prose_passes_under_each_bypass() {
    let refused = [
        "T=tm''ux; $T send-keys -t nos hi",
        "T=tm; ${T}ux send-keys -t nos hi",
        "a=t b=mux; $a$b send-keys -t nos hi",
        "set -- -t nos; tmux send-keys \"$@\" hi",
        "F=; tmux send-keys $F -t nos hi",
        "A='-t nos'; tmux send-keys $A hi",
        "echo '-t nos hi' | xargs tmux send-keys",
        "f(){ tmux send-keys -t nos hi; }; f",
        "time { tmux send-keys -t nos hi; }",
        "coproc tmux send-keys -t nos hi",
        "case x in (x) tmux send-keys -t nos hi;; esac",
        "bash <<< 'tmux send-keys -t nos hi'",
        "echo 'tmux send-keys -t nos hi' | sh",
    ];
    let passes = [
        "~/.cargo/bin/tm doctor | grep tmux",
        "cat <<'EOF' > notes.md\nThe PM's tmux pane is fine\nEOF",
        "gh issue comment 1 --body \"$(cat <<'EOF'\nWe can't trust tmux send-keys here\nEOF\n)\"",
        "tmux capture-pane -t =pm:0 -p",
        "tmux has-session -t nos",
        "tmux ls",
        "tmux display-message -p '#{session_name}'",
        "tmux list-panes -a",
    ];
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in refused {
            let out = guard(command, &env);
            assert!(out.contains("\"deny\""), "{command} {bypass:?}: {out}");
            assert!(out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
        for command in passes {
            let out = guard(command, &env);
            assert!(!out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
    }
}

/// #9001 critic r3: each bypass its review found ALLOWED is refused under
/// each bypass variable, and a shell running a script operand passes.
#[test]
fn every_r3_bypass_is_refused_and_a_script_operand_passes_under_each_bypass() {
    let refused = [
        "echo tmux | xargs -I{} env {} kill-server",
        "read -d '' C <<EOF\ntmux kill-server\nEOF\neval \"$C\"",
        "while read a b; do $a $b; done <<EOF\ntmux kill-server\nEOF",
        "bash <<< 'TMUX killp'",
    ];
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in refused {
            let out = guard(command, &env);
            assert!(out.contains("\"deny\""), "{command} {bypass:?}: {out}");
            assert!(out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
        let script = "cat log | bash scripts/report.sh tmux";
        let out = guard(script, &env);
        assert!(!out.contains("#9001"), "{script} {bypass:?}: {out}");
    }
}

/// Supervisor ruling 2026-10-02, narrow reading of rule (a): a program word
/// the shell expands is refused when its arguments put a tmux deny verb in a
/// verb position, and passes when no tmux text appears (`$P "$A"` is the
/// accepted residual), under each bypass variable.
#[test]
fn a_dynamic_program_word_denies_only_with_a_tmux_verb_or_text() {
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in ["$T send-keys -t nos hi", "$T kill-session -t x"] {
            let out = guard(command, &env);
            assert!(out.contains("\"deny\""), "{command} {bypass:?}: {out}");
            assert!(out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
        for command in ["$EDITOR \"$FILE\"", "$P \"$A\""] {
            let out = guard(command, &env);
            assert!(!out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
    }
}

/// #9001 critic r1: a tmux command the guard cannot read, one behind shell
/// grammar, and `kill-session -a` with a target are refused under each
/// bypass with no Architect live.
#[test]
fn an_unreadable_or_grammar_wrapped_tmux_command_is_refused_under_each_bypass() {
    let send = "tmux send-keys -t =nosuch9001:0 hi";
    let commands = [
        format!("sudo {send}"),
        format!("TMUX_TMPDIR=/tmp/x9001 {send}"),
        format!("{{ {send}; }}"),
        format!("if true; then {send}; fi"),
        format!("f() {{ {send}; }}; f"),
        "tmux kill-session -a -t =nosuch9001".to_owned(),
    ];
    for command in &commands {
        for bypass in BYPASSES {
            let env: Vec<(&str, &str)> = bypass.into_iter().collect();
            let out = guard(command, &env);
            assert!(out.contains("\"deny\""), "{command} {bypass:?}: {out}");
            assert!(out.contains("#9001"), "{command} {bypass:?}: {out}");
        }
    }
}
