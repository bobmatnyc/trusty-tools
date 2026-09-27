//! Which registered worktrees one `prune-worktrees` invocation may touch (#8782).
//!
//! Why: `tm session prune-worktrees` enumerated every registered project's
//! worktrees, so a run typed inside one repository surveyed — and under
//! `--force` could reclaim — the worktrees of every project the daemon knows.
//! It also could not be exercised against one throwaway repository.
//! What: [`WorktreeScope`], an optional project root and an optional allowlist
//! of paths. A scanned worktree is in scope only when both admit it; the empty
//! scope ([`WorktreeScope::all`]) admits everything, which is the pre-#8782
//! daemon-global behaviour the automatic sweep and the MCP tool keep.
//! Test: `worktree_scope_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worktree_registry::ScannedWorktree;

/// The project and path bounds of one prune pass (#8782).
///
/// Why: a scope that fails to resolve must never widen to every project, so
/// both bounds are canonical paths fixed when the request is parsed.
/// What: `project` is the canonical checkout that owns the worktree registry;
/// `only` is the canonical allowlist a `--force` run carries — the set its own
/// preview listed.
/// Test: `a_project_scope_admits_only_that_projects_worktrees`,
/// `an_allowlist_admits_only_listed_paths`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeScope {
    project: Option<PathBuf>,
    only: Option<BTreeSet<PathBuf>>,
}

/// Resolve a path's symlinks, keeping the raw path when it does not resolve.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl WorktreeScope {
    /// The unbounded scope: every registered project, every path.
    pub fn all() -> Self {
        Self::default()
    }

    /// Build a scope from the prune route's request fields (#8782).
    ///
    /// Why: a named project root that does not resolve is a caller error. Treating
    /// it as "no scope" would turn a typo into a daemon-global sweep.
    /// What: canonicalizes `project_root` (an `Err` when it does not resolve) and
    /// every `only_paths` entry (kept raw when it does not resolve, so it can
    /// match nothing a scan reports).
    /// Test: `a_project_root_that_does_not_resolve_is_refused`.
    pub fn from_request(
        project_root: Option<&str>,
        only_paths: Option<&[String]>,
    ) -> Result<Self, String> {
        let project = match project_root {
            None => None,
            Some(raw) => Some(std::fs::canonicalize(raw).map_err(|e| {
                format!(
                    "project_root `{raw}` does not resolve ({e}) — refusing rather than \
                     widening the prune to every project (#8782)"
                )
            })?),
        };
        let only = only_paths.map(|paths| paths.iter().map(|p| canonical(Path::new(p))).collect());
        Ok(Self { project, only })
    }

    /// Whether `scanned` lies inside this scope.
    ///
    /// What: in the project bound when `project` is unset or equals either the
    /// registry root or the managed project directory the scan attributed; in
    /// the path bound when `only` is unset or lists the path.
    /// Test: `a_project_scope_admits_only_that_projects_worktrees`,
    /// `an_allowlist_admits_only_listed_paths`.
    pub(crate) fn admits(&self, scanned: &ScannedWorktree) -> bool {
        let in_project = self.project.as_ref().is_none_or(|p| {
            *p == canonical(&scanned.registry_root) || *p == canonical(&scanned.project)
        });
        let listed = self
            .only
            .as_ref()
            .is_none_or(|only| only.contains(&canonical(&scanned.path)));
        in_project && listed
    }

    /// The echo the prune route returns, so a client can tell a daemon that
    /// honoured the scope from one that predates it (#8782).
    ///
    /// What: `{ "project_root": <path or null>, "only_paths": <count or null> }`.
    /// Test: `the_scope_echo_names_the_project_and_the_allowlist_size`.
    pub fn echo(&self) -> serde_json::Value {
        serde_json::json!({
            "project_root": self.project.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "only_paths": self.only.as_ref().map(BTreeSet::len),
        })
    }
}

/// The checkout that owns `dir`'s worktree registry — the project a CLI
/// invocation from `dir` is scoped to (#8782).
///
/// Why: the CLI must name the same root the daemon's scan attributes a
/// worktree to, so it asks the same function rather than a second `git` query.
/// What: [`super::worktree_registry::registry_root_for`]; `None` outside a
/// repository.
/// Test: `project_root_for_a_linked_worktree_is_its_main_checkout`.
pub fn project_root_for(dir: &Path) -> Option<PathBuf> {
    super::worktree_registry::registry_root_for(dir)
}

#[cfg(test)]
#[path = "worktree_scope_tests.rs"]
mod worktree_scope_tests;
