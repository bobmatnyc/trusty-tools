//! Working-tree diff capture for `run-task` (#1034, hardened by #2061/#1475
//! bugs 3, 4, 5).
//!
//! Why: A run's primary artifact is the change it made to the project. The
//! report must show that change as a diff so an operator (or the PM smoke-test)
//! can see exactly what the engineer wrote. When the project is a git repo we
//! capture a git TREE snapshot before vs. after the run and diff the two trees
//! (see [`snapshot_git_tree`]'s docs for why — this is the #1475 bug 3/4 fix:
//! the old approach ran `git add -AN`, mutating the user's real index as a
//! side effect, AND diffed the after-snapshot's raw `git diff HEAD` text
//! wholesale, which included any PRE-EXISTING dirty-tree noise rather than
//! just the run's own incremental change). When the project is not a git repo
//! we fall back to a recursive file-content snapshot compared before vs.
//! after, now guarded against symlink cycles (#1475 bug 5).
//! What: `capture_snapshot` records the project state; `diff_snapshots` renders
//! a unified-ish textual diff between two snapshots. `Snapshot` is the captured
//! state (a git tree object SHA for git repos, or a map of relative path →
//! content for plain directories).
//! Test: `run_task::tests` drive a mocked engineer that writes a file and assert
//! the rendered diff names that file and its new content; `diff::tests` cover
//! the git-tree non-mutation/dirty-tree-correctness/symlink-loop fixes
//! directly against real temp git repos and temp dirs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A point-in-time capture of the project's mutable state.
///
/// Why: To diff "before" against "after" we must snapshot the same shape twice.
/// Git repos and plain directories need different representations, so the snapshot
/// is an enum that the differ matches on.
/// What: `Git` holds the project root plus a git TREE OBJECT SHA representing
/// the full on-disk state (tracked + untracked, respecting `.gitignore`) at
/// capture time — NOT a diff-text snapshot (see module docs for why that
/// changed). `Files` holds a sorted map of relative path → file content for
/// non-git projects.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
#[derive(Debug, Clone)]
pub enum Snapshot {
    /// A git tree object SHA capturing the full working-tree state at this
    /// moment, plus the project root needed to diff two such SHAs later.
    Git { root: PathBuf, tree: String },
    /// Recursive file-content map for a non-git project.
    Files(BTreeMap<String, String>),
}

