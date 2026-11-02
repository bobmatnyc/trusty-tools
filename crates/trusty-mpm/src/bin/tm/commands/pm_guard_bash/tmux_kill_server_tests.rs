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
        // #10665 r3: `-L` overrides an inherited `$TMUX` (tmux 3.6b).
        "tmux -L x kill-server".to_string(),
        "tmux -L scratch kill-session -a".to_string(),
        "false || tmux -L dfg01 kill-server".to_string(),
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
        // #10665 r3: the default label, and the label of the `$TMUX` socket.
        "tmux -L default kill-server".to_string(),
        "tmux -Ldefault kill-session -a".to_string(),
        format!("tmux -S {} kill-server", dir.join("default").display()),
        format!("tmux -S {} kill-server", dir.join("tm-ws-x").display()),
        "tmux -S /tmp/tmux-501/default kill-server".to_string(),
        "tmux -S rel.sock kill-server".to_string(),
        "env -u TMUX tmux kill-server".to_string(),
        format!(
            "env -u TMUX tmux -S {} kill-server",
            dir.join("default").display()
        ),
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
    // #10665 r3: `-L <label of the $TMUX socket>` is this session's server.
    let own = ServerEnv {
        tmux: Some(format!("{},4242,0", dir.join("tm-ws-x").display())),
        ..env
    };
    assert!(evaluate_tmux_kill_server("tmux -L tm-ws-x kill-server", &own).is_some());
    assert!(evaluate_tmux_kill_server("tmux -L tm-ws-y kill-server", &own).is_none());
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
        "false || tmux -L default kill-server",
        "tmux list-sessions \\; kill-server",
        "tmux kill-ser",
        "tmux kill-session -aC",
        "TMUX kill-server",
        "/usr/local/bin/tmux kill-server",
        // #10665 r2: inline evaluator code, plain, wrapped and chained.
        "python3 -c \"import os; os.system('tmux kill-server')\"",
        "perl -e 'system \"tmux kill-server\"'",
        "node -e \"require('child_process').execSync('tmux kill-server')\"",
        "osascript -e 'do shell script \"tmux kill-server\"'",
        "env FOO=1 python3 -c \"import os; os.system('tmux kill-session -a')\"",
        "true && perl -e 'system \"tmux kill-server\"'",
        "sh -c \"node -e 'require(1).execSync(\\\"tmux kill-server\\\")'\"",
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

