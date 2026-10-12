//! Error types for tmux access and the guarded send path.

use crate::input::InputState;

/// A tmux call that could not run, or ran and failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TmuxError {
    /// `run` was given no arguments, so there is no tmux verb to run.
    #[error("tmux: no verb given")]
    EmptyCommand,
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
