//! Service-wide constants.
//!
//! Why: avoid magic numbers scattered across CLI args, port-discovery
//! fallbacks, and UI runtime config injection. A single home for these
//! values makes future port changes a one-line edit.
//! What: currently exports [`DEFAULT_PORT`], the loopback port the daemon
//! binds when no explicit `--port` / `port.lock` override is in play.
//! Test: integration tests start the daemon on auto-selected ports; this
//! constant is exercised indirectly via `cli::Start::port` defaults and
//! `read_daemon_port` fallback.

/// Default loopback port for the trusty-search daemon.
///
/// Used as the CLI `--port` default, the fallback when
/// `~/Library/Application Support/trusty-search/port.lock` is missing or
/// unreadable, and the value injected into the embedded UI when
/// `SearchAppState::daemon_port` is `None`.
pub const DEFAULT_PORT: u16 = 7878;

/// Directory names that are ephemeral / session-scoped and must never be
/// *auto*-registered as indexes.
///
/// Why: MPM creates a throwaway git worktree per session under
/// `.worktrees/<uuid>/` and deletes it when the session ends. Auto-discovery
/// and the colocated rescan would otherwise register (and FSEvents-watch) each
/// one, and every removed worktree then leaks a dead registration — the churn
/// that pinned `fseventsd` at 8 GB. Skipping the `.worktrees` component during
/// *discovery* (component-level, so nested `.base/.worktrees/*` is covered too)
/// stops the churn at the source. This only gates automatic discovery; the hard
/// denylist is untouched, so an explicit `trusty-search index <worktree>` still
/// works when a caller genuinely wants a worktree indexed.
/// What: the set of path-component names discovery must not descend into or
/// register, checked via [`is_ephemeral_dir_name`].
/// Test: `is_ephemeral_dir_name_*` and the discovery skip tests.
pub const EPHEMERAL_DIR_NAMES: &[&str] = &[".worktrees"];

/// True iff `name` is an ephemeral directory component that auto-discovery must
/// skip. See [`EPHEMERAL_DIR_NAMES`].
pub fn is_ephemeral_dir_name(name: &str) -> bool {
    EPHEMERAL_DIR_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the discovery skip keys on an exact component match — `.worktrees`
    /// is ephemeral, but an ordinary project must not be misclassified.
    /// What: asserts the positive and negative cases.
    /// Test: this test.
    #[test]
    fn is_ephemeral_dir_name_matches_worktrees_only() {
        assert!(is_ephemeral_dir_name(".worktrees"));
        assert!(!is_ephemeral_dir_name("worktrees"));
        assert!(!is_ephemeral_dir_name("my-project"));
        assert!(!is_ephemeral_dir_name(".git"));
    }
}
