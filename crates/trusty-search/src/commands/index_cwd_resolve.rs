//! CWD → index resolution for `trusty-search index-status` (no-arg form).
//!
//! Why: when the user runs `trusty-search index-status` from inside a project
//! directory, they expect to see the status of that project's index — the same
//! way `trusty-search index .` defaults to CWD. This module implements the
//! matching logic so the `index_status` handler stays focused on rendering.
//!
//! What: fetches the index list from the daemon, queries each index's
//! `root_path` via `search.index.status` over the socket (#9214), and returns
//! all indexes whose
//! `root_path` is an ANCESTOR OF or EQUAL TO the cwd.  Results are returned
//! sorted by `root_path` (shortest match first = most-root ancestor first).
//!
//! Test: `cwd_under_helper_*` unit tests below cover the matching predicate;
//! `index_cwd_resolve_tests.rs` drives the resolver against a mock socket.

use super::daemon_rpc::{registrations, resident_statuses};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use trusty_search::service::daemon_client::DaemonClient;

#[cfg(test)]
#[path = "index_cwd_resolve_tests.rs"]
mod socket_tests;

// ─── Public types ─────────────────────────────────────────────────────────────

/// One candidate index that covers the current working directory.
///
/// Why: bundles the id and the root_path so callers don't need to re-fetch.
/// What: returned by `resolve_cwd_indexes`; sorted by `root_path` length
/// ascending (broadest ancestor first).
/// Test: produced by `a_readable_registry_matches_the_covering_roots`.
#[derive(Debug, Clone)]
pub struct CwdMatch {
    /// Daemon-side index identifier (as `search.indexes.list` reports it).
    pub id: String,
    /// Registered `root_path` for the index.
    pub root_path: PathBuf,
    /// Full status JSON body (`search.index.status`).
    pub status_body: serde_json::Value,
}

// ─── Main resolver ────────────────────────────────────────────────────────────

/// Resolve all daemon indexes that "cover" the current working directory.
///
/// Why: a user invoking `trusty-search index-status` without an explicit id
/// expects to see the status of whichever index(es) own the project they are
/// working in — mirroring the convention used by `trusty-search index .`.
///
/// What: lists all indexes via `search.indexes.list`, queries each one's
/// `root_path` via `search.index.status`, and collects every index whose `root_path`
/// is an ancestor of (or equal to) `cwd`.  Results are sorted by `root_path`
/// ascending so that a broad repo root appears before a narrow sub-index.
///
/// Test: `a_readable_registry_matches_the_covering_roots`,
/// `an_unreadable_status_refuses_instead_of_matching_nothing`.
pub async fn resolve_cwd_indexes(client: &DaemonClient) -> Result<Vec<CwdMatch>> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    resolve_indexes_for_cwd(client, &cwd).await
}

