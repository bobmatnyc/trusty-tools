//! Dual-root migration scan for managed-session workspaces (#1220 AC5).
//!
//! Why: #1220 moves the default managed-session workspace root from the OLD
//! `~/.trusty-mpm/workspaces/<project>/<id>/` layout to the NEW
//! `~/trusty-mpm-projects/<owner>/<repo>/<id>/` layout. This is a *migration*, not
//! a hard cutover: sessions provisioned before the change still live on disk under
//! the old root, and the daemon must keep discovering them alongside new-path
//! sessions during the transition. Without a dual-root scan a reinstall would
//! silently "lose" every pre-#1220 workspace from any filesystem-level discovery,
//! which is exactly the silently-incomplete migration RFC #1225 warned against.
//!
//! What: [`discovered_roots`] returns BOTH roots (old + new) that exist on disk;
//! [`scan_workspaces`] walks each root and returns the union of discovered
//! workspace directories as [`DiscoveredWorkspace`] records tagged with which root
//! (and therefore which layout era) they came from. The scan is filesystem-only
//! and read-only — it never moves or mutates anything — so it is safe to run on
//! every startup. The authoritative session state remains `sessions.json`
//! (see [`crate::session_manager::store`]); this scan is the migration-aware
//! *discovery* layer that ensures old-root workspaces are not orphaned.
//!
//! Test: `dual_root_scan_finds_both_eras`, `old_root_only`, `new_root_only`,
//! `absent_roots_yield_empty` in the `tests` module.

use std::path::{Path, PathBuf};

use crate::core::trusty_tools_config::{TrustyToolsConfig, workspace_root};

/// Old (pre-#1220) workspace root, relative to `$HOME`: `.trusty-mpm/workspaces`.
///
/// Why: the migration scan must know the legacy location to keep discovering
/// pre-#1220 sessions; a named constant keeps it from drifting.
/// What: the two path segments under home that formed the old root.
/// Test: `dual_root_scan_finds_both_eras` (old leg).
pub const OLD_ROOT_SEGMENTS: [&str; 2] = [".trusty-mpm", "workspaces"];

/// Which layout era a discovered workspace root belongs to.
///
/// Why: callers (and operators reading logs) need to know whether a workspace was
/// found under the legacy or the current layout — e.g. to prioritise migrating the
/// old ones, or to surface a "N legacy workspaces still present" hint.
/// What: `Old` = `~/.trusty-mpm/workspaces/…`; `New` = the resolved
/// `~/trusty-mpm-projects/…` (or its configured/env override).
/// Test: asserted by `dual_root_scan_finds_both_eras`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceEra {
    /// Legacy pre-#1220 root (`~/.trusty-mpm/workspaces/`).
    Old,
    /// Current #1220 root (`~/trusty-mpm-projects/` or override).
    New,
}

/// A workspace directory discovered by the migration scan.
///
/// Why: discovery returns both the path and its era so callers can list, migrate,
/// or report legacy vs. current workspaces without re-deriving which root a path
/// came from.
/// What: the absolute `path` of the workspace leaf directory and its
/// [`WorkspaceEra`].
/// Test: `dual_root_scan_finds_both_eras`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorkspace {
    /// Absolute path to the discovered workspace directory.
    pub path: PathBuf,
    /// Which layout era (old vs. new root) this workspace was found under.
    pub era: WorkspaceEra,
}

/// Resolve the absolute old (legacy) workspace root under an explicit home.
///
/// Why: the hermetic core for the old-root leg; tests point `home` at a temp dir.
/// What: `<home>/.trusty-mpm/workspaces`.
/// Test: used by every scan test.
pub fn old_root_at(home: &Path) -> PathBuf {
    let mut p = home.to_path_buf();
    for seg in OLD_ROOT_SEGMENTS {
        p.push(seg);
    }
    p
}

/// Return both workspace roots (old + new), regardless of existence.
///
/// Why: callers want the canonical pair to scan; deriving them in one place keeps
/// the old/new resolution consistent with [`workspace_root`].
/// What: `(old_root, new_root)` where `new_root` honours the env/config override
/// (so a configured root is scanned too, not just the built-in default). `home` is
/// the base for the old root; the new root uses [`workspace_root`] which resolves
/// home itself. Returns `None` for the old root only when `home` is `None`.
/// Test: `dual_root_scan_finds_both_eras`.
pub fn discovered_roots(
    config: &TrustyToolsConfig,
    home: Option<&Path>,
) -> (Option<PathBuf>, PathBuf) {
    let old = home.map(old_root_at);
    let new = workspace_root(config);
    (old, new)
}

