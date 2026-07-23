//! Shared, pure tmux command-construction layer (issue #3004, consolidating
//! trusty-mpm's #2398/#2399 choke point and trusty-agents' independent tmux
//! implementation onto one workspace-wide source of truth).
//!
//! Why: before #3004 there were TWO independent tmux implementations —
//! `trusty-mpm`'s `core::tmux` (which #2398/#2399 fixed to apply generous
//! scrollback ergonomics BEFORE `new-session`) and `trusty-agents`'
//! `tmux::orchestrator`/`debugger::tmux` (which never got that fix, because
//! they never routed through trusty-mpm's choke point). A scrollback bug
//! report against trusty-agents on 2026-07-18 surfaced the duplication as the
//! root cause: #2399 could only fix the crate it lived in. This module is the
//! single, dependency-light home for the PURE parts of that layer — argv
//! construction and the option-before-pane-creation ORDERING GUARANTEE — so
//! every tmux-session-creating call site in the workspace, in any crate, can
//! share it. Process spawning, TCC-disclaim wrapping, daemon-specific error
//! types, and config-file loading are deliberately NOT here — those are
//! consumer-specific concerns (see each crate's own thin adapter).
//! What: [`TmuxTarget`] (session\[:pane\] addressing), [`TmuxCommand`] (a
//! typed tmux sub-command), [`tmux_argv`] (renders a command to an argv
//! vector), [`scrollback_option_commands`] (the two `set-option -g` entries
//! that must land before any `new-session`), [`managed_session_commands`]
//! (the full ordered recipe: scrollback options THEN `new-session`), and the
//! [`DEFAULT_TMUX_HISTORY_LIMIT`]/[`DEFAULT_TMUX_MOUSE`] defaults.
//! Test: `cargo test -p trusty-common -- tmux::` asserts the rendered argv
//! for each command shape and the options-before-new-session ordering
//! guarantee. Process-spawning is NOT tested here (no process is ever
//! spawned by this module) — see each consumer crate's own tests for that.

use serde::{Deserialize, Serialize};

/// Addresses a tmux session, optionally a specific pane within it.
///
/// Why: every tmux I/O command needs a `-t` target; modelling it once avoids
/// re-deriving the target string at each call site.
/// What: a session name plus an optional pane id (`%0`, `%1`, ...).
/// Test: `target_renders_session_and_bare_pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxTarget {
    /// tmux session name.
    pub session: String,
    /// Optional pane id; `None` addresses the session's active pane.
    #[serde(default)]
    pub pane: Option<String>,
}

impl TmuxTarget {
    /// Address the active pane of a named session.
    pub fn session(name: impl Into<String>) -> Self {
        Self {
            session: name.into(),
            pane: None,
        }
    }

    /// Address a specific pane within a session.
    pub fn pane(name: impl Into<String>, pane: impl Into<String>) -> Self {
        Self {
            session: name.into(),
            pane: Some(pane.into()),
        }
    }

    /// Render the tmux `-t` target string.
    ///
    /// Why: tmux pane ids (`%NNNN`) are GLOBALLY unique across the whole
    /// server, not scoped to a session — but `-t "<session>:<pane_id>"`
    /// still parses everything after the `:` as a WINDOW spec, not a pane
    /// spec (live tmux 3.6b proof, trusty-mpm issue #2467).
    /// What: a pane target renders as the BARE pane id (`%NNNN`) with no
    /// session qualifier; a session-only target still renders the session
    /// name.
    /// Test: `target_renders_session_and_bare_pane`.
    pub fn as_target(&self) -> String {
        match &self.pane {
            Some(p) => p.clone(),
            None => self.session.clone(),
        }
    }
}

