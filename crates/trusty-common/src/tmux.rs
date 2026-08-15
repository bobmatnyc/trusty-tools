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
//! vector), [`scrollback_option_commands`] (the `set-option` entries that must
//! land before any `new-session`), [`managed_session_commands`] (the full
//! ordered recipe: scrollback options THEN `new-session`), and the
//! [`DEFAULT_TMUX_HISTORY_LIMIT`]/[`DEFAULT_TMUX_MOUSE`]/
//! [`DEFAULT_TMUX_ALTERNATE_SCREEN`] defaults.
//! Test: `cargo test -p trusty-common --features unconditional-only -- tmux::`
//! asserts the rendered argv for each command shape and the
//! options-before-new-session ordering guarantee. Process-spawning is NOT
//! tested here (no process is ever spawned by this module) — see each
//! consumer crate's own tests for that.

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
    /// `set-option -wg <name> <value>` — set a WINDOW-scoped tmux option
    /// globally, so every window subsequently created on this server (and
    /// every pane inheriting from it) observes it (#5151).
    ///
    /// Why: `alternate-screen` is not a server or session option like
    /// `history-limit` and `mouse` — `man tmux` lists it under "Available
    /// pane options", and pane options inherit from the window scope. Copying
    /// [`TmuxCommand::SetGlobalOption`]'s `-g` invocation for it is not
    /// obviously wrong (tmux 3.6 routes a `-g` set by option name, so it
    /// happens to land), but the *pane*-scoped forms silently do nothing:
    /// measured against live tmux 3.6b, `set-option -pg alternate-screen off`
    /// and `set-option -s alternate-screen off` both exit 0, a same-flag
    /// `show-options` readback reports `off`, and the pane STILL enters the
    /// alternate screen (`#{alternate_on}` = 1). Only `-wg` (and the `-g`
    /// tmux rewrites into it) actually takes effect. Modelling the window
    /// scope as its own variant, paired with
    /// [`TmuxCommand::ShowWindowGlobalOption`], is what keeps a caller's
    /// set and its verification probe on the SAME scope — a probe that reads
    /// a different scope than it wrote is a fail-open that reports success
    /// for an option that did nothing.
    /// What: renders `set-option -wg <name> <value>`. Carries the same
    /// server-must-exist caveat as [`TmuxCommand::SetGlobalOption`].
    SetWindowGlobalOption {
        /// tmux window option name (e.g. `alternate-screen`).
        name: String,
        /// Option value (e.g. `"on"`, `"off"`).
        value: String,
    },
    /// `show-options -wg -v <name>` — read back a WINDOW-scoped global tmux
    /// option's CURRENT value (#5151).
    ///
    /// Why: the verification counterpart to
    /// [`TmuxCommand::SetWindowGlobalOption`], and deliberately scope-matched
    /// to it — see that variant's doc for the measured fail-open this pairing
    /// exists to prevent.
    /// What: renders `show-options -wg -v <name>` (`-v` prints the bare value
    /// only, on a single line of stdout).
    ShowWindowGlobalOption {
        /// tmux window option name to read back (e.g. `alternate-screen`).
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

/// tmux WINDOW option name controlling whether programs in a pane may use the
/// terminal's alternate screen buffer (#5151).
///
/// Why: worth naming separately from [`HISTORY_LIMIT_OPTION`] and
/// [`MOUSE_OPTION`] because it lives in a different tmux option scope — see
/// [`TmuxCommand::SetWindowGlobalOption`] for the measured consequences of
/// getting that scope wrong.
/// What: `"alternate-screen"`.
pub const ALTERNATE_SCREEN_OPTION: &str = "alternate-screen";

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

/// Built-in default for whether panes may use the terminal alternate screen
/// buffer (tmux `alternate-screen`) (#5151, flipped to `false` by #5364).
///
/// Why: when a full-screen TUI draws into tmux's alternate screen, tmux does
/// not append that output to the pane's scrollback at all — `history-limit` is
/// irrelevant in that state, which is why a managed session can hold a
/// 100,000-line history and still have nothing to scroll back through. #5151
/// added this knob to fix that, but shipped it defaulting to `true` — tmux's
/// factory value and the pre-fix behaviour — so the original bug persisted for
/// every operator who never found the knob. `false` makes the fix the default:
/// a managed session has working scrollback out of the box.
///
/// **The accepted tradeoff.** `alternate-screen` is server-global and every
/// trusty-* managed session shares one tmux server, so this governs every pane
/// on it, not just the ones running an agent. `vim`, `less`, `htop` and `man`
/// stop restoring the screen they covered: each leaves its final frame behind,
/// smeared into the scrollback on exit, sometimes with redraw garbage from
/// partial repaints. The repo owner accepted that cost in exchange for working
/// scrollback (#5364). Setting `tmux.alternate_screen: true` in
/// `~/.trusty-tools/trusty-mpm/config.yaml` restores the old behaviour.
/// What: `false`. Both consumers of this constant — trusty-mpm's config
/// resolution and trusty-agents' two session-creation call sites — inherit it,
/// so they cannot fight each other over the shared server's global option.
/// Test: `scrollback_option_commands_alternate_screen_defaults_off`;
/// trusty-mpm's `tmux_options_alternate_screen_defaults_off` covers the
/// config-resolution half.
// #5364: default flipped true -> false so scrollback works without opt-in.
pub const DEFAULT_TMUX_ALTERNATE_SCREEN: bool = false;

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
        TmuxCommand::SetWindowGlobalOption { name, value } => {
            vec![
                "set-option".to_string(),
                "-wg".to_string(),
                name.clone(),
                value.clone(),
            ]
        }
        TmuxCommand::ShowWindowGlobalOption { name } => {
            vec![
                "show-options".to_string(),
                "-wg".to_string(),
                "-v".to_string(),
                name.clone(),
            ]
        }
    }
}

