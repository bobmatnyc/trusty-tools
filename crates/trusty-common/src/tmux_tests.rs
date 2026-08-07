//! Unit tests for the shared tmux command-construction layer (#3004),
//! split out of `tmux.rs` to keep that file under the 500-SLOC production
//! cap (#610) — the inline `mod tests` counted as production source.

use super::*;

#[test]
fn target_renders_session_and_bare_pane() {
    assert_eq!(TmuxTarget::session("s").as_target(), "s");
    assert_eq!(TmuxTarget::pane("s", "%2").as_target(), "%2");
}

#[test]
fn new_session_argv_idempotent_with_workdir() {
    let argv = tmux_argv(&TmuxCommand::NewSession {
        name: "trusty-mpm-1".into(),
        workdir: Some("/tmp/proj".into()),
        idempotent: true,
        command: None,
    });
    assert_eq!(
        argv,
        [
            "new-session",
            "-A",
            "-d",
            "-s",
            "trusty-mpm-1",
            "-c",
            "/tmp/proj"
        ]
    );
}

#[test]
fn new_session_argv_without_workdir_or_idempotent() {
    let argv = tmux_argv(&TmuxCommand::NewSession {
        name: "s".into(),
        workdir: None,
        idempotent: false,
        command: None,
    });
    assert_eq!(argv, ["new-session", "-d", "-s", "s"]);
}

#[test]
fn new_session_argv_with_initial_command_is_trailing_positional() {
    // trusty-agents' debugger REPL launcher shape: an initial command
    // must render as the LAST argument regardless of other flags.
    let argv = tmux_argv(&TmuxCommand::NewSession {
        name: "debug-repl".into(),
        workdir: Some("/tmp/proj".into()),
        idempotent: false,
        command: Some("bash -lc 'run-repl'".into()),
    });
    assert_eq!(
        argv,
        [
            "new-session",
            "-d",
            "-s",
            "debug-repl",
            "-c",
            "/tmp/proj",
            "bash -lc 'run-repl'"
        ]
    );
}

#[test]
fn send_keys_literal_argv() {
    let argv = tmux_argv(&TmuxCommand::SendKeys {
        target: TmuxTarget::session("s"),
        keys: "claude --help".into(),
        literal: true,
    });
    assert_eq!(argv, ["send-keys", "-t", "s", "-l", "claude --help"]);
}

#[test]
fn rename_session_argv() {
    let argv = tmux_argv(&TmuxCommand::RenameSession {
        old: "tm-old-01".into(),
        new: "tm-new-01".into(),
    });
    assert_eq!(argv, ["rename-session", "-t", "tm-old-01", "tm-new-01"]);
}

#[test]
fn send_keys_keyname_argv() {
    let argv = tmux_argv(&TmuxCommand::SendKeys {
        target: TmuxTarget::pane("s", "%1"),
        keys: "Enter".into(),
        literal: false,
    });
    assert_eq!(argv, ["send-keys", "-t", "%1", "Enter"]);
}

#[test]
fn send_keys_pane_target_is_bare_id() {
    let argv = tmux_argv(&TmuxCommand::SendKeys {
        target: TmuxTarget::pane("trusty-mpm-xyz", "%6015"),
        keys: "claude --resume".into(),
        literal: true,
    });
    let target_arg = &argv[argv.iter().position(|a| a == "-t").unwrap() + 1];
    assert_eq!(target_arg, "%6015", "pane target must be the bare pane id");
    assert!(
        !target_arg.contains(':'),
        "pane target must not be session-qualified: {target_arg}"
    );
}

#[test]
fn capture_argv() {
    let argv = tmux_argv(&TmuxCommand::CapturePane {
        target: TmuxTarget::session("s"),
        lines: Some(50),
    });
    assert_eq!(argv, ["capture-pane", "-t", "s", "-p", "-S", "-50"]);

    let argv = tmux_argv(&TmuxCommand::CapturePane {
        target: TmuxTarget::session("s"),
        lines: None,
    });
    assert_eq!(argv, ["capture-pane", "-t", "s", "-p"]);
}

#[test]
fn list_sessions_uses_canonical_format() {
    let argv = tmux_argv(&TmuxCommand::ListSessions);
    assert_eq!(argv, ["list-sessions", "-F", SESSION_LIST_FORMAT]);
}

#[test]
fn list_windows_argv() {
    let argv = tmux_argv(&TmuxCommand::ListWindows {
        name: "work".into(),
    });
    assert_eq!(
        argv,
        ["list-windows", "-t", "work", "-F", WINDOW_LIST_FORMAT]
    );
}

#[test]
fn list_panes_argv() {
    let argv = tmux_argv(&TmuxCommand::ListPanes {
        name: "work".into(),
    });
    assert_eq!(
        argv,
        ["list-panes", "-s", "-t", "work", "-F", PANE_LIST_FORMAT]
    );
}

#[test]
fn set_environment_argv() {
    let argv = tmux_argv(&TmuxCommand::SetEnvironment {
        session: "tmpm-brave-otter".into(),
        key: "TM_MANAGED_SESSION_ID".into(),
        value: "11111111-2222-3333-4444-555555555555".into(),
    });
    assert_eq!(
        argv,
        [
            "set-environment",
            "-t",
            "tmpm-brave-otter",
            "TM_MANAGED_SESSION_ID",
            "11111111-2222-3333-4444-555555555555"
        ]
    );
}

#[test]
fn set_global_option_argv() {
    let argv = tmux_argv(&TmuxCommand::SetGlobalOption {
        name: "history-limit".into(),
        value: "100000".into(),
    });
    assert_eq!(argv, ["set-option", "-g", "history-limit", "100000"]);
}