/// A typed tmux sub-command.
///
/// Why: enumerating the small set of tmux operations consumers need (vs.
/// building ad-hoc argv vectors at each call site) keeps tmux usage
/// auditable and the argv rendering testable without spawning processes.
/// What: covers session lifecycle, keystroke injection, and output capture.
/// Test: see the per-variant tests in this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TmuxCommand {
    /// `new-session -d -s <name> [-A] [-c <dir>] [<command>]` — create a
    /// detached session.
    NewSession {
        /// Session name.
        name: String,
        /// Optional working directory for the session's first pane.
        workdir: Option<String>,
        /// When `true`, renders `-A` so an existing session of the same
        /// name is attached to rather than causing a duplicate-session
        /// error (trusty-mpm's idempotent-creation semantics).
        /// trusty-agents' consumers pass `false` (fail-if-exists, matching
        /// their pre-#3004 behavior).
        idempotent: bool,
        /// Optional initial command to run in the pane's first process
        /// instead of the default shell (trusty-agents' debugger REPL
        /// launcher). `None` runs the default shell.
        command: Option<String>,
    },
    /// `kill-session -t <name>` — destroy a session.
    KillSession {
        /// Session name to kill.
        name: String,
    },
    /// `rename-session -t <old> <new>` — rename a session in place.
    RenameSession {
        /// Current session name.
        old: String,
        /// New session name.
        new: String,
    },
    /// `has-session -t <name>` — probe whether a session exists.
    HasSession {
        /// Session name to probe.
        name: String,
    },
    /// `list-sessions -F <fmt>` — enumerate sessions.
    ListSessions,
    /// `list-windows -t <name> -F <fmt>` — enumerate a session's windows.
    ListWindows {
        /// Session whose windows to list.
        name: String,
    },
    /// `list-panes -s -t <name> -F <fmt>` — enumerate ALL of a session's
    /// panes, across every window (`-s`).
    ///
    /// Why: without `-s`, tmux resolves a session-name target to only the
    /// session's currently ACTIVE window and lists that window's panes — a
    /// pane in a non-active window would silently vanish from this list
    /// even though it is still alive (trusty-mpm issue #2467).
    ListPanes {
        /// Session whose panes to list.
        name: String,
    },
    /// `send-keys -t <target> [-l] <keys>` — inject keystrokes.
    SendKeys {
        /// Target session/pane.
        target: TmuxTarget,
        /// The keys (or literal text) to send.
        keys: String,
        /// When true, pass `-l` so tmux sends the text literally rather
        /// than interpreting words like `Enter`/`C-c` as key names.
        literal: bool,
    },
    /// `capture-pane -t <target> -p [-S -<lines>]` — capture pane output.
    CapturePane {
        /// Target session/pane.
        target: TmuxTarget,
        /// Optional number of trailing scrollback lines to capture.
        lines: Option<u32>,
    },
    /// `set-environment -t <session> <key> <value>` — durably publish a
    /// variable into the tmux SESSION environment.
    ///
    /// Why: a pane-shell `export` prefix only lands in the one shell
    /// process that ran it — a sibling pane/window never sees it.
    /// `set-environment` writes into the session's OWN environment table
    /// instead, queryable via `show-environment` regardless of which
    /// pane/shell asks or when it was created.
    /// What: targets the SESSION (not a pane).
    SetEnvironment {
        /// Session to set the variable in.
        session: String,
        /// Environment variable name.
        key: String,
        /// Environment variable value.
        value: String,
    },
    /// `set-option -g <name> <value>` — set a server-wide (global) tmux
    /// option (#2398).
    ///
    /// Why: managed sessions need a generous scrollback (`history-limit`)
    /// and mouse-wheel scrolling (`mouse`) applied to the SERVER before any
    /// pane is created — `history-limit` is captured into a pane's ring
    /// buffer AT CREATION TIME, so a per-session `set-option` issued after
    /// a pane already exists would not retroactively grow it. Every
    /// consumer in this workspace runs its managed sessions on the ONE
    /// default tmux server (no dedicated `-S` socket), so a single `-g`
    /// (global) `set-option` benefits every session subsequently created
    /// on that server, regardless of which crate created it.
    ///
    /// **Caveat (#3386):** unlike `new-session`/`attach-session`,
    /// `set-option -g` has no `CMD_STARTSERVER` behavior — it fails ("no
    /// server running") if issued before ANY server-starting command has
    /// run. A caller that issues this before confirming the server exists
    /// (e.g. [`TmuxCommand::StartServer`]) can have it silently no-op right
    /// before a `new-session` call spawns a fresh server that never saw the
    /// option.
    /// What: renders `set-option -g <name> <value>`.
    SetGlobalOption {
        /// tmux option name (e.g. `history-limit`, `mouse`).
        name: String,
        /// Option value (e.g. `"100000"`, `"on"`).
        value: String,
    },
    /// `start-server` — ensure the tmux SERVER exists, creating it if
    /// necessary, without creating any session (#3386).
    ///
    /// Why: [`TmuxCommand::SetGlobalOption`]'s caveat above is exactly the
    /// #3386 root cause — a resume/recreate path that issues
    /// `SetGlobalOption` before any session-creating command has run (e.g.
    /// right after the previous tmux server died) hits "no server running",
    /// the failure is logged as non-fatal, and the global option never
    /// lands before the SUBSEQUENT `new-session` call spawns a fresh server
    /// AND a pane in one step — that pane is then born under tmux's own
    /// factory `history-limit` of 2000 instead of the intended value.
    /// Issuing `start-server` explicitly, and confirming it exits
    /// successfully, before any `set-option -g` closes that race — it is
    /// the one tmux sub-command whose entire job is "make sure a server
    /// exists", so a caller can rely on its own exit status rather than a
    /// side effect of an unrelated command.
    /// What: renders `start-server` with no further arguments. Idempotent —
    /// tmux documents this as a no-op against an already-running server.
    StartServer,
    /// `show-options -g -v <name>` — read back a server-wide (global) tmux
    /// option's CURRENT value (#3386).
    ///
    /// Why: `set-option -g` reporting a zero exit does not, by itself,
    /// prove the value a subsequently-created pane will actually observe —
    /// this is the verification counterpart to
    /// [`TmuxCommand::SetGlobalOption`], letting a caller confirm the
    /// option truly landed on the server before trusting it and creating a
    /// pane that will inherit it.
    /// What: renders `show-options -g -v <name>` (`-v` prints the bare
    /// value only, with no leading `<name> ` prefix, on a single line of
    /// stdout).
    ShowGlobalOption {
        /// tmux option name to read back (e.g. `history-limit`).
        name: String,
    },
}