/// Build the `set-option` command sequence that applies the scrollback +
/// mouse-scroll + alternate-screen ergonomics to the tmux server (#2398,
/// extended by #5151).
///
/// Why: a pure, unit-testable builder for the exact commands a consumer
/// must issue (and their order) — the resolved values come from each
/// consumer's own config (e.g. trusty-mpm's `core::trusty_tools_config`),
/// not from here, so this function stays free of any config/file-system
/// dependency and is testable with plain arguments.
///
/// **What `alternate_screen: false` costs** — the default since #5364, so this
/// is what a caller passing [`DEFAULT_TMUX_ALTERNATE_SCREEN`] now gets. tmux's
/// alternate screen is what gives a full-screen program its own buffer: the
/// pane's prior contents reappear untouched when the program exits, and nothing
/// the program drew is added to the pane's scrollback. Turning it off is why a
/// TUI's output becomes scrollable — and the price is paid by EVERY pane on the
/// shared tmux server, not just the one running an agent TUI. `vim`, `less`,
/// `htop` and `man` all leave their final frame behind, smeared into the
/// scrollback on exit, sometimes with redraw garbage. It is also not
/// retroactive: history already lost to the alt screen stays lost.
///
/// What: three entries in the order the caller must run them, all of which
/// must land before the caller's subsequent `new-session` —
/// [`TmuxCommand::SetGlobalOption`] for `history-limit`, then the same for
/// `mouse`, then [`TmuxCommand::SetWindowGlobalOption`] for
/// `alternate-screen` (rendered `"on"`/`"off"`). The third uses the WINDOW
/// scope deliberately; see [`TmuxCommand::SetWindowGlobalOption`] for the
/// measured reason the server/pane scopes silently do nothing here.
/// Test: `scrollback_option_commands_uses_configured_values`,
/// `scrollback_option_commands_mouse_off`,
/// `scrollback_option_commands_alternate_screen_defaults_off`,
/// `scrollback_option_commands_alternate_screen_off_uses_window_scope`.
pub fn scrollback_option_commands(
    history_limit: u32,
    mouse: bool,
    alternate_screen: bool,
) -> Vec<TmuxCommand> {
    vec![
        TmuxCommand::SetGlobalOption {
            name: HISTORY_LIMIT_OPTION.to_string(),
            value: history_limit.to_string(),
        },
        TmuxCommand::SetGlobalOption {
            name: MOUSE_OPTION.to_string(),
            value: if mouse { "on" } else { "off" }.to_string(),
        },
        // #5151: window scope, not `-g`/`-s`/`-pg` — see SetWindowGlobalOption.
        TmuxCommand::SetWindowGlobalOption {
            name: ALTERNATE_SCREEN_OPTION.to_string(),
            value: if alternate_screen { "on" } else { "off" }.to_string(),
        },
    ]
}

/// Build the full ordered command sequence for creating a tmux session with
/// generous scrollback ergonomics applied FIRST (issue #3004): every
/// [`scrollback_option_commands`] entry followed by `new-session`.
///
/// Why: `history-limit` is captured into a pane's ring buffer AT CREATION
/// TIME — every session-creating call site, in every crate, must apply the
/// scrollback/mouse ergonomics before its `new-session` call, or a future
/// pane is silently born with tmux's tiny 2000-line default (this is
/// exactly how trusty-agents' independent tmux implementation missed the
/// #2398/#2399 fix — it never shared this ordering guarantee). This is the
/// single, unit-tested source of that ordering: a consumer that calls this
/// function structurally cannot get the order wrong.
/// What: returns exactly 4 [`TmuxCommand`]s in caller-execution order —
/// `SetGlobalOption(history-limit)`, `SetGlobalOption(mouse)`,
/// `SetWindowGlobalOption(alternate-screen)` (#5151), `NewSession`.
/// `idempotent` and `command` are threaded straight into the
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
    alternate_screen: bool,
    idempotent: bool,
    command: Option<&str>,
) -> Vec<TmuxCommand> {
    let mut commands = scrollback_option_commands(history_limit, mouse, alternate_screen);
    commands.push(TmuxCommand::NewSession {
        name: name.to_string(),
        workdir: workdir.map(str::to_string),
        idempotent,
        command: command.map(str::to_string),
    });
    commands
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tmux_tests;
