//! Tests for the whole-server kill floor (#10665). No test runs tmux.

use super::*;

/// A hook environment whose `$TMUX` names the fleet's default server.
fn fleet_env() -> ServerEnv {
    let uid = 501;
    let dir = socket_dir(None, uid);
    ServerEnv {
        tmux: Some(format!("{},4242,0", dir.join("default").display())),
        tmux_tmpdir: None,
        uid,
    }
}

/// A scratch socket path outside every tmux socket directory.
fn scratch() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("scratch.sock").display().to_string();
    (dir, sock)
}

fn refused(command: &str) -> bool {
    evaluate_tmux_kill_server(command, &fleet_env()).is_some()
}

#[test]
fn the_incident_shape_is_refused_with_tmux_inherited() {
    let (dir, _) = scratch();
    let t = dir.path().display();
    for command in [
        format!("TMUX_TMPDIR={t} tmux kill-server"),
        format!("TMUX_TMPDIR=\"{t}\" tmux kill-server 2>/dev/null || true"),
        format!("TMUX_TMPDIR={t} tmux kill-session -a"),
    ] {
        let reason = evaluate_tmux_kill_server(&command, &fleet_env())
            .unwrap_or_else(|| panic!("{command} must be refused"));
        assert!(reason.contains("#10665"), "{reason}");
        assert!(reason.contains("`$TMUX`"), "{reason}");
        assert!(
            reason.contains("tmux -S <absolute scratch socket> kill-server"),
            "{reason}"
        );
    }
}

#[test]
fn an_explicit_scratch_socket_is_allowed() {
    let (_dir, sock) = scratch();
    for command in [
        format!("tmux -S {sock} kill-server"),
        format!("env TMUX_TMPDIR=/tmp/x tmux -S {sock} kill-server"),
        format!("TMUX_TMPDIR=/tmp/x tmux -S '{sock}' kill-server"),
        format!("tmux -L x -S {sock} kill-session -a"),
        format!("tmux -S{sock} kill-session -a -t keep"),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

#[test]
fn a_default_socket_or_label_is_refused() {
    let env = fleet_env();
    let dir = socket_dir(None, env.uid);
    let (_scratch, sock) = scratch();
    for command in [
        "tmux kill-server".to_string(),
        "tmux kill-session -a".to_string(),
        "tmux kill-session -a -t keep".to_string(),
        "tmux -L x kill-server".to_string(),
        "tmux -L scratch kill-session -a".to_string(),
        format!("tmux -S {} kill-server", dir.join("default").display()),
        format!("tmux -S {} kill-server", dir.join("tm-ws-x").display()),
        "tmux -S /tmp/tmux-501/default kill-server".to_string(),
        "tmux -S rel.sock kill-server".to_string(),
        // The last `-S` is the one tmux uses.
        format!(
            "tmux -S {sock} -S {} kill-server",
            dir.join("default").display()
        ),
    ] {
        assert!(refused(&command), "{command} must be refused");
    }
    // A `TMUX_TMPDIR` in the hook's own environment moves the default too.
    let (tmp, _) = scratch();
    let moved = ServerEnv {
        tmux: None,
        tmux_tmpdir: Some(tmp.path().to_path_buf()),
        uid: env.uid,
    };
    let default = socket_dir(Some(tmp.path()), env.uid).join("default");
    let command = format!("tmux -S {} kill-server", default.display());
    assert!(evaluate_tmux_kill_server(&command, &moved).is_some());
}

#[test]
fn wrapped_nested_and_chained_kills_are_refused() {
    for command in [
        "env TMUX_TMPDIR=/tmp/s tmux kill-server",
        "command tmux kill-server",
        "nohup tmux kill-server",
        "sudo tmux kill-server",
        "echo $(tmux kill-server)",
        "x=`tmux kill-session -a`",
        "sh -c 'tmux kill-server'",
        "bash -c \"TMUX_TMPDIR=/tmp/s tmux kill-server\"",
        "true; tmux kill-server",
        "cd /tmp && tmux kill-server",
        "false || tmux -L x kill-server",
        "tmux list-sessions \\; kill-server",
        "tmux kill-ser",
        "tmux kill-session -aC",
        "TMUX kill-server",
        "/usr/local/bin/tmux kill-server",
    ] {
        assert!(refused(command), "{command} must be refused");
    }
}

#[test]
fn an_unreadable_kill_fails_closed() {
    for command in [
        "tmux kill-server 'unterminated",
        "$'\\x74mux' kill-server",
        "T=tmux; $T kill-server",
        "tmux $FLAGS kill-server",
        "tmux kill-session $ARGS -a",
    ] {
        assert!(refused(command), "{command} must be refused");
    }
}

#[test]
fn ordinary_tmux_commands_are_unaffected() {
    for command in [
        "tmux list-sessions",
        "tmux kill-session -t =scratch",
        "tmux kill-pane -t %3",
        "tmux new-session -d -s work",
        "tmux has-session -t =work",
        "echo kill-server",
        "grep -n kill-server notes.md",
        "git log --oneline",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}