/// tmux `-F` format string for `list-sessions`.
///
/// Why: a single canonical format keeps every consumer's parser aligned
/// with the command emitted here.
/// What: name, creation epoch, attached flag — colon-separated.
pub const SESSION_LIST_FORMAT: &str = "#{session_name}:#{session_created}:#{session_attached}";

/// tmux `-F` format string for `list-windows` (`index:name`).
pub const WINDOW_LIST_FORMAT: &str = "#{window_index}:#{window_name}";

/// tmux `-F` format string for `list-panes` (`pane_id:active`).
pub const PANE_LIST_FORMAT: &str = "#{pane_id}:#{pane_active}";

/// tmux global option name for scrollback lines retained per pane (#2398).
pub const HISTORY_LIMIT_OPTION: &str = "history-limit";

/// tmux global option name for mouse-wheel scrolling / copy-mode (#2398).
pub const MOUSE_OPTION: &str = "mouse";

/// Built-in default tmux `history-limit` (scrollback lines) applied to every
/// managed session (#2398, moved to this shared layer by #3004).
///
/// Why: tmux's own default is 2000 lines, which a long-running session
/// exhausts almost immediately. 100,000 lines comfortably covers a full
/// working session without materially growing tmux's per-pane memory
/// footprint (each line is only retained while it exists in the pane).
/// What: `100_000`.
pub const DEFAULT_TMUX_HISTORY_LIMIT: u32 = 100_000;

/// Built-in default for whether mouse-wheel scrolling (and click-to-select
/// copy mode) is enabled on the tmux server (#2398, moved by #3004).
///
/// Why: a large `history-limit` is only reachable in practice if the
/// operator has an easy way to scroll into it; tmux's `mouse on` option
/// maps the wheel to scrolling the pane / entering copy-mode.
/// What: `true`.
pub const DEFAULT_TMUX_MOUSE: bool = true;

