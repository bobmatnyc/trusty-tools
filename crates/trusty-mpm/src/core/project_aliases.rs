//! Local project-path alias store — lightweight `alias → path` registry.
//!
//! Why: operators run `tm` across many git projects; recording where `tm` has
//! been run lets `tm ls` give a quick overview of known local project paths
//! without requiring a running daemon. The store is independent of the DOC-24
//! standalone fleet registry (which maps alias → GitHub URL) and the daemon's
//! project HTTP API.
//! What: [`ProjectAliasStore`] persists a `Vec<ProjectAliasEntry>` to
//! `<root>/project-paths.json`. [`try_register_project_alias`] is the single
//! non-fatal entry point for auto-registration on `tm` launch.
//! Test: `project_alias_store_new_is_empty`, `try_register_idempotent`,
//! `try_register_collision_keeps_existing`, `alias_from_path_basename`,
//! `alias_from_path_no_component`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ── Record type ──────────────────────────────────────────────────────────────

/// One entry in the local project-path alias store.
///
/// Why: a flat struct is the simplest representation; separate alias and path
/// fields map cleanly to the `tm ls` two-column display.
/// What: alias (basename-derived, stable key) plus the absolute git root path.
/// Test: round-tripped by `project_alias_store_new_is_empty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAliasEntry {
    /// Short human-readable alias derived from the git root directory basename.
    pub alias: String,
    /// Absolute path to the git repository root.
    pub path: PathBuf,
}

// ── Store ────────────────────────────────────────────────────────────────────

/// In-memory + on-disk registry of local project-path aliases.
///
/// Why: all auto-registration and listing goes through one struct so persistence
/// never drifts from the in-memory view.
/// What: a thin wrapper around `Vec<ProjectAliasEntry>` with a `file_path`
/// pointing to `<root>/project-paths.json`. Mutations are committed by explicit
/// `save()` calls so callers control the write order.
/// Test: `try_register_idempotent`, `try_register_collision_keeps_existing`.
#[derive(Debug, Default)]
pub struct ProjectAliasStore {
    entries: Vec<ProjectAliasEntry>,
    file_path: PathBuf,
}

impl ProjectAliasStore {
    /// Load (or create) the alias store at `<root>/project-paths.json`.
    ///
    /// Why: a missing file is treated as an empty store so first-run
    /// auto-registration succeeds without an explicit init step.
    /// What: reads and deserializes the JSON file when present; returns an
    /// empty store with the correct path when the file is absent.
    /// Test: `project_alias_store_new_is_empty`.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let file_path = root.join("project-paths.json");
        if !file_path.exists() {
            return Ok(Self {
                entries: Vec::new(),
                file_path,
            });
        }
        let raw = std::fs::read_to_string(&file_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file_path.display()))?;
        let entries: Vec<ProjectAliasEntry> = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", file_path.display()))?;
        Ok(Self { entries, file_path })
    }

    /// Persist the store to disk.
    ///
    /// Why: write only when there is a change so most `tm` invocations skip
    /// the write entirely.
    /// What: creates parent directories if absent, serialises to pretty JSON, and
    /// writes atomically via a rename.
    /// Test: `try_register_idempotent` (write+reload cycle).
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| anyhow::anyhow!("failed to serialize project-paths: {e}"))?;
        // Atomic write: write to .tmp then rename.
        let tmp_path = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp_path.display()))?;
        if let Err(e) = std::fs::rename(&tmp_path, &self.file_path) {
            // Best-effort cleanup: remove the leftover .json.tmp so it does not
            // accumulate across repeated rename failures.
            let _ = std::fs::remove_file(&tmp_path);
            return Err(anyhow::anyhow!(
                "failed to rename {} → {}: {e}",
                tmp_path.display(),
                self.file_path.display()
            ));
        }
        Ok(())
    }

    /// Attempt to register `alias → path`.
    ///
    /// Why: idempotency and collision safety must be centralised so every call
    /// site gets the same semantics without duplicating the logic.
    /// What: three outcomes:
    ///   • Identical entry already exists → no-op, returns `false`.
    ///   • Alias exists with a *different* path → keep existing, log warn
    ///     (visible at default log level so operator knows about the skip),
    ///     returns `false` (the caller should NOT save).
    ///   • Alias is new → insert, returns `true` (caller should save).
    /// Collision rule: keep existing. This is conservative — the first project
    /// with a given basename wins. A future `--force` flag could override.
    /// Test: `try_register_idempotent`, `try_register_collision_keeps_existing`.
    pub fn try_register(&mut self, alias: &str, path: &Path) -> bool {
        for entry in &self.entries {
            if entry.alias == alias {
                if entry.path == path {
                    debug!(
                        alias,
                        "project-alias: already registered (same path); no-op"
                    );
                } else {
                    warn!(
                        alias,
                        existing = %entry.path.display(),
                        incoming = %path.display(),
                        "project-alias: collision — '{}' already points to a different path; use `tm register` to override",
                        alias
                    );
                }
                return false;
            }
        }
        self.entries.push(ProjectAliasEntry {
            alias: alias.to_string(),
            path: path.to_path_buf(),
        });
        true
    }

    /// Return all entries sorted by alias name.
    ///
    /// Why: stable ordering makes `tm ls` output deterministic across runs.
    /// What: clones and sorts by alias (lexicographic).
    /// Test: `project_alias_store_list_is_sorted`.
    pub fn list(&self) -> Vec<ProjectAliasEntry> {
        let mut out = self.entries.clone();
        out.sort_by(|a, b| a.alias.cmp(&b.alias));
        out
    }
}