/// List the immediate child directories of `root`, recursing one extra level.
///
/// Why: both layouts group session leaf dirs under a project segment
/// (`<project>/<id>` for old, `<owner>/<repo>/<id>` for new); a depth-limited walk
/// finds the actual workspace leaf dirs without an unbounded recursion. We collect
/// the deepest existing directory level under each project grouping.
/// What: for the old root, returns `<root>/<project>/<id>` dirs; for the new root,
/// returns `<root>/<owner>/<repo>/<id>` dirs. Implemented as a bounded walk that
/// descends through project/owner/repo grouping dirs to their child dirs. Any I/O
/// error on a directory is skipped (best-effort discovery, never fatal).
/// Test: `dual_root_scan_finds_both_eras`.
fn leaf_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    fn child_dirs(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.push(path);
                }
            }
        }
        out
    }

    // Descend `depth` grouping levels, collecting the directories at the final
    // level. `depth == 1` → immediate children; `depth == 2` → grandchildren.
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for dir in &frontier {
            next.extend(child_dirs(dir));
        }
        frontier = next;
    }
    frontier
}

/// Return true if `path` carries the `.trusty-mpm-worktree` sentinel marker.
///
/// Why: #3382's aggravator — this scanner walks the new root at a fixed depth
/// with no content validation, so any directory that happens to match the
/// `<owner>/<repo>/<id>` shape (e.g. a leaked mktemp test scaffold with the
/// same nesting — the observed litter, `<tmp>/origin/.base`, matches it
/// exactly) would read as a "discovered workspace" if this scanner is ever
/// wired to a caller. Requiring the sentinel closes that gap: it is written
/// by `WorkspaceProvisioner` at the root of every SM-created workspace (see
/// `provisioner::workspace::finalize_worktree` /
/// `session_manager::decommission::WORKTREE_SENTINEL_FILE`), so a directory
/// missing it was never provisioned by trusty-mpm regardless of how closely
/// its shape matches.
/// What: `path.join(WORKTREE_SENTINEL_FILE).is_file()`.
/// Test: `new_root_leaf_without_marker_is_excluded`.
fn has_worktree_marker(path: &Path) -> bool {
    path.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE)
        .is_file()
}