/// Capture the project's current state for later diffing.
///
/// Why: The run needs a "before" and an "after" of identical shape; this is the
/// single capture entry point both phases call.
/// What: If `root` is inside a git work tree, returns `Snapshot::Git` with a
/// tree SHA from [`snapshot_git_tree`]. Otherwise returns `Snapshot::Files`
/// with a recursive content map (skipping `.git`).
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer` (Files path)
/// and `diff::tests::git_index_and_working_tree_are_unchanged_after_capture`
/// (Git path).
pub fn capture_snapshot(root: &Path) -> Snapshot {
    if is_git_repo(root) {
        Snapshot::Git {
            root: root.to_path_buf(),
            tree: snapshot_git_tree(root),
        }
    } else {
        Snapshot::Files(snapshot_files(root))
    }
}

/// Render a textual diff between a before and after snapshot.
///
/// Why: The report shows the run's net change; this produces that text for both
/// the human and JSON outputs.
/// What: For `Git`, diffs the two captured TREE SHAs directly
/// (`git diff <before_tree> <after_tree>`) — this is exactly the run's
/// incremental change, correct regardless of any pre-existing dirty-tree
/// state that was already present in BOTH snapshots (#1475 bug 4). For
/// `Files`, emits per-file `+++ added`, `--- removed`, or a content delta for
/// changed files, comparing the two maps. Returns an empty string when
/// nothing changed, or when either tree SHA is empty (git invocation failed —
/// degrade to "no diff available" rather than issuing a malformed command).
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
/// `diff::tests::dirty_tree_diff_contains_only_the_tasks_own_change`.
pub fn diff_snapshots(before: &Snapshot, after: &Snapshot) -> String {
    match (before, after) {
        (
            Snapshot::Git {
                root,
                tree: before_tree,
            },
            Snapshot::Git {
                tree: after_tree, ..
            },
        ) => {
            if before_tree == after_tree || before_tree.is_empty() || after_tree.is_empty() {
                String::new()
            } else {
                git_diff_trees(root, before_tree, after_tree)
            }
        }
        (Snapshot::Files(before_map), Snapshot::Files(after_map)) => {
            diff_file_maps(before_map, after_map)
        }
        // Mixed snapshot kinds cannot happen (capture is deterministic for a
        // given root), but handle it defensively rather than panicking.
        _ => String::new(),
    }
}

/// Whether `root` is inside a git work tree.
///
/// Why: Chooses the snapshot strategy; a git repo gets a real tree-diff. This
/// is a thin alias for [`crate::binding::is_git_worktree`] rather than a second
/// implementation: `ProjectBinding` classifies `GitRepo` vs `Directory` on that
/// exact probe, and two independent git checks could disagree — producing a
/// `GitRepo` binding whose diff silently took the non-git file-map path.
/// What: delegates; fails open to `false` (git absent, not a repo).
/// Test: Indirect via snapshot tests; the probe itself in
/// `binding::tests::is_git_worktree_fails_open_for_missing_path`.
fn is_git_repo(root: &Path) -> bool {
    crate::binding::is_git_worktree(root)
}

/// Capture the FULL on-disk working-tree state (tracked changes + untracked
/// files, respecting `.gitignore`) as a git tree object SHA, WITHOUT
/// mutating the user's real index or working tree (#1475 bug 3).
///
/// Why: The old implementation ran `git add -AN` against the repo's REAL
/// index as a snapshot side effect — a genuine correctness bug (the user's
/// index must be byte-identical before and after a run). This implementation
/// stages into a THROWAWAY index file (via the `GIT_INDEX_FILE` env var,
/// pointed at a tempdir-scoped path that is never `root/.git/index`) and
/// only ever reads from / writes objects for that temp index — the real
/// index and working tree are never touched. `git write-tree`/`git add`
/// against a temp index still writes blob/tree objects into the repo's real
/// `.git/objects` object database, but that is inherent, harmless,
/// content-addressed storage growth — it is invisible to `git status`,
/// `git diff`, and every other porcelain command, so it does not violate
/// "the index/working tree must be unchanged."
/// What: seeds the temp index from `HEAD`'s tree (best-effort — a repo with
/// no commits yet has no HEAD, so this step simply fails and leaves an
/// empty temp index, which is the CORRECT "before" state for a brand-new
/// repo), stages everything currently on disk into ONLY that temp index via
/// `git add -A`, then writes and returns a tree object SHA for it. Returns
/// an empty string if `git` itself cannot be invoked (degrades to "no diff
/// available" in [`diff_snapshots`] rather than erroring).
/// Test: `diff::tests::git_index_and_working_tree_are_unchanged_after_capture`,
/// `diff::tests::dirty_tree_diff_contains_only_the_tasks_own_change`.
fn snapshot_git_tree(root: &Path) -> String {
    let Ok(index_dir) = tempfile::tempdir() else {
        return String::new();
    };
    let index_path = index_dir.path().join("throwaway-index");

    // Best-effort: fails harmlessly on a repo with no commits yet, leaving
    // an empty temp index (the correct "before" state for a fresh repo).
    let _ = run_git_with_index(root, &index_path, &["read-tree", "HEAD"]);
    let _ = run_git_with_index(root, &index_path, &["add", "-A"]);

    run_git_with_index(root, &index_path, &["write-tree"])
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run a git subcommand against `root`, with `GIT_INDEX_FILE` pointed at
/// `index_path` — the mechanism that keeps every index mutation confined to
/// the throwaway index (#1475 bug 3).
fn run_git_with_index(root: &Path, index_path: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .env("GIT_INDEX_FILE", index_path)
        .args(args)
        .output()
        .ok()
}

/// Diff two git tree object SHAs.
///
/// Why: Captured trees from [`snapshot_git_tree`] are the "before"/"after"
/// pair; diffing them directly (rather than two `git diff HEAD` text
/// captures) is what makes the reported diff exactly the run's own
/// incremental change (#1475 bug 4) — any pre-existing dirty-tree content
/// present identically in both trees produces no delta.
/// What: Runs `git diff --no-color <before_tree> <after_tree>` in `root`;
/// returns stdout (empty on failure or no change). Does not touch the index
/// at all (diffing two tree objects is a pure read).
/// Test: `diff::tests::dirty_tree_diff_contains_only_the_tasks_own_change`.
fn git_diff_trees(root: &Path, before_tree: &str, after_tree: &str) -> String {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-color", before_tree, after_tree])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Recursively snapshot file contents under `root` (skipping `.git`).
///
/// Why: Non-git projects still need a before/after comparison; a content map is
/// the simplest faithful representation.
/// What: Walks `root`, reading each regular file into a `relative-path → content`
/// map. Binary/unreadable files are stored as a placeholder marker. Skips the
/// `.git` directory and any path component starting with `.git`.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
fn snapshot_files(root: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    collect_files(root, root, &mut map, 0);
    map
}

/// Maximum directory-nesting depth `collect_files` will descend.
///
/// Why: a belt-and-braces bound alongside the symlink-identity guard below —
/// even a non-cyclic but pathologically deep tree (or an exotic filesystem
/// where the identity check somehow fails to detect a cycle) cannot recurse
/// past this many levels. 128 is far deeper than any real project tree.
const MAX_WALK_DEPTH: usize = 128;

/// Recursive helper for `snapshot_files`, guarded against symlink cycles
/// (#1475 bug 5).
///
/// Why: The original implementation recursed on `path.is_dir()`, which
/// FOLLOWS symlinks — a symlink pointing back at an ancestor directory (or
/// forming any cycle) caused unbounded recursion and a stack overflow. Both
/// a per-call symlink check AND a depth bound are applied so a cycle can
/// never be traversed, regardless of how it is shaped.
/// What: Iterates `dir`; skips any entry whose `symlink_metadata` reports
/// `is_symlink()` (a symlink is never followed, whether it points to a file
/// or a directory — the run's diff cares about real project content, not
/// symlink targets that may point outside the project entirely); recurses
/// into real subdirectories (except `.git`) up to [`MAX_WALK_DEPTH`]; records
/// readable regular files keyed by their path relative to `base`.
/// Test: `diff::tests::collect_files_terminates_on_symlink_cycle`.
fn collect_files(base: &Path, dir: &Path, map: &mut BTreeMap<String, String>, depth: usize) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".git") {
            continue;
        }
        // `symlink_metadata` does NOT follow the link — this is the check
        // that makes a symlink cycle impossible to traverse at all, rather
        // than merely bounding how far it can be traversed.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files(base, &path, map, depth + 1);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let content =
                std::fs::read_to_string(&path).unwrap_or_else(|_| "<binary or unreadable>".into());
            map.insert(rel, content);
        }
    }
}

/// Diff two file-content maps into a readable change report.
///
/// Why: For non-git projects this is the only way to show what changed; the
/// report compares the before/after maps key by key.
/// What: Emits `+++ added: <path>` (with content) for new keys, `--- removed:
/// <path>` for deleted keys, and a `~~~ changed: <path>` block with old→new
/// content for keys whose content differs. Unchanged keys are omitted. Returns an
/// empty string when the maps are identical.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
fn diff_file_maps(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> String {
    let mut out = String::new();

    for (path, content) in after {
        match before.get(path) {
            None => {
                out.push_str(&format!("+++ added: {path}\n{content}\n"));
            }
            Some(old) if old != content => {
                out.push_str(&format!(
                    "~~~ changed: {path}\n--- before\n{old}\n+++ after\n{content}\n"
                ));
            }
            Some(_) => {}
        }
    }

    for path in before.keys() {
        if !after.contains_key(path) {
            out.push_str(&format!("--- removed: {path}\n"));
        }
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// File-map diff reports an added file with its content.
    ///
    /// Why: The non-git path must surface a brand-new file as an addition.
    /// What: Before is empty; after has one file; assert the rendered diff names
    /// the file and includes its content.
    /// Test: this test.
    #[test]
    fn file_map_diff_reports_addition() {
        let before = BTreeMap::new();
        let mut after = BTreeMap::new();
        after.insert("hello.py".to_string(), "print('hi')".to_string());

        let diff = diff_file_maps(&before, &after);
        assert!(diff.contains("+++ added: hello.py"), "diff: {diff}");
        assert!(diff.contains("print('hi')"), "diff: {diff}");
    }

    /// File-map diff reports a changed file with old and new content.
    ///
    /// Why: Edits to existing files must be visible.
    /// What: Same key, different content; assert a `changed` block with both.
    /// Test: this test.
    #[test]
    fn file_map_diff_reports_change() {
        let mut before = BTreeMap::new();
        before.insert("a.txt".to_string(), "old".to_string());
        let mut after = BTreeMap::new();
        after.insert("a.txt".to_string(), "new".to_string());

        let diff = diff_file_maps(&before, &after);
        assert!(diff.contains("~~~ changed: a.txt"), "diff: {diff}");
        assert!(diff.contains("old") && diff.contains("new"), "diff: {diff}");
    }

    /// Identical maps produce an empty diff (no-op detection).
    ///
    /// Why: A run that changes nothing must report no diff, enabling the
    /// distinct "no changes" exit code.
    /// What: Equal maps; assert empty string.
    /// Test: this test.
    #[test]
    fn identical_maps_produce_empty_diff() {
        let mut m = BTreeMap::new();
        m.insert("x".to_string(), "y".to_string());
        assert!(diff_file_maps(&m, &m).is_empty());
    }

    /// `capture_snapshot` + `diff_snapshots` surface a new file in a non-git dir.
    ///
    /// Why: End-to-end guard for the non-git capture/diff path.
    /// What: Snapshot an empty dir, write a file, snapshot again, diff; assert the
    /// file appears as an addition.
    /// Test: this test.
    #[test]
    fn capture_and_diff_non_git_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let before = capture_snapshot(tmp.path());
        std::fs::write(tmp.path().join("new.rs"), "fn main() {}").expect("write");
        let after = capture_snapshot(tmp.path());

        let diff = diff_snapshots(&before, &after);
        assert!(diff.contains("+++ added: new.rs"), "diff: {diff}");
        assert!(diff.contains("fn main()"), "diff: {diff}");
    }

    /// `collect_files` must TERMINATE (not hang or stack-overflow) when the
    /// directory tree contains a symlink cycle (#1475 bug 5).
    ///
    /// Why: The original implementation followed symlinks via `is_dir()`
    /// with no guard; a symlink pointing back at an ancestor directory
    /// caused unbounded recursion.
    /// What: Create `root/loop` as a symlink to `root` itself (the simplest
    /// possible cycle: a directory containing a link back to itself), then
    /// call `snapshot_files(root)`. The call returning at all (rather than
    /// hanging or overflowing the stack) is the pass condition; also assert
    /// the symlink itself was not traversed into.
    /// Test: this test.
    #[test]
    #[cfg(unix)]
    fn collect_files_terminates_on_symlink_cycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("real.txt"), "hello").expect("write real file");
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("loop"))
            .expect("create symlink cycle");

        // If the symlink guard is broken, this call never returns (stack
        // overflow) — its mere completion is the assertion.
        let map = snapshot_files(tmp.path());

        assert!(
            map.contains_key("real.txt"),
            "the real file must still be captured: {map:?}"
        );
        assert!(
            !map.keys().any(|k| k.starts_with("loop/")),
            "the symlink must not have been traversed into: {map:?}"
        );
    }

    /// The git index and working-tree status must be BYTE-IDENTICAL before
    /// and after `capture_snapshot` runs against a git repo (#1475 bug 3).
    ///
    /// Why: The original implementation ran `git add -AN` against the real
    /// index as a snapshot side effect — a real mutation the user would see
    /// in their next `git status`. This is the guard against reintroducing
    /// that.
    /// What: Init a repo with one committed file, add both a staged and an
    /// unstaged pre-existing change plus an untracked file (a representative
    /// dirty tree), snapshot the real index file's bytes and
    /// `git status --porcelain`'s output, call `capture_snapshot`, then
    /// assert the index bytes and porcelain status are UNCHANGED.
    /// Test: this test.
    #[test]
    fn git_index_and_working_tree_are_unchanged_after_capture() {
        let repo = init_repo_with_commit();

        // Pre-existing dirty state: a staged change, an unstaged change, and
        // an untracked file — the full matrix `git add -AN` used to disturb.
        std::fs::write(repo.path().join("committed.txt"), "staged edit").expect("write");
        run(&repo, &["add", "committed.txt"]);
        std::fs::write(repo.path().join("unstaged.txt"), "unstaged\n").expect("write");
        run(&repo, &["add", "unstaged.txt"]);
        run(&repo, &["commit", "-m", "second"]);
        std::fs::write(repo.path().join("unstaged.txt"), "unstaged edit").expect("write");
        std::fs::write(repo.path().join("untracked.txt"), "new").expect("write");

        let index_path = repo.path().join(".git").join("index");
        let index_before = std::fs::read(&index_path).expect("read index");
        let status_before = porcelain_status(&repo);

        let _ = capture_snapshot(repo.path());

        let index_after = std::fs::read(&index_path).expect("read index");
        let status_after = porcelain_status(&repo);

        assert_eq!(
            index_before, index_after,
            "the real .git/index must be byte-identical before/after capture_snapshot"
        );
        assert_eq!(
            status_before, status_after,
            "git status --porcelain must be unchanged before/after capture_snapshot"
        );
    }

    /// On a DIRTY working tree (pre-existing uncommitted changes untouched
    /// by the task), the captured diff must contain ONLY the task's own
    /// change — not the pre-existing noise (#1475 bug 4).
    ///
    /// Why: The original implementation returned the after-snapshot's raw
    /// `git diff HEAD` text wholesale, which necessarily included whatever
    /// was already dirty before the run started.
    /// What: Commit a file, dirty the tree with an untracked file and an
    /// unstaged edit to a DIFFERENT tracked file, capture "before", make the
    /// task's OWN change (write a new file), capture "after", diff; assert
    /// the diff names the task's file but NOT the pre-existing dirty
    /// filenames.
    /// Test: this test.
    #[test]
    fn dirty_tree_diff_contains_only_the_tasks_own_change() {
        let repo = init_repo_with_commit();

        // Pre-existing dirty noise the task never touches.
        std::fs::write(
            repo.path().join("committed.txt"),
            "pre-existing unstaged edit",
        )
        .expect("write");
        std::fs::write(repo.path().join("pre-existing-untracked.txt"), "noise").expect("write");

        let before = capture_snapshot(repo.path());

        // The task's own change.
        std::fs::write(repo.path().join("task_output.py"), "print('task ran')").expect("write");

        let after = capture_snapshot(repo.path());
        let diff = diff_snapshots(&before, &after);

        assert!(
            diff.contains("task_output.py"),
            "diff must contain the task's own new file: {diff}"
        );
        assert!(
            !diff.contains("pre-existing-untracked.txt"),
            "diff must NOT contain the pre-existing untracked noise: {diff}"
        );
        assert!(
            !diff.contains("pre-existing unstaged edit"),
            "diff must NOT contain the pre-existing unstaged edit's content: {diff}"
        );
    }

    /// Init a temp git repo with one committed file (`committed.txt`).
    fn init_repo_with_commit() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("tempdir");
        run(&repo, &["init", "-q"]);
        run(&repo, &["config", "user.email", "test@example.com"]);
        run(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("committed.txt"), "original\n").expect("write");
        run(&repo, &["add", "committed.txt"]);
        run(&repo, &["commit", "-q", "-m", "initial"]);
        repo
    }

    /// Run a git subcommand against `repo`'s REAL index (test-only helper —
    /// simulates what a human/CI operator does directly, distinct from
    /// `run_git_with_index`'s throwaway-index production path).
    fn run(repo: &tempfile::TempDir, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} must succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn porcelain_status(repo: &tempfile::TempDir) -> String {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["status", "--porcelain"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .expect("git status")
    }
}
