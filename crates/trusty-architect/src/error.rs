//! Error types for tmux access and the guarded send path.

use crate::input::InputState;

/// A tmux call that could not run, or ran and failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TmuxError {
    /// `run` was given no arguments, so there is no tmux verb to run.
    #[error("tmux: no verb given")]
    EmptyCommand,
    /// `Tmux::run` was given the `send-keys` verb. Keys go out only through
    /// [`crate::send_guarded`], which checks the input box first.
    #[error("tmux send-keys is refused here; use send_guarded")]
    SendKeysRefused,
    /// The `tmux` process could not be started (not on `PATH`, no permission).
    #[error("tmux {verb}: could not start: {source}")]
    Spawn {
        /// The tmux verb, e.g. `send-keys`.
        verb: String,
        /// The underlying spawn error.
        #[source]
        source: std::io::Error,
    },
    /// tmux ran and exited non-zero.
    #[error("tmux {verb}: exit {code:?}: {stderr}")]
    Failed {
        /// The tmux verb, e.g. `send-keys`.
        verb: String,
        /// The exit code, or `None` when a signal ended the process.
        code: Option<i32>,
        /// tmux's stderr, trimmed.
        stderr: String,
    },
    /// tmux did not finish within the deadline; the child was killed and
    /// reaped (#9171 A3a).
    #[error("tmux {verb}: timed out after {after:?}")]
    Timeout {
        /// The tmux verb, e.g. `capture-pane`.
        verb: String,
        /// The deadline that passed.
        after: std::time::Duration,
    },
    /// Waiting on the tmux child failed; the child was killed and reaped.
    #[error("tmux {verb}: wait failed: {source}")]
    Wait {
        /// The tmux verb.
        verb: String,
        /// The underlying wait error.
        #[source]
        source: std::io::Error,
    },
    /// A pane target that is not tmux's immutable `%N` pane id.
    #[error("not a tmux pane id (want %N): {0:?}")]
    InvalidPaneId(String),
}

/// Why [`crate::send_guarded`] did not deliver keys.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    /// The input box did not accept input; nothing was sent.
    #[error("refused to send to {pane}: input box reads {state}")]
    Refused {
        /// The pane the keys were meant for.
        pane: String,
        /// The input-box state that blocked the send.
        state: InputState,
    },
    /// The box accepted input, but the tmux call failed.
    #[error(transparent)]
    Tmux(#[from] TmuxError),
}

/// Why [`crate::self_ctx::self_ctx_tokens`] could not measure a transcript.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SelfCtxError {
    /// The chosen transcript could not be read.
    #[error("read transcript {}: {source}", path.display())]
    Io {
        /// The transcript path.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A usage field (or `usage` itself) is not what the sum needs.
    #[error("transcript usage field {0:?} is not an integer")]
    BadUsage(String),
}

/// An environment value the poller cannot use (#9171 A3a).
///
/// The prototype re-reads these on every cycle. A bad `LOAD_FACTOR` fails the
/// whole host check there, so [`crate::poller::Config::from_env`] returns this
/// error for it. A bad `PANE_CTX_WINDOW`, `SELF_CTX_WINDOW` or
/// `SELF_CTX_THRESHOLD` is caught inside its own sensor there, so the port
/// keeps the config and lists the error in `Config::problems`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("environment {name}={value:?} is not a valid {want}")]
pub struct ConfigError {
    /// The variable name, e.g. `LOAD_FACTOR`.
    pub name: &'static str,
    /// The value found.
    pub value: String,
    /// What the variable must hold (`number`, `integer`).
    pub want: &'static str,
}

/// Why `poll-state.json` could not be read or written (#9171 A3a).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// The file could not be read, written, synced or renamed.
    #[error("state file {}: {source}", path.display())]
    Io {
        /// The path involved.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file holds text that is not a poller state object.
    #[error("state file {}: not a poller state: {source}", path.display())]
    Corrupt {
        /// The state file.
        path: std::path::PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
}
