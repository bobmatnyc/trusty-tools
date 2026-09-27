//! Which registered worktrees one `prune-worktrees` invocation may touch (#8782).
//!
//! Why: `tm session prune-worktrees` enumerated every registered project's
//! worktrees, so a run typed inside one repository surveyed — and under
//! `--force` could reclaim — the worktrees of every project the daemon knows.
//! It also could not be exercised against one throwaway repository.
//! What: [`WorktreeScope`], an optional project root and an optional allowlist
//! of paths. A scanned worktree is in scope only when both admit it; the empty
//! scope ([`WorktreeScope::all`]) admits everything, which is the pre-#8782
//! daemon-global behaviour the automatic sweep keeps. [`PruneScope`] is one
//! request's pair of them: one per pass, each with its own allowlist.
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
/// preview listed. `discard` is the subset whose unsaved work that preview
/// named; `--discard-dirty` removes no other dirty tree.
/// Test: `a_project_scope_admits_only_that_projects_worktrees`,
/// `an_allowlist_admits_only_listed_paths`,
/// `a_tree_dirtied_after_a_clean_preview_is_not_discarded`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeScope {
    project: Option<PathBuf>,
    only: Option<BTreeSet<PathBuf>>,
    discard: Option<BTreeSet<PathBuf>>,
}

/// Canonicalize each path of an optional allowlist (raw when it does not resolve).
fn canonical_set(paths: Option<&[String]>) -> Option<BTreeSet<PathBuf>> {
    paths.map(|paths| paths.iter().map(|p| canonical(Path::new(p))).collect())
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

    /// The scope of one project's checkout, every path in it (#8782).
    ///
    /// What: `Err` when `root` does not resolve — never the unbounded scope.
    /// Test: `a_pause_prunes_only_its_own_projects_orphans`.
    pub fn for_project(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            project: Some(std::fs::canonicalize(root)?),
            ..Self::default()
        })
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
        Ok(Self {
            project,
            only: canonical_set(only_paths),
            discard: None,
        })
    }

    /// Bound `--discard-dirty` to `paths`: the trees whose unsaved work the
    /// operator's preview named (#8782). `None` leaves it unbounded.
    pub fn with_discard_only(mut self, paths: Option<&[String]>) -> Self {
        self.discard = canonical_set(paths);
        self
    }

    /// Whether a removal may discard `path`'s unsaved work: `true` when no
    /// discard allowlist is set, else only when it lists `path` (#8782).
    /// Test: `a_tree_dirtied_after_a_clean_preview_is_not_discarded`.
    pub(crate) fn may_discard(&self, path: &Path) -> bool {
        self.discard
            .as_ref()
            .is_none_or(|d| d.contains(&canonical(path)))
    }

    /// Whether `scanned` lies inside this scope.
    ///
    /// What: in the project bound when `project` is unset or equals either the
    /// registry root or the managed project directory the scan attributed; in
    /// the path bound when `only` is unset or lists the path exactly.
    ///
    /// The two project arms are deliberately asymmetric for a walk project
    /// `<repo>` with a `.base` clone. Scoped to `<repo>`, the `project` arm
    /// admits the worktrees of BOTH registries, `<repo>` and `<repo>/.base`,
    /// because the scan attributes both to the managed directory `<repo>` — one
    /// project, two clones. Scoped to `<repo>/.base` (a run from inside the
    /// `.base` clone), only the `registry_root` arm can match, so it admits
    /// `.base`'s own worktrees and not `<repo>`'s. The narrower answer is the
    /// safe one: a scope never reaches a registry the caller did not run from,
    /// except the `.base` clone of the checkout the caller did run from.
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

    /// Whether the daemon's scan reaches this scope's project at all (#8782).
    ///
    /// Why: a scoped run from a checkout the daemon does not scan finds
    /// nothing, and `total: 0` alone reads as "nothing to prune".
    /// What: `true` for the unbounded scope. Otherwise `true` when the project
    /// is one of the scan's anchors — an adopted checkout, or a
    /// `<repos_root>/<owner>/<repo>` directory or its `.base` clone. Reads
    /// directories only; spawns no `git`.
    /// Test: `project_known_is_false_for_a_checkout_the_daemon_does_not_scan`.
    pub fn project_known(&self, repos_root: &Path, adopted: &[PathBuf]) -> bool {
        let Some(project) = self.project.as_ref() else {
            return true;
        };
        if adopted.iter().any(|a| canonical(a) == *project) {
            return true;
        }
        let Ok(owners) = std::fs::read_dir(repos_root) else {
            return false;
        };
        owners
            .flatten()
            .filter_map(|owner| std::fs::read_dir(owner.path()).ok())
            .flat_map(|repos| repos.flatten())
            .map(|repo| canonical(&repo.path()))
            .any(|dir| {
                dir == *project
                    || dir.join(super::worktree_registry::BASE_CLONE_DIRNAME) == *project
            })
    }

    /// The canonical project root, when the scope names one.
    fn project_echo(&self) -> Option<String> {
        self.project
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// One prune request's scope: the same project bound for both passes, and a
/// separate allowlist for each (#8782).
///
/// Why: a `--force` run hands back what its preview listed. One shared
/// allowlist let the merged-PR pass remove a path the preview listed only as an
/// orphan, and the reverse; one per pass bounds each pass by its own rows.
/// What: `orphan` bounds the orphan sweep, `merged` the merged-PR pass.
/// Test: `a_per_pass_allowlist_bounds_each_pass_by_its_own_rows`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneScope {
    /// The orphan sweep's bounds.
    pub orphan: WorktreeScope,
    /// The merged-PR pass's bounds.
    pub merged: WorktreeScope,
}

impl PruneScope {
    /// Build both scopes from the route's request fields; see
    /// [`WorktreeScope::from_request`] for the refusal rule. The discard
    /// allowlist bounds the orphan pass only: the merged-PR pass never
    /// discards unsaved work.
    ///
    /// Test: `a_project_root_that_does_not_resolve_is_refused`.
    pub fn from_request(
        project_root: Option<&str>,
        only_orphan_paths: Option<&[String]>,
        only_merged_paths: Option<&[String]>,
        only_discard_paths: Option<&[String]>,
    ) -> Result<Self, String> {
        Ok(Self {
            orphan: WorktreeScope::from_request(project_root, only_orphan_paths)?
                .with_discard_only(only_discard_paths),
            merged: WorktreeScope::from_request(project_root, only_merged_paths)?,
        })
    }

    /// The echo the prune route returns, so a client can tell a daemon that
    /// honoured the scope from one that predates it (#8782).
    ///
    /// What: `project_root` (path or null), `project_known` (see
    /// [`WorktreeScope::project_known`]), and each allowlist's size or null —
    /// always present as keys, so a client can refuse a reply that lacks one.
    /// Test: `the_scope_echo_names_the_project_and_the_allowlist_size`.
    pub fn echo(&self, project_known: bool) -> serde_json::Value {
        serde_json::json!({
            "project_root": self.orphan.project_echo(),
            "project_known": project_known,
            "only_orphan_paths": self.orphan.only.as_ref().map(BTreeSet::len),
            "only_merged_paths": self.merged.only.as_ref().map(BTreeSet::len),
            "only_discard_paths": self.orphan.discard.as_ref().map(BTreeSet::len),
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
