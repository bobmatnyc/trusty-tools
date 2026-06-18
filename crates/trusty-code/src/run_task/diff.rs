//! Working-tree diff capture for `run-task` (#1034).
//!
//! Why: A run's primary artifact is the change it made to the project. The
//! report must show that change as a diff so an operator (or the PM smoke-test)
//! can see exactly what the engineer wrote. When the project is a git repo we
//! capture `git diff` before vs. after the run; when it is not, we fall back to a
//! recursive file-content snapshot compared before vs. after.
//! What: `capture_snapshot` records the project state; `diff_snapshots` renders a
//! unified-ish textual diff between two snapshots. `Snapshot` is the captured
//! state (git working-tree diff text, or a map of relative path → content).
//! Test: `run_task::tests` drive a mocked engineer that writes a file and assert
//! the rendered diff names that file and its new content.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// A point-in-time capture of the project's mutable state.
///
/// Why: To diff "before" against "after" we must snapshot the same shape twice.
/// Git repos and plain directories need different representations, so the snapshot
/// is an enum that the differ matches on.
/// What: `Git` holds the textual `git diff` of the working tree against HEAD at
/// capture time; `Files` holds a sorted map of relative path → file content for
/// non-git projects.
/// Test: `run_task::tests::diff_reflects_engineer_file_change`.
#[derive(Debug, Clone)]
pub enum Snapshot {
    /// Git working-tree diff (vs. HEAD) captured at this moment.
    Git(String),
    /// Recursive file-content map for a non-git project.
    Files(BTreeMap<String, String>),
}

/// Capture the project's current state for later diffing.
///
/// Why: The run needs a "before" and an "after" of identical shape; this is the
/// single capture entry point both phases call.
/// What: If `root` is inside a git work tree, returns `Snapshot::Git` with the
/// current `git diff` (unstaged + staged via `HEAD`). Otherwise returns
/// `Snapshot::Files` with a recursive content map (skipping `.git`).
/// Test: `run_task::tests::diff_reflects_engineer_file_change` (Files path) and
/// `git_snapshot_is_used_in_repo` (Git path).
pub fn capture_snapshot(root: &Path) -> Snapshot {
    if is_git_repo(root) {
        Snapshot::Git(git_diff(root))
    } else {
        Snapshot::Files(snapshot_files(root))
    }
}

/// Render a textual diff between a before and after snapshot.
///
/// Why: The report shows the run's net change; this produces that text for both
/// the human and JSON outputs.
/// What: For `Git`, returns the *after* diff (git already expresses the full
/// working-tree change vs. HEAD, so the after-snapshot IS the diff). For `Files`,
/// emits per-file `+++ added`, `--- removed`, or a content delta for changed
/// files, comparing the two maps. Returns an empty string when nothing changed.
/// Test: `run_task::tests::diff_reflects_engineer_file_change`.
pub fn diff_snapshots(before: &Snapshot, after: &Snapshot) -> String {
    match (before, after) {
        // Git already diffs vs HEAD, so the after-snapshot is the authoritative
        // working-tree change. When it is byte-identical to the before-snapshot
        // the run changed nothing — report an empty diff so the caller can emit
        // the distinct "no changes" exit code.
        (Snapshot::Git(before_diff), Snapshot::Git(after_diff)) => {
            if before_diff == after_diff {
                String::new()
            } else {
                after_diff.clone()
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
/// Why: Chooses the snapshot strategy; a git repo gets a real `git diff`.
/// What: Runs `git rev-parse --is-inside-work-tree` and checks for a `true`
/// stdout. Any failure (git absent, not a repo) yields `false`.
/// Test: Indirect via snapshot tests.
fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// Capture the git working-tree diff against HEAD.
///
/// Why: Expresses every uncommitted change (the run's net edit) in standard
/// unified-diff form. Using `HEAD` includes both staged and unstaged edits.
/// What: Runs `git diff HEAD` in `root`; returns stdout (empty on failure or no
/// change). Untracked files are added with `--no-color` and intent-to-add so new
/// files appear in the diff.
/// Test: `run_task::tests::git_snapshot_is_used_in_repo`.
fn git_diff(root: &Path) -> String {
    // Stage intent-to-add for untracked files so brand-new files show in the diff
    // without actually committing anything (`-N` records only the path).
    let _ = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "-AN"])
        .output();

    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-color", "HEAD"])
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
/// Test: `run_task::tests::diff_reflects_engineer_file_change`.
fn snapshot_files(root: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    collect_files(root, root, &mut map);
    map
}

/// Recursive helper for `snapshot_files`.
///
/// Why: Directory walking needs the original `base` to compute relative paths.
/// What: Iterates `dir`; recurses into subdirs (except `.git`); records readable
/// files keyed by their path relative to `base`.
/// Test: Indirect via `snapshot_files` and the diff tests.
fn collect_files(base: &Path, dir: &Path, map: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".git") {
            continue;
        }
        if path.is_dir() {
            collect_files(base, &path, map);
        } else if path.is_file() {
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
/// Test: `run_task::tests::diff_reflects_engineer_file_change`.
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
}