// ── Alias derivation ─────────────────────────────────────────────────────────

/// Derive a local-alias from a directory path.
///
/// Why: the auto-registration convention uses the basename as the alias so that
/// `~/Projects/trusty-tools` becomes `trusty-tools` without operator input.
/// What: returns the final path component as a UTF-8 string, falling back to
/// `"project"` for a path with no usable component (e.g. `/`).
/// Test: `alias_from_path_basename`, `alias_from_path_no_component`.
pub fn alias_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "project".to_string())
}

// ── Non-fatal public entry point ─────────────────────────────────────────────

/// Auto-register the git project root found from `cwd` into the alias store.
///
/// Why: `tm` should silently accumulate a map of visited git project roots so
/// `tm ls` gives a quick overview without operator effort. The operation is
/// fire-and-forget — it must never block or fail the calling command.
/// What: resolves the git root by walking ancestors looking for a `.git` entry;
/// if cwd is not inside a git repo, silently returns. Otherwise derives the
/// alias from the basename, loads the store from `store_root` (the same root
/// that `tm ls` reads from — callers resolve it once via `resolve_managed_paths`
/// so write and read always agree), calls `try_register`, and saves only when a
/// new entry was added.
/// Test: `try_register_and_load_round_trip_same_root` proves the write-then-read
/// path through the same configurable root.
pub fn try_register_project_alias(cwd: &Path, store_root: &Path) {
    if let Err(e) = try_register_project_alias_inner(cwd, store_root) {
        warn!("project-alias: auto-registration failed (non-fatal): {e}");
    }
}

/// Internal implementation that can return `Err`.
///
/// Why: wrapping the fallible logic lets `try_register_project_alias` present
/// a non-fatal interface while keeping the inner code idiomatic with `?`.
/// What: walks ancestors to find git root, derives alias, loads store from
/// `store_root`, registers, saves if needed.
/// Test: exercised via `try_register_project_alias` wrapper tests.
fn try_register_project_alias_inner(cwd: &Path, store_root: &Path) -> anyhow::Result<()> {
    // Resolve git root via ancestor walk. Not a git repo → skip silently.
    let Some(git_root) = find_git_root(cwd) else {
        debug!("project-alias: cwd is not inside a git repo; skipping");
        return Ok(());
    };

    // Skip git worktree checkouts — they pollute `tm ls` with ephemeral paths.
    // Primary: git-level check (git-dir ≠ common-dir → linked worktree).
    // Belt-and-suspenders: path-segment check for known MPM layouts.
    if git_dir_differs_from_common(&git_root) || is_worktree_path(&git_root) {
        debug!(path = %git_root.display(), "project-alias: skipping worktree checkout");
        return Ok(());
    }

    // Derive alias from the basename.
    let alias = alias_from_path(&git_root);

    // Load the store from the caller-provided root (same root that ls_cmd reads).
    let mut store = ProjectAliasStore::load(store_root)
        .map_err(|e| anyhow::anyhow!("load project-paths: {e}"))?;

    // Register; save only when a new entry was added.
    if store.try_register(&alias, &git_root) {
        store
            .save()
            .map_err(|e| anyhow::anyhow!("save project-paths: {e}"))?;
        debug!(alias, path = %git_root.display(), "project-alias: registered");
    }
    Ok(())
}

/// Return `true` when `path` contains a known MPM or Claude agent worktree segment.
///
/// Why: belt-and-suspenders guard for the worktree-skip logic. Catches the two
/// managed worktree path layouts (`.claude/worktrees/<id>/` and the repo-local
/// `.worktrees/<id>/`) without spawning a subprocess, so it works even when
/// `git` is not on PATH. Complements the primary `git_dir_differs_from_common`
/// runtime check.
/// What: returns `true` when the lossy path string contains `/.claude/worktrees/`
/// or `/.worktrees/`. Both patterns are specific enough to have no false positives
/// in real project trees.
/// Test: `is_worktree_path_claude_worktrees`, `is_worktree_path_repo_worktrees`,
/// `is_worktree_path_normal_project`.
pub(crate) fn is_worktree_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/.claude/worktrees/") || s.contains("/.worktrees/")
}