/// Render a [`TmuxCommand`] into an argv vector suitable for `Command::args`.
///
/// Why: separating argv construction from process spawning makes the
/// command logic pure and unit-testable; a consumer just executes `tmux`
/// with the returned argv via whatever spawning mechanism it needs (plain
/// `Command`, TCC-disclaimed spawn, etc.).
/// What: returns the argument list (excluding the `tmux` program name
/// itself).
/// Test: `new_session_argv*`, `send_keys_literal_argv`, `capture_argv`, etc.
pub fn tmux_argv(cmd: &TmuxCommand) -> Vec<String> {
    match cmd {
        TmuxCommand::NewSession {
            name,
            workdir,
            idempotent,
            command,
        } => {
            let mut argv = vec!["new-session".to_string()];
            if *idempotent {
                // `-A` attaches to an existing session of the same name
                // instead of failing with "duplicate session", making
                // creation idempotent.
                argv.push("-A".to_string());
            }
            argv.push("-d".to_string());
            argv.push("-s".to_string());
            argv.push(name.clone());
            if let Some(dir) = workdir {
                argv.push("-c".to_string());
                argv.push(dir.clone());
            }
            if let Some(cmd) = command {
                // `command` is a trailing POSITIONAL argument in tmux's own
                // `new-session` syntax — it must come last regardless of
                // which flags precede it.
                argv.push(cmd.clone());
            }
            argv
        }
        TmuxCommand::KillSession { name } => {
            vec!["kill-session".into(), "-t".into(), name.clone()]
        }
        TmuxCommand::RenameSession { old, new } => {
            vec![
                "rename-session".into(),
                "-t".into(),
                old.clone(),
                new.clone(),
            ]
        }
        TmuxCommand::HasSession { name } => {
            vec!["has-session".into(), "-t".into(), name.clone()]
        }
        TmuxCommand::ListSessions => {
            vec![
                "list-sessions".into(),
                "-F".into(),
                SESSION_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::ListWindows { name } => {
            vec![
                "list-windows".into(),
                "-t".into(),
                name.clone(),
                "-F".into(),
                WINDOW_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::ListPanes { name } => {
            vec![
                "list-panes".into(),
                // `-s`: list every pane in the SESSION (all windows), not
                // just the currently active window's panes.
                "-s".into(),
                "-t".into(),
                name.clone(),
                "-F".into(),
                PANE_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::SendKeys {
            target,
            keys,
            literal,
        } => {
            let mut argv = vec![
                "send-keys".to_string(),
                "-t".to_string(),
                target.as_target(),
            ];
            if *literal {
                argv.push("-l".to_string());
            }
            argv.push(keys.clone());
            argv
        }
        TmuxCommand::CapturePane { target, lines } => {
            let mut argv = vec![
                "capture-pane".to_string(),
                "-t".to_string(),
                target.as_target(),
                "-p".to_string(),
            ];
            if let Some(n) = lines {
                argv.push("-S".to_string());
                argv.push(format!("-{n}"));
            }
            argv
        }
        TmuxCommand::SetEnvironment {
            session,
            key,
            value,
        } => {
            vec![
                "set-environment".to_string(),
                "-t".to_string(),
                session.clone(),
                key.clone(),
                value.clone(),
            ]
        }
        TmuxCommand::SetGlobalOption { name, value } => {
            vec![
                "set-option".to_string(),
                "-g".to_string(),
                name.clone(),
                value.clone(),
            ]
        }
        TmuxCommand::StartServer => vec!["start-server".to_string()],
        TmuxCommand::ShowGlobalOption { name } => {
            vec![
                "show-options".to_string(),
                "-g".to_string(),
                "-v".to_string(),
                name.clone(),
            ]
        }
    }
}

/// Build the `set-option -g` command sequence that applies the scrollback +
/// mouse-scroll ergonomics to the tmux server (#2398).
///
/// Why: a pure, unit-testable builder for the exact commands a consumer
/// must issue (and their order) — the resolved values come from each
/// consumer's own config (e.g. trusty-mpm's `core::trusty_tools_config`),
/// not from here, so this function stays free of any config/file-system
/// dependency and is testable with plain arguments.
/// What: two [`TmuxCommand::SetGlobalOption`] entries, `history-limit` then
/// `mouse` (rendered `"on"`/`"off"`), in the order the caller must run
/// them — both must land before the caller's subsequent `new-session`.
/// Test: `scrollback_option_commands_uses_configured_values`,
/// `scrollback_option_commands_mouse_off`.
pub fn scrollback_option_commands(history_limit: u32, mouse: bool) -> Vec<TmuxCommand> {
    vec![
        TmuxCommand::SetGlobalOption {
            name: HISTORY_LIMIT_OPTION.to_string(),
            value: history_limit.to_string(),
        },
        TmuxCommand::SetGlobalOption {
            name: MOUSE_OPTION.to_string(),
            value: if mouse { "on" } else { "off" }.to_string(),
        },
    ]
}

/// Build the full ordered command sequence for creating a tmux session with
/// generous scrollback ergonomics applied FIRST (issue #3004): the two
/// [`scrollback_option_commands`] entries followed by `new-session`.
///
/// Why: `history-limit` is captured into a pane's ring buffer AT CREATION
/// TIME — every session-creating call site, in every crate, must apply the
/// scrollback/mouse ergonomics before its `new-session` call, or a future
/// pane is silently born with tmux's tiny 2000-line default (this is
/// exactly how trusty-agents' independent tmux implementation missed the
/// #2398/#2399 fix — it never shared this ordering guarantee). This is the
/// single, unit-tested source of that ordering: a consumer that calls this
/// function structurally cannot get the order wrong.
/// What: returns exactly 3 [`TmuxCommand`]s in caller-execution order —
/// `SetGlobalOption(history-limit)`, `SetGlobalOption(mouse)`,
/// `NewSession`. `idempotent` and `command` are threaded straight into the
/// `NewSession` entry (see its field docs) so callers with differing
/// creation semantics (trusty-mpm's idempotent `-A` reuse vs.
/// trusty-agents' fail-if-exists) can still share this one recipe.
/// Test: `managed_session_commands_orders_options_before_new_session`,
/// `managed_session_commands_idempotent_flag`,
/// `managed_session_commands_with_initial_command`.
pub fn managed_session_commands(
    name: &str,
    workdir: Option<&str>,
    history_limit: u32,
    mouse: bool,
    idempotent: bool,
    command: Option<&str>,
) -> Vec<TmuxCommand> {
    let mut commands = scrollback_option_commands(history_limit, mouse);
    commands.push(TmuxCommand::NewSession {
        name: name.to_string(),
        workdir: workdir.map(str::to_string),
        idempotent,
        command: command.map(str::to_string),
    });
    commands
}

#[cfg(test)]
mod tests {
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
    fn scrollback_option_commands_uses_configured_values() {
        // Order matters: history-limit must precede mouse (both must land
        // before the caller's subsequent new-session, but the internal
        // order between the two is asserted here so it stays stable).
        let cmds = scrollback_option_commands(50_000, true);
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            tmux_argv(&cmds[0]),
            ["set-option", "-g", "history-limit", "50000"]
        );
        assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "on"]);
    }

    #[test]
    fn scrollback_option_commands_mouse_off() {
        let cmds = scrollback_option_commands(100_000, false);
        assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "off"]);
    }

    // ── #3004: the shared ordering-guarantee recipe ─────────────────────

    #[test]
    fn managed_session_commands_orders_options_before_new_session() {
        let cmds = managed_session_commands("sess", Some("/tmp"), 100_000, true, true, None);
        assert_eq!(cmds.len(), 3);
        assert_eq!(
            tmux_argv(&cmds[0]),
            ["set-option", "-g", "history-limit", "100000"]
        );
        assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "on"]);
        assert_eq!(
            tmux_argv(&cmds[2]),
            ["new-session", "-A", "-d", "-s", "sess", "-c", "/tmp"]
        );
    }

    #[test]
    fn managed_session_commands_idempotent_flag() {
        let non_idempotent = managed_session_commands("sess", None, 100_000, true, false, None);
        assert_eq!(
            tmux_argv(&non_idempotent[2]),
            ["new-session", "-d", "-s", "sess"]
        );

        let idempotent = managed_session_commands("sess", None, 100_000, true, true, None);
        assert_eq!(
            tmux_argv(&idempotent[2]),
            ["new-session", "-A", "-d", "-s", "sess"]
        );
    }

    #[test]
    fn managed_session_commands_with_initial_command() {
        let cmds =
            managed_session_commands("debug-repl", None, 100_000, true, false, Some("run-repl"));
        assert_eq!(
            tmux_argv(&cmds[2]),
            ["new-session", "-d", "-s", "debug-repl", "run-repl"]
        );
    }

    #[test]
    fn default_history_limit_is_100_000() {
        assert_eq!(DEFAULT_TMUX_HISTORY_LIMIT, 100_000);
    }
}
