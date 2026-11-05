//! Tests for the whole-server kill floor (#10665). No test runs tmux, and
//! no test touches a tmux socket: every `$TMUX` and `-S` is a path string.
//! Owner ruling Dac: `refused` rows refuse, `logged` rows are log-only, and
//! `None` rows are allowed silently.

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

/// Whether the floor refuses `command` under [`fleet_env`].
fn refused(command: &str) -> bool {
    refused_in(command, &fleet_env())
}

/// Whether the floor refuses `command` under `env`.
fn refused_in(command: &str, env: &ServerEnv) -> bool {
    matches!(
        evaluate_tmux_kill_server(command, env),
        Some(KillVerdict::Refuse(_))
    )
}

/// #10665 (ruling Dac): whether `command` is a log-only would-refuse.
fn logged(command: &str) -> bool {
    matches!(
        evaluate_tmux_kill_server(command, &fleet_env()),
        Some(KillVerdict::LogOnly(_))
    )
}

/// #10665 (ruling Dac, df-D7f): the rows the owner's ruling names, one each.
#[test]
fn the_ruling_dac_rows_refuse_or_allow() {
    // kill-server under an inherited `$TMUX`.
    assert!(refused("tmux kill-server"));
    // Bare kill-server on the default server: no `$TMUX` at all.
    let bare = ServerEnv {
        tmux: None,
        tmux_tmpdir: None,
        uid: 501,
    };
    assert!(refused_in("tmux kill-server", &bare));
    // kill-session of a `tm-` session.
    assert!(refused("tmux kill-session -t tm-foo"));
    assert!(refused_in("tmux kill-session -t tm-foo", &bare));
    // A scratch `-S` and a scratch `-L` are allowed.
    assert_eq!(
        evaluate_tmux_kill_server("tmux -S /abs/scratch.sock kill-server", &fleet_env()),
        None
    );
    assert_eq!(
        evaluate_tmux_kill_server("tmux -L scratch1 kill-server", &fleet_env()),
        None
    );
    // `-L default` is the default server.
    assert!(refused("tmux -L default kill-server"));
    // df-D7f: `kill-session -a` on the fleet server kills `tm-` sessions; on
    // a scratch server it is allowed.
    assert!(refused("tmux kill-session -a"));
    assert!(refused_in("tmux kill-session -a", &bare));
    assert_eq!(
        evaluate_tmux_kill_server("tmux -S /abs/scratch.sock kill-session -a", &fleet_env()),
        None
    );
}