/// #10665 r3 (df-D77): `env -u TMUX` or `env -i` running tmux directly with
/// a literal scratch `TMUX_TMPDIR` reaches that directory's server; `/tmp`,
/// no `TMUX_TMPDIR`, an expanded one, a wrapper between, or a `TMUX` set
/// elsewhere in the command does not.
#[test]
fn env_cleared_scratch_tmpdir_kills_are_allowed() {
    let (dir, sock) = scratch();
    let t = dir.path().display();
    for command in [
        format!("env -u TMUX TMUX_TMPDIR={t} tmux kill-server"),
        format!("env -u TMUX -u TMUX_PANE TMUX_TMPDIR={t} tmux kill-session -a"),
        format!("env --unset=TMUX TMUX_TMPDIR={t} tmux kill-server"),
        format!("env -i PATH=/usr/bin TMUX_TMPDIR={t} tmux kill-server 2>/dev/null"),
        format!("env -u TMUX tmux -S {sock} kill-server"),
        format!("env --unset TMUX tmux -S {sock} kill-session -a"),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
    for command in [
        "env -u TMUX tmux kill-server".to_string(),
        "env -i tmux kill-server".to_string(),
        "env -u TMUX TMUX_TMPDIR=/tmp tmux kill-server".to_string(),
        "env -u TMUX TMUX_TMPDIR=/private/tmp tmux kill-server".to_string(),
        "env -u TMUX TMUX_TMPDIR=\"$T\" tmux kill-server".to_string(),
        format!("env TMUX_TMPDIR={t} tmux kill-server"),
        format!("env -u TMUX TMUX_TMPDIR={t} nohup tmux kill-server"),
        format!("env -u TMUX TMUX_TMPDIR={t} TMUX=/tmp/tmux-501/default tmux kill-server"),
        format!("export TMUX=/tmp/tmux-501/default; env -i TMUX_TMPDIR={t} tmux kill-server"),
    ] {
        assert!(refused(&command), "{command} must be refused");
    }
}

/// #10665 r3: a `-S "$VAR"` the command assigns once from a literal path or
/// `mktemp` reads as that path; under `env -i` any value is allowed.
#[test]
fn a_socket_variable_is_read_from_its_one_assignment() {
    let (_dir, sock) = scratch();
    for command in [
        format!("SOCK={sock}; tmux -S \"$SOCK\" kill-server"),
        "S=$(mktemp -d)/s.sock; tmux -S \"$S\" kill-server".to_string(),
        "env -i PATH=\"$PATH\" tmux -S \"$SOCK\" kill-server >/dev/null 2>&1 || true".to_string(),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
    for command in [
        "SOCK=/tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server".to_string(),
        "tmux -S \"$SOCK\" kill-server".to_string(),
        format!("SOCK={sock}; SOCK=/tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server"),
        format!("SOCK={sock}; read SOCK; tmux -S \"$SOCK\" kill-server"),
    ] {
        assert!(refused(&command), "{command} must be refused");
    }
}

/// #10665 r3 (df-D77): a kill word that no judged tmux command accounts for
/// refuses — a script written and run in the same command, an interpreter,
/// a remote shell, an alias.
#[test]
fn a_kill_word_no_tmux_hit_accounts_for_is_refused() {
    for command in [
        "cat > /tmp/k.sh <<'EOF'\nexport TMUX_TMPDIR=/tmp/s\ntmux kill-server\nEOF\nbash /tmp/k.sh",
        "tee /tmp/x.sh <<< 'tmux kill-server' && sh /tmp/x.sh",
        "awk 'BEGIN{system(\"tmux kill-server\")}'",
        "tclsh <<< 'exec tmux kill-server'",
        "alias t=tmux\nt kill-server",
        "ssh localhost tmux kill-server",
        "printf 'tmux kill-server\\n' > /tmp/k.sh; bash /tmp/k.sh",
        "echo 'tmux kill-server' | tee /tmp/k.sh",
        "echo $(awk 'BEGIN{system(\"tmux kill-server\")}')",
        "git -c alias.k='!tmux kill-server' k",
        "sed -i 's/x/tmux kill-server/' a.sh",
        "awk 'BEGIN{system(\"tmux kill-ser\")}'",
        "tmux -S /tmp/dfg01/s.sock run-shell 'awk \"BEGIN{system(\\\"tmux kill-server\\\")}\"'",
    ] {
        assert!(refused(command), "{command} must be refused");
    }
}

/// #10665 r3 (#10548): a kill word in an argument of a known-inert program
/// that prints to stdout is no kill.
#[test]
fn a_kill_word_in_an_inert_argument_is_allowed() {
    for command in [
        "grep -rn kill-server crates/",
        "rg 'tmux kill-server' docs/",
        "echo \"never run tmux kill-server\"",
        "git commit -m \"guard tmux kill-server\"",
        "sed -n '/kill-server/p' file",
        "cat docs/x.md",
        "grep -rn kill-server crates/ | head -20",
        "printf 'tmux kill-server\\n'",
        "git commit -m \"$(cat <<'EOF'\nguard tmux kill-server\nEOF\n)\"",
        "grep -c 'kill-session -a' notes.md 2>/dev/null",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10665 r3 (df-D77): `kill-session` of a fleet session (`tm-…`, `=tm-…`)
/// refuses on the default or `$TMUX` server; a scratch server allows it, and
/// `kill-session -a` stays refused.
#[test]
fn a_fleet_session_kill_needs_a_scratch_server() {
    let (_dir, sock) = scratch();
    for command in [
        "tmux kill-session -t tm-architect",
        "tmux kill-session -t =tm-ws-x",
        "tmux -L default kill-session -t tm-ws-x:0",
        "tmux kill-session -a -t tm-ws-x",
    ] {
        assert!(refused(command), "{command} must be refused");
    }
    for command in [
        format!("tmux -S {sock} kill-session -t =tm-ws-x"),
        "tmux -L dfg01 kill-session -t tm-ws-x".to_string(),
        "tmux kill-session -t =scratch".to_string(),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
}