#[test]
fn start_server_argv() {
    // #3386: no session/target arguments — this only ensures the server
    // process itself exists.
    assert_eq!(tmux_argv(&TmuxCommand::StartServer), ["start-server"]);
}

#[test]
fn show_global_option_argv() {
    // #3386: `-v` so the reply is the bare value, directly parseable.
    let argv = tmux_argv(&TmuxCommand::ShowGlobalOption {
        name: "history-limit".into(),
    });
    assert_eq!(argv, ["show-options", "-g", "-v", "history-limit"]);
}

#[test]
fn set_window_global_option_argv() {
    // #5151: `-wg`, NOT `-g`/`-s`/`-pg`. Measured against live tmux 3.6b,
    // `set-option -pg alternate-screen off` and `set-option -s
    // alternate-screen off` both exit 0 and leave the pane still entering
    // the alternate screen; only the window scope takes effect.
    let argv = tmux_argv(&TmuxCommand::SetWindowGlobalOption {
        name: "alternate-screen".into(),
        value: "off".into(),
    });
    assert_eq!(argv, ["set-option", "-wg", "alternate-screen", "off"]);
}

#[test]
fn show_window_global_option_argv_matches_set_scope() {
    // #5151: the readback must query the SAME scope the set wrote, or a
    // wrong-scope set would verify green while doing nothing.
    let set_scope = tmux_argv(&TmuxCommand::SetWindowGlobalOption {
        name: ALTERNATE_SCREEN_OPTION.into(),
        value: "off".into(),
    })[1]
        .clone();
    let show_scope = tmux_argv(&TmuxCommand::ShowWindowGlobalOption {
        name: ALTERNATE_SCREEN_OPTION.into(),
    })[1]
        .clone();
    assert_eq!(set_scope, "-wg");
    assert_eq!(show_scope, set_scope);
    assert_eq!(
        tmux_argv(&TmuxCommand::ShowWindowGlobalOption {
            name: ALTERNATE_SCREEN_OPTION.into()
        }),
        ["show-options", "-wg", "-v", "alternate-screen"]
    );
}

#[test]
fn scrollback_option_commands_uses_configured_values() {
    // Order matters: history-limit must precede mouse (all three must land
    // before the caller's subsequent new-session, but the internal
    // order between them is asserted here so it stays stable).
    let cmds = scrollback_option_commands(50_000, true, DEFAULT_TMUX_ALTERNATE_SCREEN);
    assert_eq!(cmds.len(), 3);
    assert_eq!(
        tmux_argv(&cmds[0]),
        ["set-option", "-g", "history-limit", "50000"]
    );
    assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "on"]);
}

#[test]
fn scrollback_option_commands_mouse_off() {
    let cmds = scrollback_option_commands(100_000, false, DEFAULT_TMUX_ALTERNATE_SCREEN);
    assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "off"]);
}

#[test]
fn scrollback_option_commands_alternate_screen_defaults_on() {
    // #5151: `on` is tmux's factory default and today's behaviour — the
    // knob is opt-IN, so the default must never alter an operator's
    // terminal.
    // Compile-time tripwire: flipping the default is a deliberate act.
    const { assert!(DEFAULT_TMUX_ALTERNATE_SCREEN) };
    let cmds = scrollback_option_commands(100_000, true, DEFAULT_TMUX_ALTERNATE_SCREEN);
    assert_eq!(
        tmux_argv(&cmds[2]),
        ["set-option", "-wg", "alternate-screen", "on"]
    );
}

#[test]
fn scrollback_option_commands_alternate_screen_off_uses_window_scope() {
    // #5151: a configured `off` is what turns it off, and it must be
    // written at the window scope — see set_window_global_option_argv.
    let cmds = scrollback_option_commands(100_000, true, false);
    assert_eq!(
        tmux_argv(&cmds[2]),
        ["set-option", "-wg", "alternate-screen", "off"]
    );
}

// ── #3004: the shared ordering-guarantee recipe ─────────────────────

#[test]
fn managed_session_commands_orders_options_before_new_session() {
    let cmds = managed_session_commands("sess", Some("/tmp"), 100_000, true, false, true, None);
    assert_eq!(cmds.len(), 4);
    assert_eq!(
        tmux_argv(&cmds[0]),
        ["set-option", "-g", "history-limit", "100000"]
    );
    assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "on"]);
    assert_eq!(
        tmux_argv(&cmds[2]),
        ["set-option", "-wg", "alternate-screen", "off"]
    );
    assert_eq!(
        tmux_argv(&cmds[3]),
        ["new-session", "-A", "-d", "-s", "sess", "-c", "/tmp"]
    );
}

#[test]
fn managed_session_commands_idempotent_flag() {
    let non_idempotent = managed_session_commands("sess", None, 100_000, true, true, false, None);
    assert_eq!(
        tmux_argv(&non_idempotent[3]),
        ["new-session", "-d", "-s", "sess"]
    );

    let idempotent = managed_session_commands("sess", None, 100_000, true, true, true, None);
    assert_eq!(
        tmux_argv(&idempotent[3]),
        ["new-session", "-A", "-d", "-s", "sess"]
    );
}

#[test]
fn managed_session_commands_with_initial_command() {
    let cmds = managed_session_commands(
        "debug-repl",
        None,
        100_000,
        true,
        true,
        false,
        Some("run-repl"),
    );
    assert_eq!(
        tmux_argv(&cmds[3]),
        ["new-session", "-d", "-s", "debug-repl", "run-repl"]
    );
}

#[test]
fn default_history_limit_is_100_000() {
    assert_eq!(DEFAULT_TMUX_HISTORY_LIMIT, 100_000);
}