/// #10665 r4: an `-L` label is a file name on APFS, which folds case, so a
/// case variant of `default`, of the `$TMUX` label or of `tm-` is the fleet's
/// server. A scratch label stays allowed.
#[test]
fn a_case_variant_of_a_fleet_label_is_refused() {
    let dir = socket_dir(None, 501);
    for command in [
        "tmux -L DEFAULT kill-server".to_string(),
        "tmux -L Default kill-session -a".to_string(),
        "tmux -L TM-ws-1 kill-server".to_string(),
        "tmux -L Default kill-session -t tm-x".to_string(),
        format!("tmux -S {} kill-server", dir.join("DEFAULT").display()),
    ] {
        assert!(refused(&command), "{command} must be refused");
    }
    let own = ServerEnv {
        tmux: Some(format!("{},4242,0", dir.join("tm-ws-x").display())),
        ..fleet_env()
    };
    assert!(refused_in("tmux -L TM-WS-X kill-server", &own));
    for command in ["tmux -L scratch kill-server", "tmux -L scr kill-session"] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10665 r4: on the fleet server a `kill-session` that names no literal
/// non-`tm-` session kills the caller's own session or an unknown one; it is
/// refused with and without `$TMUX`. On a scratch server it is allowed.
#[test]
fn an_untargeted_fleet_kill_session_is_refused() {
    let bare = ServerEnv {
        tmux: None,
        tmux_tmpdir: None,
        uid: 501,
    };
    for command in [
        "tmux kill-session",
        "tmux kill-session -t %0",
        "tmux kill-session -t '$0'",
        "tmux kill-session -t @1",
        "tmux kill-session -t :",
        "tmux kill-session -t tm",
        "tmux kill-session -t '*'",
        "tmux kill-session -t TM-x",
    ] {
        assert!(refused(command), "{command} must be refused");
        assert!(refused_in(command, &bare), "{command} must be refused");
    }
    let (_dir, sock) = scratch();
    for command in [
        "tmux -L scr kill-session".to_string(),
        format!("tmux -S {sock} kill-session"),
        "tmux kill-session -t =scratch".to_string(),
        "tmux kill-session -t scratch".to_string(),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10665 (ruling Dac): every other finding records and allows.
#[test]
fn every_other_finding_is_log_only() {
    let dir = socket_dir(None, 501);
    for command in [
        "tmux -S rel.sock kill-server".to_string(),
        "tmux -S \"$SOCK\" kill-server".to_string(),
        format!("tmux -S {} kill-server", dir.join("scratch1").display()),
        "python3 -c \"import os; os.system('tmux kill-server')\"".to_string(),
        "awk 'BEGIN{system(\"tmux kill-server\")}'".to_string(),
        "T=tmux; $T kill-server".to_string(),
    ] {
        assert!(logged(&command), "{command} must be log-only");
    }
}

#[test]
fn the_incident_shape_is_refused_with_tmux_inherited() {
    let (dir, _) = scratch();
    let t = dir.path().display();
    for command in [
        format!("TMUX_TMPDIR={t} tmux kill-server"),
        format!("TMUX_TMPDIR=\"{t}\" tmux kill-server 2>/dev/null || true"),
        // df-D7f: the `kill-session -a` form of the incident.
        format!("TMUX_TMPDIR={t} tmux kill-session -a"),
    ] {
        let Some(KillVerdict::Refuse(reason)) = evaluate_tmux_kill_server(&command, &fleet_env())
        else {
            panic!("{command} must be refused");
        };
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
        // df-D7f: `kill-session -a` on the fleet server.
        "tmux kill-session -a".to_string(),
        "tmux kill-session -a -t keep".to_string(),
        "tmux -Ldefault kill-session -a".to_string(),
        // #10665 r3: the default label, and the label of the `$TMUX` socket.
        "tmux -L default kill-server".to_string(),
        format!("tmux -S {} kill-server", dir.join("default").display()),
        format!("tmux -S {} kill-server", dir.join("tm-ws-x").display()),
        "tmux -S /tmp/tmux-501/default kill-server".to_string(),
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
    assert!(refused_in(&command, &moved));
    // #10665 r3: `-L <label of the $TMUX socket>` is this session's server.
    let own = ServerEnv {
        tmux: Some(format!("{},4242,0", dir.join("tm-ws-x").display())),
        ..env
    };
    assert!(refused_in("tmux -L tm-ws-x kill-server", &own));
    // #10665 (ruling Dac, df-D7f): any `tm-` server holds fleet sessions.
    // This row was allowed at ba1eb15878 and is refused now.
    assert!(refused_in("tmux -L tm-ws-y kill-server", &own));
    assert!(evaluate_tmux_kill_server("tmux -L scratch kill-server", &own).is_none());
    // #10665 (ruling Dac): a relative `-S` is log-only.
    assert!(logged("tmux -S rel.sock kill-server"));
}

#[test]
fn wrapped_nested_and_chained_kills_are_refused() {
    for command in [
        "env TMUX_TMPDIR=/tmp/s tmux kill-server",
        "command tmux kill-server",
        "nohup tmux kill-server",
        "sudo tmux kill-server",
        "echo $(tmux kill-server)",
        "sh -c 'tmux kill-server'",
        "bash -c \"TMUX_TMPDIR=/tmp/s tmux kill-server\"",
        "true; tmux kill-server",
        "cd /tmp && tmux kill-server",
        "false || tmux -L default kill-server",
        "tmux list-sessions \\; kill-server",
        "tmux kill-ser",
        "TMUX kill-server",
        "/usr/local/bin/tmux kill-server",
        // #10665 (ruling Dac): a shell `-c` string and `eval` text are read.
        "bash -lc 'tmux kill-server'",
        "eval tmux kill-server",
        "bash -c \"sh -c 'tmux -L default kill-server'\"",
        // df-D7f: `kill-session -a` in a substitution.
        "x=`tmux kill-session -a`",
        "tmux kill-session -aC",
    ] {
        assert!(refused(command), "{command} must be refused");
    }
    // #10665 r2: inline evaluator code, plain, wrapped and chained; ruling
    // Dac keeps it log-only (#10706).
    for command in [
        "python3 -c \"import os; os.system('tmux kill-server')\"",
        "perl -e 'system \"tmux kill-server\"'",
        "node -e \"require('child_process').execSync('tmux kill-server')\"",
        "osascript -e 'do shell script \"tmux kill-server\"'",
        "env FOO=1 python3 -c \"import os; os.system('tmux kill-session -a')\"",
        "true && perl -e 'system \"tmux kill-server\"'",
        "sh -c \"node -e 'require(1).execSync(\\\"tmux kill-server\\\")'\"",
    ] {
        assert!(logged(command), "{command} must be log-only");
    }
}

/// #10665 (ruling Dac): a kill the guard cannot read is log-only.
#[test]
fn an_unreadable_kill_is_log_only() {
    for command in [
        "tmux kill-server 'unterminated",
        "$'\\x74mux' kill-server",
        "T=tmux; $T kill-server",
        "tmux $FLAGS kill-server",
        "tmux kill-session $ARGS -a",
    ] {
        assert!(logged(command), "{command} must be log-only");
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
/// `mktemp` reads as that path (#10706 C2: `env -i` no longer allows any value).
#[test]
fn a_socket_variable_is_read_from_its_one_assignment() {
    let (_dir, sock) = scratch();
    for command in [
        format!("SOCK={sock}; tmux -S \"$SOCK\" kill-server"),
        "S=$(mktemp -d)/s.sock; tmux -S \"$S\" kill-server".to_string(),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
    assert!(refused(
        "SOCK=/tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server"
    ));
    // #10665 (ruling Dac): a socket the guard cannot resolve is log-only.
    for command in [
        "tmux -S \"$SOCK\" kill-server".to_string(),
        format!("SOCK={sock}; SOCK=/tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server"),
        format!("SOCK={sock}; read SOCK; tmux -S \"$SOCK\" kill-server"),
    ] {
        assert!(logged(&command), "{command} must be log-only");
    }
}

/// #10665 r3 (df-D77): a kill word that no judged tmux command accounts for
/// — a script written and run in the same command, an interpreter, a remote
/// shell, an alias. Ruling Dac: log-only (#10706).
#[test]
fn a_kill_word_no_tmux_hit_accounts_for_is_log_only() {
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
        assert!(logged(command), "{command} must be log-only");
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
/// refuses on the default, `$TMUX` or a `tm-` server; a scratch server
/// allows it. `kill-session -a` naming a `tm-` session refuses too.
#[test]
fn a_fleet_session_kill_needs_a_scratch_server() {
    let (_dir, sock) = scratch();
    for command in [
        "tmux kill-session -t tm-architect",
        "tmux kill-session -t =tm-ws-x",
        "tmux -L default kill-session -t tm-ws-x:0",
        "tmux kill-session -a -t tm-ws-x",
        "tmux -L tm-ws-x kill-session -t tm-ws-x",
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

/// #10706 M1: a `-S` path or `TMUX_TMPDIR` with a `..` component resolves at
/// run time, so `/tmp/n/../tmux-501/default` is the default socket. The text
/// is refused before any path is canonicalized.
#[test]
fn deny_socket_path_with_dotdot() {
    let (dir, _sock) = scratch();
    let t = dir.path().display();
    for command in [
        "tmux -S /tmp/n/../tmux-501/default kill-server".to_string(),
        format!("tmux -S {t}/../tmux-501/default kill-server"),
        "SOCK=/tmp/n/../tmux-501/default; tmux -S \"$SOCK\" kill-server".to_string(),
        "TMUX_TMPDIR=/tmp/n/.. tmux kill-server".to_string(),
        "env -u TMUX TMUX_TMPDIR=/tmp/n/.. tmux kill-server".to_string(),
        format!("env -u TMUX TMUX_TMPDIR={t}/.. tmux kill-server"),
    ] {
        assert!(refused(&command), "{command} must be refused");
    }
}

/// #10706 M1: a scratch `-S` path or `TMUX_TMPDIR` with no `..` stays allowed.
#[test]
fn a_scratch_socket_path_without_dotdot_is_allowed() {
    let (dir, sock) = scratch();
    let t = dir.path().display();
    for command in [
        "tmux -S /tmp/scratch/sock kill-server".to_string(),
        format!("tmux -S {sock} kill-server"),
        format!("env -u TMUX TMUX_TMPDIR={t} tmux kill-server"),
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(&command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10706 H3: `printf -vSOCK` writes `SOCK` with no space after `-v`, so the
/// one-assignment read must count it as a second write and not resolve.
#[test]
fn deny_printf_v_joined_assignment() {
    for command in [
        "SOCK=/tmp/s; printf -vSOCK %s /tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server",
        "SOCK=/tmp/s.sock\nprintf -vSOCK %s /tmp/tmux-501/default\ntmux -S \"$SOCK\" kill-server",
        "SOCK=/tmp/s; read -raSOCK <<< /tmp/tmux-501/default; tmux -S \"$SOCK\" kill-server",
    ] {
        // Ruling Dac: a kill the guard cannot resolve is log-only.
        assert!(logged(command), "{command} must be log-only");
    }
}

/// #10706 H3: a socket variable written once stays allowed.
#[test]
fn a_socket_variable_written_once_is_allowed() {
    for command in [
        "SOCK=/tmp/scratch; tmux -S \"$SOCK\" kill-server",
        // A longer name that ends in the variable's is another variable.
        "SOCK=/tmp/scratch; DAEMON_SOCK=/tmp/d; echo ${#DAEMON_SOCK}; tmux -S \"$SOCK\" kill-server",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10706 H2: only `commit`, `tag`, `merge`, `notes` and `stash` take a
/// message after `-m`; for `git rebase` it is `--merge`, so the next word is
/// an option such as `--exec` and must be read.
#[test]
fn deny_git_rebase_m_exec() {
    for command in [
        "git rebase -m '--exec=tmux kill-server' HEAD~1",
        "git rebase -m -x'tmux kill-server' HEAD~1",
        "git -C . rebase -m '--exec=tmux kill-server' HEAD~1",
        "git rebase -m --exec 'tmux kill-server' HEAD~1",
    ] {
        // Ruling Dac: a kill the guard cannot resolve is log-only.
        assert!(logged(command), "{command} must be log-only");
    }
}

/// #10706 H2: the kill words in a message of a message-taking subcommand
/// stay allowed.
#[test]
fn a_git_message_naming_a_kill_is_allowed() {
    for command in [
        "git commit -m \"tmux kill-server\"",
        "git tag -a v1 -m 'guard tmux kill-server'",
        "git merge --no-ff -m 'tmux kill-server' topic",
        "git notes add -m 'tmux kill-server'",
        "git stash push -m 'tmux kill-server'",
        "git -C crates commit -am 'tmux kill-server'",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10706 C2: the outer shell expands `"$SOCK"` before `env -i` runs, so an
/// `env -i` prefix says nothing about the socket. A variable the command does
/// not assign a literal scratch path is unresolved: log-only (ruling Dac).
#[test]
fn deny_env_i_with_preexpanded_socket() {
    for command in [
        "SOCK=$(echo /tmp/tmux-$(id -u)/default); env -i tmux -S \"$SOCK\" kill-server",
        "SOCK=$(echo /tmp/tmux-$(id -u)/default)\nenv -i tmux -S \"$SOCK\" kill-server",
        "env -i PATH=\"$PATH\" tmux -S \"$SOCK\" kill-server >/dev/null 2>&1 || true",
        "env - tmux -S \"${SOCK}\" kill-session -a",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// Run `git` in `dir` for a test fixture repository.
fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// #10706 C2: with the `env -i` fallback gone, a `scripts/sandbox_*.sh` is
/// trusted only in a bobmatnyc/trusty-tools checkout and only when its bytes
/// match `HEAD`. The repo's own sandbox scripts stay allowed; a copy in
/// another repo, or an edited one, is judged and its unresolved socket logged.
#[test]
fn sandbox_daemon_script_allowed_by_repo_and_content() {
    use crate::commands::pm_guard_secret_script::tmux_kill::evaluate_script_body_tmux_kill;
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for command in [
        "bash scripts/sandbox_daemon.sh --help",
        "bash scripts/sandbox_search_daemon.sh --help",
    ] {
        assert_eq!(
            evaluate_script_body_tmux_kill(command, &repo, &fleet_env()),
            None,
            "{command}"
        );
    }
    let body = "#!/bin/bash\nSOCK=\"$D/tmux-$(id -u)/default\"\n\
                env -i PATH=\"$PATH\" tmux -S \"$SOCK\" kill-server\n";
    let command = "bash scripts/sandbox_fixture.sh";
    let fixture = |origin: &str| {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["remote", "add", "origin", origin]);
        std::fs::create_dir(dir.path().join("scripts")).expect("mkdir");
        std::fs::write(dir.path().join("scripts/sandbox_fixture.sh"), body).expect("write");
        git(dir.path(), &["add", "scripts"]);
        git(dir.path(), &["commit", "-q", "-m", "fixture"]);
        dir
    };
    let ours = fixture("git@github.com:bobmatnyc/trusty-tools.git");
    assert_eq!(
        evaluate_script_body_tmux_kill(command, ours.path(), &fleet_env()),
        None,
        "a committed sandbox script in a trusty-tools checkout"
    );
    std::fs::write(
        ours.path().join("scripts/sandbox_fixture.sh"),
        format!("{body}echo edited\n"),
    )
    .expect("edit");
    assert!(
        evaluate_script_body_tmux_kill(command, ours.path(), &fleet_env()).is_some(),
        "an edited sandbox script is judged"
    );
    let theirs = fixture("https://github.com/someone/trusty-tools-fork.git");
    assert!(
        evaluate_script_body_tmux_kill(command, theirs.path(), &fleet_env()).is_some(),
        "a sandbox script in another repo is judged"
    );
}

/// #10706 H1: `rg --pre` runs a program on every file it searches.
#[test]
fn deny_rg_pre_execution() {
    for command in [
        "rg --pre sh -e kill-server <(echo 'tmux kill-server')",
        "rg --pre=sh 'tmux kill-server' notes.txt",
        "rg --pre-glob '*.txt' --pre sh 'tmux kill-server' .",
        "echo 'tmux kill-server' | rg --pre sh -e kill x.sh",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// #10706 H1: `sort -o` writes a file, and `--compress-program` runs one.
#[test]
fn deny_sort_output_file() {
    for command in [
        "echo 'tmux kill-server' | sort -o x.sh; sh x.sh",
        "echo 'tmux kill-server' | sort -uo x.sh",
        "echo 'tmux kill-server' | sort -ox.sh",
        "printf 'tmux kill-server\\n' | sort --output=x.sh",
        "echo 'tmux kill-server' | sort --out x.sh",
        "echo 'tmux kill-server' | sort --compress-program=sh",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// #10706 H1: `uniq INPUT OUTPUT` writes its second operand.
#[test]
fn deny_uniq_second_file() {
    for command in [
        "echo 'tmux kill-server' | uniq - x.sh; sh x.sh",
        "echo 'tmux kill-server' | uniq -c - x.sh",
        "echo 'tmux kill-server' | uniq -f 1 -- - x.sh",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// #10706 H1: `less -o`/`--log-file` copies its piped input into a file.
#[test]
fn deny_less_log_file() {
    for command in [
        "echo 'tmux kill-server' | less -o x.sh",
        "echo 'tmux kill-server' | less --log-file=x.sh",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// #10706 H1: a `sed -n` script is inert only when every command is an
/// address and `p`, or an `s` whose flags are `g`, `p`, `i`, `m` or a count.
/// `w`, `W`, `e`, an `s///w` or `s///e`, an `-f` script, and anything the
/// reader does not parse are not.
#[test]
fn deny_sed_write_or_execute() {
    for command in [
        "sed -n '1e tmux kill-server' f",
        "sed -n '/tmux kill-server/w x.sh' f",
        "sed -n '/tmux kill-server/wx.sh' f",
        "sed -n '/a;tmux kill-server/W x.sh' f",
        "sed -n '\\,tmux kill-server,w x.sh' f",
        "sed -n 's/^/tmux kill-server/e' f",
        "sed -n 's/^/tmux kill-server/w x.sh' f",
        "sed -n -e '/kill-server/p' -e '1w x.sh' f",
        "sed -n --expression='1w x.sh' kill-server.txt",
        "sed -n -f k.sed kill-server.txt",
        "sed -n '/kill-server/{w x.sh\n}' f",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
}

/// #10706 H1: the readers' plain uses stay allowed.
#[test]
fn safe_reader_programs_allowed() {
    for command in [
        "rg -e 'tmux kill-server' docs/",
        "rg -n --no-heading 'kill-server' crates/ | sort | uniq -c",
        "echo 'tmux kill-server' | sort",
        "echo 'tmux kill-server' | sort -u -k1,1 -t:",
        "echo 'tmux kill-server' | uniq -c",
        "echo 'tmux kill-server' | uniq -f 1 -",
        "grep -rn kill-server crates/ | less",
        "sed -n 's/x/tmux kill-server/p' f",
        "sed -n '/kill-server/p' file",
        "sed -n -e '1,/kill-server/p' f",
        "sed -nE '/tmux kill-(server|session)/!p' f",
        "sed -n '/a;b/p; s|x|tmux kill-server|gp' f",
        "sed --quiet '$p' kill-server.log",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
}

/// #10706 C1: whether the script-body rule records `body`, run as
/// `bash <script>` (ruling Dac: that rule is log-only, so `Some` is its
/// strongest verdict).
fn script_body_flagged(body: &str) -> bool {
    use crate::commands::pm_guard_secret_script::tmux_kill::evaluate_script_body_tmux_kill;
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("c1.sh");
    std::fs::write(&script, body).expect("write script");
    let command = format!("bash {}", script.display());
    evaluate_script_body_tmux_kill(&command, dir.path(), &fleet_env()).is_some()
}

/// #10706 C1: a here-document whose operator line ends in a pipe feeds the
/// command after its body, here `sh`.
#[test]
fn heredoc_piped_to_shell_is_refused() {
    for command in [
        "cat <<EOF |\ntmux kill-server\nEOF\nsh",
        "cat <<EOF |&\ntmux kill-server\nEOF\nbash",
        "cat <<'EOF' \\\n| sh\ntmux kill-server\nEOF",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
    assert!(script_body_flagged(
        "cat <<EOF |\ntmux kill-server\nEOF\nsh\n"
    ));
}

/// #10706 C1: a group's output reaches the pipe after its close.
#[test]
fn command_group_piped_to_shell_is_refused() {
    for command in [
        "{ echo 'tmux kill-server'; } | sh",
        "if true; then printf 'tmux kill-server'; fi | bash",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
    assert!(script_body_flagged("{ echo 'tmux kill-server'; } | sh\n"));
}

/// #10706 C1: a loop's output reaches the pipe after `done`.
#[test]
fn loop_piped_to_shell_is_refused() {
    for command in [
        "for x in 1; do echo 'tmux kill-server'; done | sh",
        "while true; do echo 'tmux kill-server'; break; done | bash",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
    assert!(script_body_flagged(
        "for x in 1; do echo 'tmux kill-server'; done | sh\n"
    ));
}

/// #10706 C1: substitution output stored, run in place, or handed to `eval`
/// or a shell runs the kill.
#[test]
fn command_substitution_executed_is_refused() {
    for command in [
        "x=$(echo tmux kill-server); $x",
        "x=`echo tmux kill-server`\n$x",
        "$(echo tmux kill-server)",
        "eval \"$(echo tmux kill-server)\"",
        "sh <(echo tmux kill-server)",
        "x=$(cat <<'EOF'\ntmux kill-server\nEOF\n)\n$x",
    ] {
        assert!(logged(command), "{command} must not be allowed");
    }
    assert!(script_body_flagged("x=$(echo tmux kill-server)\n$x\n"));
}

/// #10706 C1: printed output a redirect sends to a file a shell then runs,
/// a process substitution, or a coprocess runs the kill, as a pipe does.
#[test]
fn inert_output_redirected_to_shell_is_refused() {
    for command in [
        "{ echo 'tmux kill-server'; } > x.sh; sh x.sh",
        "for i in 1; do echo 'tmux kill-server'; done > >(sh)",
        "{ cat <<EOF\ntmux kill-server\nEOF\n} > x.sh; sh x.sh",
        "exec > >(sh); echo 'tmux kill-server'",
        "coproc sh; echo 'tmux kill-server' >&\"${COPROC[1]}\"",
    ] {
        assert!(logged(command), "{command} must not be allowed");
        assert!(!refused(command), "{command} is log-only (ruling Dac)");
        assert!(
            script_body_flagged(&format!("{command}\n")),
            "{command} must not be allowed as a script body"
        );
    }
}

/// #10706 C1: printed text that reaches no shell keeps its verdict: a
/// here-document to stdout or a reader, and a commit message substitution,
/// stay allowed; a here-document to a file stays a log-only would-refuse.
#[test]
fn inert_output_reaching_no_shell_keeps_its_verdict() {
    for command in [
        "cat <<EOF\ntmux kill-server\nEOF",
        "cat <<EOF | head -1\ntmux kill-server\nEOF",
        "git commit -m \"$(cat <<'EOF'\nguard tmux kill-server\nEOF\n)\"",
        "echo \"$(echo tmux kill-server)\" | wc -l",
    ] {
        assert_eq!(
            evaluate_tmux_kill_server(command, &fleet_env()),
            None,
            "{command}"
        );
    }
    let to_file = "cat <<EOF > file.txt\ntmux kill-server\nEOF";
    assert!(logged(to_file) && !refused(to_file));
}
