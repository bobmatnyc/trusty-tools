//! Finding `git worktree add|remove|prune` in a Bash tool call (#8994 S2b).
//!
//! RED STUB: the tests in `bash_worktree_tests.rs` are written first.

use std::path::{Path, PathBuf};

/// One `git worktree` call the command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOp {
    /// `git worktree add <path>`: where the new tree may be, most likely first.
    Add(Vec<PathBuf>),
    /// `git worktree remove <path>`: where the removed tree may have been.
    Remove(Vec<PathBuf>),
    /// `git worktree prune` (not `-n` / `--dry-run`).
    Prune,
}

/// What [`parse_worktree_command`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommand {
    /// The `git worktree` calls, in command order.
    pub ops: Vec<WorktreeOp>,
    /// Calls that name `git worktree` but could not be resolved.
    pub errors: Vec<String>,
}

/// RED STUB.
pub fn parse_worktree_command(_command: &str, _cwd: Option<&Path>) -> ParsedCommand {
    ParsedCommand::default()
}

#[cfg(test)]
#[path = "bash_worktree_tests.rs"]
mod tests;
