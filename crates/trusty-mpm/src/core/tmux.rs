//! tmux session-control primitives.
//!
//! Why: trusty-mpm hosts each Claude Code session inside a named tmux session
//! (the primary control model — see `docs/research/session-control-models.md`).
//! The patterns here are distilled from `ai-commander`'s `commander-tmux` crate
//! and `open-mpm`'s `tm` module: create named detached sessions, send keystrokes
//! with `send-keys`, and capture pane output. Keeping the command-builder logic
//! in `core` (pure, no process spawning) makes it unit-testable; the daemon
//! owns the actual `std::process::Command` execution.
//! What: `TmuxTarget` (session\[:pane\] addressing), `TmuxCommand` (a typed tmux
//! sub-command), and `tmux_argv` (renders a command to an argv vector).
//! Test: `cargo test -p trusty-mpm-core` asserts the rendered argv for each
//! command shape, including literal vs. key-name `send-keys`.

use serde::{Deserialize, Serialize};

/// Addresses a tmux session, optionally a specific pane within it.
///
/// Why: every tmux I/O command needs a `-t` target; modelling it once avoids
/// re-deriving the `session:pane` string at each call site.
/// What: a session name plus an optional pane id (`%0`, `%1`, ...).
/// Test: `target_renders_session_and_pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxTarget {
    /// tmux session name (the daemon names these `trusty-mpm-<session-id>`).
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

    /// Render the tmux `-t` target string (`session` or `session:pane`).
    pub fn as_target(&self) -> String {
        match &self.pane {
            Some(p) => format!("{}:{}", self.session, p),
            None => self.session.clone(),
        }
    }
}

/// A typed tmux sub-command the daemon's session manager can execute.
///
/// Why: enumerating the small set of tmux operations trusty-mpm needs
/// (vs. building ad-hoc argv vectors) keeps the daemon's tmux usage auditable
/// and the argv rendering testable without spawning processes.
/// What: covers session lifecycle, keystroke injection, and output capture.
/// Test: see the per-variant tests in this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TmuxCommand {
    /// `new-session -A -d -s <name> [-c <dir>]` — create a detached session,
    /// or attach to it if a session with the same name already exists (the
    /// `-A` flag makes the command idempotent rather than failing on a
    /// duplicate session name).
    NewSession {
        /// Session name.
        name: String,
        /// Optional working directory for the session's first pane.
        workdir: Option<String>,
    },
    /// `kill-session -t <name>` — destroy a session.
    KillSession {
        /// Session name to kill.
        name: String,
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
    /// `list-panes -t <name> -F <fmt>` — enumerate a session's panes.
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
        /// When true, pass `-l` so tmux sends the text literally rather than
        /// interpreting words like `Enter`/`C-c` as key names.
        literal: bool,
    },
    /// `capture-pane -t <target> -p [-S -<lines>]` — capture pane output.
    CapturePane {
        /// Target session/pane.
        target: TmuxTarget,
        /// Optional number of trailing scrollback lines to capture.
        lines: Option<u32>,
    },
}

/// tmux `-F` format string for `list-sessions`.
///
/// Why: a single canonical format keeps the parser in the daemon aligned with
/// the command emitted here.
/// What: name, creation epoch, attached flag — colon-separated.
pub const SESSION_LIST_FORMAT: &str = "#{session_name}:#{session_created}:#{session_attached}";

/// tmux `-F` format string for `list-windows` (`index:name`).
pub const WINDOW_LIST_FORMAT: &str = "#{window_index}:#{window_name}";

/// tmux `-F` format string for `list-panes` (`pane_id:active`).
pub const PANE_LIST_FORMAT: &str = "#{pane_id}:#{pane_active}";

/// Render a [`TmuxCommand`] into an argv vector suitable for `Command::args`.
///
/// Why: separating argv construction from process spawning makes the command
/// logic pure and unit-testable; the daemon just executes `tmux` with the
/// returned argv.
/// What: returns the argument list (excluding the `tmux` program name itself).
/// Test: `new_session_argv`, `send_keys_literal_argv`, `capture_argv`, etc.
pub fn tmux_argv(cmd: &TmuxCommand) -> Vec<String> {
    match cmd {
        TmuxCommand::NewSession { name, workdir } => {
            // `-A` attaches to an existing session of the same name instead
            // of failing with "duplicate session"; combined with `-d` it
            // stays detached, making session creation idempotent.
            let mut argv = vec![
                "new-session".to_string(),
                "-A".to_string(),
                "-d".to_string(),
                "-s".to_string(),
                name.clone(),
            ];
            if let Some(dir) = workdir {
                argv.push("-c".to_string());
                argv.push(dir.clone());
            }
            argv
        }
        TmuxCommand::KillSession { name } => {
            vec!["kill-session".into(), "-t".into(), name.clone()]
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_renders_session_and_pane() {
        assert_eq!(TmuxTarget::session("s").as_target(), "s");
        assert_eq!(TmuxTarget::pane("s", "%2").as_target(), "s:%2");
    }

    #[test]
    fn new_session_argv() {
        let argv = tmux_argv(&TmuxCommand::NewSession {
            name: "trusty-mpm-1".into(),
            workdir: Some("/tmp/proj".into()),
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
    fn new_session_argv_without_workdir() {
        let argv = tmux_argv(&TmuxCommand::NewSession {
            name: "s".into(),
            workdir: None,
        });
        assert_eq!(argv, ["new-session", "-A", "-d", "-s", "s"]);
    }

    #[test]
    fn send_keys_literal_argv() {
        // Literal text: -l is present so tmux does not interpret words as keys.
        let argv = tmux_argv(&TmuxCommand::SendKeys {
            target: TmuxTarget::session("s"),
            keys: "claude --help".into(),
            literal: true,
        });
        assert_eq!(argv, ["send-keys", "-t", "s", "-l", "claude --help"]);
    }

    #[test]
    fn send_keys_keyname_argv() {
        // Non-literal: used to send key names like `Enter` or `C-c`.
        let argv = tmux_argv(&TmuxCommand::SendKeys {
            target: TmuxTarget::pane("s", "%1"),
            keys: "Enter".into(),
            literal: false,
        });
        assert_eq!(argv, ["send-keys", "-t", "s:%1", "Enter"]);
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
        assert_eq!(argv, ["list-panes", "-t", "work", "-F", PANE_LIST_FORMAT]);
    }
}