/// Scan BOTH workspace roots and return the union of discovered workspaces.
///
/// Why: the heart of AC5 — guarantee that sessions under the OLD
/// `~/.trusty-mpm/workspaces/` root are discovered ALONGSIDE new-path
/// (`~/trusty-mpm-projects/<owner>/<repo>`) sessions during the transition, so the
/// migration is never silently incomplete.
/// What: resolves both roots via [`discovered_roots`], walks the old root at the
/// `<project>/<id>` depth (2) and the new root at the `<owner>/<repo>/<id>` depth
/// (3), tags each discovered leaf with its [`WorkspaceEra`], and returns the
/// combined list. New-root leaves are additionally required to carry the
/// `.trusty-mpm-worktree` sentinel (see [`has_worktree_marker`], #3382) before
/// being reported — old-root leaves are NOT marker-gated, deliberately: the
/// sentinel postdates the pre-#1220 layout, and old-root discovery exists
/// specifically so pre-sentinel sessions are never silently lost (this
/// module's core Why). Missing roots contribute nothing (no error). Read-only.
/// Test: `dual_root_scan_finds_both_eras`, `old_root_only`, `new_root_only`,
/// `absent_roots_yield_empty`, `new_root_leaf_without_marker_is_excluded`.
pub fn scan_workspaces(
    config: &TrustyToolsConfig,
    home: Option<&Path>,
) -> Vec<DiscoveredWorkspace> {
    let (old_root, new_root) = discovered_roots(config, home);
    let mut found = Vec::new();

    if let Some(old) = old_root.filter(|p| p.is_dir()) {
        for path in leaf_dirs(&old, 2) {
            found.push(DiscoveredWorkspace {
                path,
                era: WorkspaceEra::Old,
            });
        }
    }

    if new_root.is_dir() {
        for path in leaf_dirs(&new_root, 3) {
            if !has_worktree_marker(&path) {
                continue;
            }
            found.push(DiscoveredWorkspace {
                path,
                era: WorkspaceEra::New,
            });
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create `dir/a/b/.../leaf` and return the leaf path.
    fn mkdirs(base: &Path, segments: &[&str]) -> PathBuf {
        let mut p = base.to_path_buf();
        for s in segments {
            p.push(s);
        }
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Write the `.trusty-mpm-worktree` sentinel into `leaf`, mirroring what
    /// `WorkspaceProvisioner` writes into every real new-root workspace.
    fn write_marker(leaf: &Path) {
        std::fs::write(
            leaf.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE),
            b"",
        )
        .unwrap();
    }

    /// A config whose NEW root is an explicit path inside the temp home, so the
    /// scan is hermetic and never touches the real `~/trusty-mpm-projects`.
    fn config_with_new_root(new_root: &Path) -> TrustyToolsConfig {
        TrustyToolsConfig {
            workspace_root_template: Some(new_root.to_string_lossy().to_string()),
            ..Default::default()
        }
    }

    /// Ensure the env override is unset so the config template (not a stray env
    /// var from another test) decides the new root. The returned guard serialises
    /// these env-sensitive tests against each other.
    fn clear_env() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised by the LOCK guard held for the test's duration.
        unsafe { std::env::remove_var(super::super::trusty_tools_config::WORKSPACE_ROOT_ENV) };
        guard
    }

    /// Why: the core AC5 guarantee — a session under the OLD root AND a session
    /// under the NEW root must BOTH be discovered, each tagged with its era.
    /// Test: itself.
    #[test]
    fn dual_root_scan_finds_both_eras() {
        let _g = clear_env();
        let home = tempfile::TempDir::new().unwrap();
        let new_root_dir = tempfile::TempDir::new().unwrap();
        let cfg = config_with_new_root(new_root_dir.path());

        // Old layout: ~/.trusty-mpm/workspaces/<project>/<id>/
        let old_leaf = mkdirs(
            home.path(),
            &[".trusty-mpm", "workspaces", "trusty-tools", "sess-old-1"],
        );
        // New layout: <new_root>/<owner>/<repo>/<id>/
        let new_leaf = mkdirs(
            new_root_dir.path(),
            &["bobmatnyc", "trusty-tools", "sess-new-1"],
        );
        write_marker(&new_leaf);

        let found = scan_workspaces(&cfg, Some(home.path()));
        let paths: Vec<&PathBuf> = found.iter().map(|w| &w.path).collect();

        assert!(
            paths.contains(&&old_leaf),
            "old-root workspace must be discovered: {found:?}"
        );
        assert!(
            paths.contains(&&new_leaf),
            "new-root workspace must be discovered: {found:?}"
        );

        let old_era = found.iter().find(|w| w.path == old_leaf).unwrap().era;
        let new_era = found.iter().find(|w| w.path == new_leaf).unwrap().era;
        assert_eq!(old_era, WorkspaceEra::Old);
        assert_eq!(new_era, WorkspaceEra::New);
    }

    /// Why: when only legacy workspaces exist, they must still be discovered (the
    /// reinstall-doesn't-lose-old-sessions case).
    /// Test: itself.
    #[test]
    fn old_root_only() {
        let _g = clear_env();
        let home = tempfile::TempDir::new().unwrap();
        let new_root_dir = tempfile::TempDir::new().unwrap(); // exists but empty
        let cfg = config_with_new_root(new_root_dir.path());
        let old_leaf = mkdirs(
            home.path(),
            &[".trusty-mpm", "workspaces", "proj", "sess-1"],
        );

        let found = scan_workspaces(&cfg, Some(home.path()));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, old_leaf);
        assert_eq!(found[0].era, WorkspaceEra::Old);
    }

    /// Why: post-migration (only new-layout sessions) must work symmetrically.
    /// Test: itself.
    #[test]
    fn new_root_only() {
        let _g = clear_env();
        let home = tempfile::TempDir::new().unwrap(); // no old root created
        let new_root_dir = tempfile::TempDir::new().unwrap();
        let cfg = config_with_new_root(new_root_dir.path());
        let new_leaf = mkdirs(new_root_dir.path(), &["acme", "widget", "sess-9"]);
        write_marker(&new_leaf);

        let found = scan_workspaces(&cfg, Some(home.path()));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, new_leaf);
        assert_eq!(found[0].era, WorkspaceEra::New);
    }

    /// Why: #3382's aggravator — a new-root leaf whose shape matches
    /// `<owner>/<repo>/<id>` but lacks the `.trusty-mpm-worktree` sentinel
    /// (e.g. leaked mktemp test litter) must NOT be reported as a discovered
    /// workspace.
    /// Test: itself.
    #[test]
    fn new_root_leaf_without_marker_is_excluded() {
        let _g = clear_env();
        let home = tempfile::TempDir::new().unwrap();
        let new_root_dir = tempfile::TempDir::new().unwrap();
        let cfg = config_with_new_root(new_root_dir.path());
        // Same shape as a real workspace leaf, but no sentinel written —
        // simulates leaked litter (#3382) that happens to match the depth.
        mkdirs(new_root_dir.path(), &["tmpXXXXXX", "origin", ".base"]);

        let found = scan_workspaces(&cfg, Some(home.path()));
        assert!(found.is_empty(), "{found:?}");
    }

    /// Why: absent roots must yield an empty result, never an error or panic.
    /// Test: itself.
    #[test]
    fn absent_roots_yield_empty() {
        let _g = clear_env();
        let home = tempfile::TempDir::new().unwrap();
        // Point the new root at a non-existent path.
        let cfg = TrustyToolsConfig {
            workspace_root_template: Some(
                home.path()
                    .join("does-not-exist")
                    .to_string_lossy()
                    .to_string(),
            ),
            ..Default::default()
        };
        let found = scan_workspaces(&cfg, Some(home.path()));
        assert!(found.is_empty(), "{found:?}");
    }
}