/// Return `true` when `path` is a linked git worktree according to git itself.
///
/// Why: the authoritative, git-level detection for linked worktrees.
/// `git rev-parse --git-dir` returns a path inside `.git/worktrees/<name>/`
/// for a linked worktree, while `--git-common-dir` always points to the MAIN
/// repo's `.git`. When the two differ the directory is a linked worktree and
/// must be skipped — auto-registering it would add a UUID-basename noise entry
/// to `tm ls`.
/// What: runs `git -C <path> rev-parse --git-dir` and `--git-common-dir`;
/// returns `true` when the outputs differ. Returns `false` on any subprocess
/// failure (git not on PATH, not a git repo, etc.) so the caller safely falls
/// through to the path-segment check.
/// Test: not directly unit-testable (spawns a subprocess); the pure
/// `is_worktree_path` helper covers the same cases for unit tests.
fn git_dir_differs_from_common(path: &Path) -> bool {
    let run = |arg: &str| -> Option<String> {
        std::process::Command::new("git")
            .args(["-C", &path.to_string_lossy(), "rev-parse", arg])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    match (run("--git-dir"), run("--git-common-dir")) {
        (Some(git_dir), Some(common_dir)) => git_dir != common_dir,
        _ => false,
    }
}

/// Walk ancestors of `cwd` looking for a `.git` entry (file or directory).
///
/// Why: replaces a `git -C <cwd> rev-parse --show-toplevel` subprocess,
/// removing a fork+exec on every `tm` invocation and working without `git` on
/// PATH. The `.git` entry is a directory for normal repos and a file for git
/// worktrees and submodules — `Path::exists()` handles both.
/// What: returns the first ancestor directory that contains a `.git` entry.
/// Returns `None` when no such ancestor exists (non-git directory or bare repo).
/// Test: `find_git_root_returns_none_outside_repo`,
/// `find_git_root_returns_some_in_this_repo`.
fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> ProjectAliasStore {
        ProjectAliasStore::load(dir.path()).expect("load")
    }

    #[test]
    fn project_alias_store_new_is_empty() {
        let dir = TempDir::new().expect("tmpdir");
        let store = make_store(&dir);
        assert!(store.list().is_empty());
    }

    #[test]
    fn try_register_inserts_new_entry() {
        let dir = TempDir::new().expect("tmpdir");
        let mut store = make_store(&dir);
        let path = PathBuf::from("/home/user/Projects/my-project");

        let changed = store.try_register("my-project", &path);
        assert!(changed, "new entry should return true");
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].alias, "my-project");
        assert_eq!(list[0].path, path);
    }

    #[test]
    fn try_register_idempotent() {
        let dir = TempDir::new().expect("tmpdir");
        let mut store = make_store(&dir);
        let path = PathBuf::from("/home/user/Projects/my-project");

        // First registration: inserts.
        assert!(store.try_register("my-project", &path));
        // Second identical registration: no-op.
        let changed = store.try_register("my-project", &path);
        assert!(!changed, "re-register same alias+path must be no-op");
        assert_eq!(store.list().len(), 1, "no duplicate entries");
    }

    #[test]
    fn try_register_collision_keeps_existing() {
        let dir = TempDir::new().expect("tmpdir");
        let mut store = make_store(&dir);
        let path_a = PathBuf::from("/home/user/Projects-A/my-project");
        let path_b = PathBuf::from("/home/user/Projects-B/my-project");

        // Register first path.
        assert!(store.try_register("my-project", &path_a));
        // Attempt registration of same alias with different path: keep existing.
        let changed = store.try_register("my-project", &path_b);
        assert!(!changed, "collision must not mutate the store");

        let list = store.list();
        assert_eq!(list.len(), 1, "collision must not add a second entry");
        assert_eq!(list[0].path, path_a, "existing path must be preserved");
    }

    #[test]
    fn project_alias_store_list_is_sorted() {
        let dir = TempDir::new().expect("tmpdir");
        let mut store = make_store(&dir);

        store.try_register("zebra", &PathBuf::from("/z"));
        store.try_register("apple", &PathBuf::from("/a"));
        store.try_register("mango", &PathBuf::from("/m"));

        let list = store.list();
        let names: Vec<&str> = list.iter().map(|e| e.alias.as_str()).collect();
        assert_eq!(names, ["apple", "mango", "zebra"]);
    }

    #[test]
    fn save_and_reload_round_trip() {
        let dir = TempDir::new().expect("tmpdir");
        {
            let mut store = make_store(&dir);
            store.try_register("alpha", &PathBuf::from("/home/user/alpha"));
            store.try_register("beta", &PathBuf::from("/home/user/beta"));
            store.save().expect("save");
        }
        {
            let store2 = make_store(&dir);
            let list = store2.list();
            assert_eq!(list.len(), 2);
            let names: Vec<&str> = list.iter().map(|e| e.alias.as_str()).collect();
            assert!(names.contains(&"alpha"), "{names:?}");
            assert!(names.contains(&"beta"), "{names:?}");
        }
    }

    #[test]
    fn alias_from_path_basename() {
        assert_eq!(
            alias_from_path(Path::new("/home/user/Projects/trusty-tools")),
            "trusty-tools"
        );
        assert_eq!(
            alias_from_path(Path::new("/home/user/Projects/trusty-tools/")),
            "trusty-tools"
        );
    }

    #[test]
    fn alias_from_path_no_component() {
        assert_eq!(alias_from_path(Path::new("/")), "project");
    }

    // ── is_worktree_path ────────────────────────────────────────────────────

    #[test]
    fn is_worktree_path_claude_worktrees() {
        // A path under `.claude/worktrees/<id>/` must be detected as a worktree.
        let p = Path::new("/home/user/Projects/my-repo/.claude/worktrees/agent-abc123def456/");
        assert!(
            is_worktree_path(p),
            "expected is_worktree_path=true for .claude/worktrees path, got false"
        );
    }

    #[test]
    fn is_worktree_path_repo_worktrees() {
        // A path under `<repo>/.worktrees/<id>/` must be detected as a worktree.
        let p = Path::new("/home/user/Projects/my-repo/.worktrees/feature-branch-wt/");
        assert!(
            is_worktree_path(p),
            "expected is_worktree_path=true for .worktrees path, got false"
        );
    }

    #[test]
    fn is_worktree_path_normal_project() {
        // A normal project root must NOT be detected as a worktree.
        let p = Path::new("/home/user/Projects/my-repo");
        assert!(
            !is_worktree_path(p),
            "expected is_worktree_path=false for normal project path, got true"
        );
    }

    #[test]
    fn is_worktree_path_home_dir_not_a_worktree() {
        // The home directory itself must not be misidentified.
        let p = Path::new("/home/user");
        assert!(!is_worktree_path(p));
    }

    // ── find_git_root ───────────────────────────────────────────────────────

    #[test]
    fn find_git_root_returns_none_outside_repo() {
        // Use a fresh TempDir guaranteed to not be inside any git repo
        // (system temp directories sit outside all project trees).
        let dir = TempDir::new().expect("tmpdir");
        let result = find_git_root(dir.path());
        assert!(
            result.is_none(),
            "expected None for non-git dir, got {result:?}"
        );
    }

    #[test]
    fn find_git_root_returns_some_in_this_repo() {
        // The worktree itself is a git repo (has a .git file at the worktree
        // root). We use CARGO_MANIFEST_DIR (crates/trusty-mpm/) as cwd.
        let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let result = find_git_root(cwd);
        assert!(result.is_some(), "expected Some for cwd inside a git repo");
        let root = result.unwrap();
        // The returned dir must contain a .git entry (file for worktrees, dir
        // for normal repos; Path::exists covers both).
        assert!(
            root.join(".git").exists(),
            ".git must exist at {}",
            root.display()
        );
    }

    #[test]
    fn try_register_and_load_round_trip_same_root() {
        // Why: proves that try_register_project_alias and ProjectAliasStore::load
        // use the same store_root, AND that worktree paths are correctly skipped
        // while normal project paths ARE registered.
        // What: detects whether CARGO_MANIFEST_DIR is inside a worktree or a
        // normal project root; asserts the correct outcome for each case.
        // Test: this function IS the test.
        let store_root = TempDir::new().expect("tmpdir");
        let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let Some(git_root) = find_git_root(cwd) else {
            return; // not inside a git repo — skip (unusual CI env)
        };

        // Register using the same store_root as the read path.
        try_register_project_alias(cwd, store_root.path());

        // Read back — same code path as ls_cmd (ProjectAliasStore::load(root)).
        let store = ProjectAliasStore::load(store_root.path()).expect("load");
        let entries = store.list();

        if is_worktree_path(&git_root) || git_dir_differs_from_common(&git_root) {
            // Running inside a git worktree: registration must be skipped.
            assert!(
                entries.is_empty(),
                "expected no entries when cwd is a worktree, got {entries:?}"
            );
        } else {
            // Running inside a normal project root: entry must appear.
            let expected_alias = alias_from_path(&git_root);
            assert!(
                entries
                    .iter()
                    .any(|e| e.alias == expected_alias && e.path == git_root),
                "expected entry alias={expected_alias} path={} in {entries:?}",
                git_root.display()
            );
        }
    }
}