/// Core resolver that takes an explicit `cwd` rather than reading
/// `std::env::current_dir()` — makes the function unit-testable without
/// manipulating the process's working directory.
///
/// Why: separating the env-read from the matching logic lets unit tests
/// pass synthetic index lists and synthetic cwd paths without side effects.
/// What: identical logic to `resolve_cwd_indexes` but receives `cwd` as a
/// parameter. #9214: a status that cannot be read refuses the whole lookup
/// rather than being skipped — the skipped index could own `cwd`, and the
/// caller would then report "no index registered" or show a narrower one.
/// Test: `an_unreadable_status_refuses_instead_of_matching_nothing`,
/// `a_readable_registry_matches_the_covering_roots`.
pub async fn resolve_indexes_for_cwd(client: &DaemonClient, cwd: &Path) -> Result<Vec<CwdMatch>> {
    let ids = registrations(client).await?.resident;
    // #9214: H1 — a failed lookup refuses; it used to `continue`.
    let read = resident_statuses(client, ids)
        .await
        .require_all("refusing to guess which index covers the current directory")?;

    let mut matches: Vec<CwdMatch> = Vec::new();
    for status in read {
        let canonical_root =
            std::fs::canonicalize(&status.root).unwrap_or_else(|_| status.root.clone());

        // Include this index if cwd is equal to root or is nested under it.
        if cwd_is_under(cwd, &canonical_root) {
            matches.push(CwdMatch {
                id: status.id,
                root_path: status.root,
                status_body: status.body,
            });
        }
    }

    // Sort deterministically: shortest root_path string first (broadest ancestor).
    matches.sort_by(|a, b| {
        let la = a.root_path.as_os_str().len();
        let lb = b.root_path.as_os_str().len();
        la.cmp(&lb).then_with(|| {
            a.root_path
                .to_string_lossy()
                .cmp(&b.root_path.to_string_lossy())
        })
    });

    Ok(matches)
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

/// Return `true` when `cwd` equals `root` or is a descendant of `root`.
///
/// Why: a single predicate keeps the matching logic out of the loop body and
/// easy to test in isolation.
/// What: calls `Path::starts_with` (true when `cwd == root` or `root` is a
/// proper prefix component of `cwd`).
/// Test: `cwd_under_helper_*` in this module's tests.
pub fn cwd_is_under(cwd: &Path, root: &Path) -> bool {
    cwd.starts_with(root)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── cwd_is_under helper ───────────────────────────────────────────────────

    /// Exact match: cwd == root must return true.
    ///
    /// Why: the most common case — user is at the project root.
    /// What: asserts `cwd_is_under("/proj", "/proj") == true`.
    /// Test: this test.
    #[test]
    fn cwd_under_helper_exact_match() {
        assert!(cwd_is_under(Path::new("/proj"), Path::new("/proj")));
    }

    /// Ancestor match: cwd inside root returns true.
    ///
    /// Why: engineers typically run status from a subdirectory.
    /// What: asserts `cwd_is_under("/proj/a/b", "/proj") == true`.
    /// Test: this test.
    #[test]
    fn cwd_under_helper_ancestor_match() {
        assert!(cwd_is_under(Path::new("/proj/a/b"), Path::new("/proj")));
    }

    /// Non-ancestor: cwd outside root returns false.
    ///
    /// Why: ensures sibling or unrelated directories are not included.
    /// What: asserts `cwd_is_under("/other", "/proj") == false`.
    /// Test: this test.
    #[test]
    fn cwd_under_helper_non_ancestor() {
        assert!(!cwd_is_under(Path::new("/other"), Path::new("/proj")));
    }

    /// Partial path component match should not succeed.
    ///
    /// Why: `starts_with` is component-aware, so "/projfoo" does NOT match root "/proj".
    /// What: asserts `cwd_is_under("/projfoo/bar", "/proj") == false`.
    /// Test: this test.
    #[test]
    fn cwd_under_helper_partial_component_no_match() {
        assert!(!cwd_is_under(Path::new("/projfoo/bar"), Path::new("/proj")));
    }

    // ── resolve_indexes_for_cwd logic ────────────────────────────────────────
    // The resolver itself is driven against a mock socket in
    // `index_cwd_resolve_tests.rs`; these pin the predicate and sort order.

    /// Build a synthetic CwdMatch list and assert sort order.
    ///
    /// Why: the caller relies on deterministic ordering (broadest ancestor first)
    /// to display multi-index output predictably.
    /// What: constructs two matches with different root depths and asserts the
    /// shallower root is first after sorting.
    /// Test: this test.
    #[test]
    fn sort_by_root_path_length_shortest_first() {
        let make_match = |root: &str| CwdMatch {
            id: root.to_string(),
            root_path: PathBuf::from(root),
            status_body: serde_json::json!({}),
        };
        let mut matches = [make_match("/project/sub"), make_match("/project")];
        matches.sort_by(|a, b| {
            let la = a.root_path.as_os_str().len();
            let lb = b.root_path.as_os_str().len();
            la.cmp(&lb).then_with(|| {
                a.root_path
                    .to_string_lossy()
                    .cmp(&b.root_path.to_string_lossy())
            })
        });
        assert_eq!(matches[0].root_path, PathBuf::from("/project"));
        assert_eq!(matches[1].root_path, PathBuf::from("/project/sub"));
    }

    /// No-match: when no index covers the cwd, the result is an empty vec.
    ///
    /// Why: the caller interprets an empty vec as "no index found" and emits
    /// a friendly error.
    /// What: applies `cwd_is_under` against a root that does not cover the
    /// test cwd and verifies nothing matches.
    /// Test: this test.
    #[test]
    fn no_match_when_cwd_outside_all_roots() {
        let cwd = Path::new("/home/user/other");
        let roots = ["/home/user/project", "/opt/work"];
        let matches: Vec<_> = roots
            .iter()
            .filter(|r| cwd_is_under(cwd, Path::new(r)))
            .collect();
        assert!(matches.is_empty());
    }

    /// Multiple-match: several ancestor roots all cover the cwd.
    ///
    /// Why: polyrepo setups may register both a broad workspace root and a
    /// narrower package sub-root; both should appear.
    /// What: applies `cwd_is_under` against two roots that both cover the cwd.
    /// Test: this test.
    #[test]
    fn multiple_roots_covering_cwd() {
        let cwd = Path::new("/ws/pkg/src/main.rs");
        let roots = ["/ws", "/ws/pkg", "/other"];
        let matches: Vec<_> = roots
            .iter()
            .filter(|r| cwd_is_under(cwd, Path::new(r)))
            .collect();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|r| **r == "/ws"));
        assert!(matches.iter().any(|r| **r == "/ws/pkg"));
    }
}
