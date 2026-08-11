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
/// What: the BUILT-IN set of path-component names discovery must not descend
/// into or register. #5204 made the worktree base configurable, so this is no
/// longer the whole answer — resolve [`ephemeral_dir_names`] and ask it instead
/// of matching against this constant directly.
/// Test: `is_ephemeral_dir_name_*` and the discovery skip tests.
pub const EPHEMERAL_DIR_NAMES: &[&str] = &[".worktrees"];

/// Resolve the ephemeral-directory matcher once, for a whole scan.
///
/// Why (#5204): trusty-mpm's worktree base is configurable, and if a retargeted
/// base is not threaded here, every session worktree under it gets indexed as
/// duplicate content — the exact `fseventsd` churn this exclusion exists to
/// stop. Resolving ONCE per directory scanned (rather than per entry) keeps the
/// config read off the hot path.
/// What: a [`trusty_common::workspace_layout::WorktreeDirNames`] whose `matches`
/// accepts the configured base OR the built-in `.worktrees` — a superset, so a
/// retarget never starts indexing worktrees that were already on disk.
/// Test: `is_ephemeral_dir_name_matches_worktrees_only`,
/// `configured_worktree_base_is_treated_as_ephemeral`.
pub fn ephemeral_dir_names() -> trusty_common::workspace_layout::WorktreeDirNames {
    trusty_common::workspace_layout::WorktreeDirNames::resolve()
}

/// True iff `name` is an ephemeral directory component that auto-discovery must
/// skip, resolving the configured base on every call.
///
/// Prefer [`ephemeral_dir_names`] inside a loop — this re-reads the config each
/// time. See [`EPHEMERAL_DIR_NAMES`].
pub fn is_ephemeral_dir_name(name: &str) -> bool {
    ephemeral_dir_names().matches(name)
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

    /// Why (#5204): if a CONFIGURED worktree base is not excluded here, every
    /// session worktree under it is auto-registered and FSEvents-watched —
    /// reintroducing the churn this exclusion exists to prevent.
    /// What: a matcher built from a configured base excludes that base AND the
    /// built-in `.worktrees`, and still admits ordinary project directories.
    /// Test: this test.
    #[test]
    fn configured_worktree_base_is_treated_as_ephemeral() {
        let names =
            trusty_common::workspace_layout::WorktreeDirNames::from_configured(Some(".sessions"));
        assert!(
            names.matches(".sessions"),
            "configured base must be skipped"
        );
        assert!(
            names.matches(".worktrees"),
            "worktrees already on disk before the retarget must stay skipped"
        );
        assert!(!names.matches("my-project"));
    }
}
