//! Error types for tmux operations.
//!
//! Why: Centralizes error handling for the tmux subprocess wrapper so callers
//! can match on specific failure modes (missing binary, missing session, etc.)
//! instead of scraping stderr strings.
//! What: Defines `TmuxError` enum and `Result<T>` alias used throughout the
//! `tmux` module.
//! Test: Construct each variant and verify Display/From implementations work.

use thiserror::Error;

/// Errors that can occur during tmux operations.
#[derive(Error, Debug)]
pub enum TmuxError {
    /// tmux not found in PATH.
    #[error("tmux not found in PATH")]
    NotFound,

    /// Session not found.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// Pane not found in session.
    #[error("pane not found: {0} in session {1}")]
    PaneNotFound(String, String),

    /// tmux command failed.
    #[error("tmux command failed: {0}")]
    CommandFailed(String),

    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Failed to parse tmux output.
    #[error("parse error: {0}")]
    ParseError(String),
}

/// Result type alias for tmux operations.
pub type Result<T> = std::result::Result<T, TmuxError>;
