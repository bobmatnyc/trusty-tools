//! Runtime adapter trait and concrete implementations.
//!
//! Why: the session manager must be able to swap different runtime backends
//! (MVP: Claude Code CLI; future: trusty-code) without changing its own code.
//! A trait seam here means future adapters slot in without touching the manager.
//! What: defines [`RuntimeAdapter`] trait, [`RuntimeError`] error type, and
//! re-exports [`ClaudeCodeAdapter`] as the only MVP implementation.
//! Test: each adapter carries its own unit tests; `ClaudeCodeAdapter` is
//! testable without a real tmux binary via [`FakeTmuxDriver`].

mod claude_code;

pub use claude_code::ClaudeCodeAdapter;

use std::path::Path;
use thiserror::Error;

/// Errors produced by a runtime adapter during spawning.
///
/// Why: callers (HTTP handlers, the session manager) need structured errors
/// to map to the right HTTP status and log message.
/// What: one variant per failure class: spawn command failed, tmux unavailable,
/// or required binary not found on PATH.
/// Test: exercised by `ClaudeCodeAdapter` unit tests.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The spawn command could not be executed in the target pane.
    #[error("spawn failed: {0}")]
    Spawn(String),

    /// tmux was unavailable or a tmux operation failed.
    #[error("tmux unavailable: {0}")]
    TmuxUnavailable(String),

    /// A required binary (e.g. `claude`) was not found on PATH.
    #[error("binary not found: {0}")]
    BinaryNotFound(String),
}

/// Trait for launching an agent runtime inside a named tmux session.
///
/// Why: the session manager calls `spawn` without knowing or caring which
/// runtime backend is in use; the trait is the contract that makes that work.
/// What: one `spawn` method that takes the tmux session name, working directory,
/// and task description and starts the runtime inside the pane. `identify`
/// returns a human-readable backend name for logging.
/// Test: `ClaudeCodeAdapter` is the only MVP implementor; a `FakeTmuxDriver`
/// can substitute the tmux layer for unit tests.
pub trait RuntimeAdapter: Send + Sync {
    /// Start the runtime inside the already-created tmux session `tmux_name`.
    ///
    /// Why: the session manager creates the tmux session first, then calls
    /// `spawn` so the adapter can send the start command into the pane.
    /// What: sends the appropriate shell command(s) to start the runtime in the
    /// named tmux pane; returns `RuntimeError` on any failure.
    /// Test: `claude_code_adapter_spawn_sends_env_scrub_command`.
    fn spawn(&self, tmux_name: &str, cwd: &Path, task: &str) -> Result<(), RuntimeError>;

    /// Return a short human-readable name for this runtime backend.
    ///
    /// Why: logs and status responses need to identify which runtime is in use
    /// so operators can distinguish `claude-code` from `tcode` sessions.
    /// What: returns a static string like `"claude-code"` or `"tcode"`.
    /// Test: `claude_code_adapter_identifies`.
    fn identify(&self) -> &str;
}
