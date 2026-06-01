//! Per-commit diff statistics computation via `git2`.

use std::path::Path;

use git2::{Commit, Delta, DiffFormat, DiffOptions, Repository};
use tracing::debug;

use crate::collect::errors::{CollectError, Result};
use crate::core::models::ChangeType;

/// Maximum byte limit for the unified diff text returned by [`diff_for_commit`].
///
/// Why: some commits touch enormous generated files or binary assets and can
/// produce multi-MB diffs; a hard cap prevents pathological memory usage when
/// callers buffer the full text (e.g. for LLM input).
/// What: 200 KiB — empirically covers ~99% of human-authored commits while
/// keeping the worst-case bounded to a safe size.
/// Test: see `tests::diff_for_commit_truncates_at_cap` below.
pub const DIFF_BYTE_CAP: usize = 200 * 1024; // 200 KiB

/// Marker appended to a diff that was cut short by the byte cap.
const TRUNCATION_MARKER: &str = "\n[... diff truncated: output exceeded maximum byte limit ...]\n";

/// Return the unified diff text for a single commit, opening the repository
/// at `repo_path` with libgit2.
///
/// Why: the contributor-profile epic (#558) needs diff text per commit so that
/// downstream callers (e.g. LLM-based complexity scoring, review assistants)
/// can inspect what changed — the existing `compute_commit_diff` produces only
/// stats (insertions/deletions) and does not return text.
/// What: opens the repository at `repo_path`, resolves `sha` to a commit,
/// computes the diff against its first parent (or the empty tree for the root
/// commit), and formats the result as a unified diff string. Output is capped
/// at [`DIFF_BYTE_CAP`] bytes and terminated with [`TRUNCATION_MARKER`] if the
/// raw diff would exceed that limit.
/// Test: see `tests::diff_for_commit_normal_commit`,
/// `tests::diff_for_commit_initial_commit`, and
/// `tests::diff_for_commit_truncates_at_cap` below.
pub fn diff_for_commit(repo_path: &Path, sha: &str) -> Result<String> {
    let repo = Repository::open(repo_path).map_err(CollectError::Git)?;

    let oid = repo
        .revparse_single(sha)
        .map_err(CollectError::Git)?
        .peel_to_commit()
        .map_err(CollectError::Git)?
        .id();

    let commit = repo.find_commit(oid).map_err(CollectError::Git)?;

    let tree = commit.tree().map_err(CollectError::Git)?;
    let parent_tree = if commit.parent_count() > 0 {
        let parent = commit.parent(0).map_err(CollectError::Git)?;
        Some(parent.tree().map_err(CollectError::Git)?)
    } else {
        // Root commit — diff against the empty tree.
        debug!(
            sha,
            "diff_for_commit: root commit, diffing against empty tree"
        );
        None
    };

    let mut opts = DiffOptions::new();
    opts.context_lines(3)
        .include_typechange(true)
        .ignore_whitespace(false);

    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(CollectError::Git)?;

    // Accumulate the unified diff text, honouring the byte cap.
    let mut buf = String::new();
    let mut capped = false;

    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if capped {
            return true;
        }
        let origin = line.origin();
        // Standard diff line prefixes: '+', '-', ' ' (context), '@', '\n', etc.
        if matches!(origin, '+' | '-' | ' ' | '@' | '\\') {
            buf.push(origin);
        }
        if let Ok(content) = std::str::from_utf8(line.content()) {
            let remaining = DIFF_BYTE_CAP.saturating_sub(buf.len());
            if content.len() > remaining {
                buf.push_str(&content[..remaining]);
                capped = true;
            } else {
                buf.push_str(content);
            }
        }
        true
    })
    .map_err(CollectError::Git)?;

    if capped {
        buf.push_str(TRUNCATION_MARKER);
    }

    Ok(buf)
}

/// Aggregated diff stats for a single commit.
#[derive(Debug, Clone, Default)]
pub struct CommitDiff {
    /// Total number of files touched by the commit.
    pub files_changed: u32,

    /// Total lines inserted across all files.
    pub insertions: u32,

    /// Total lines deleted across all files.
    pub deletions: u32,

    /// Per-file change records.
    pub files: Vec<FileDiff>,
}

/// Per-file diff record for storage in the `files` table.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// Path relative to the repository root.
    pub path: String,

    /// Type of change.
    pub change_type: ChangeType,

    /// Lines inserted in this file.
    pub insertions: u32,

    /// Lines deleted in this file.
    pub deletions: u32,
}

/// Compute the diff between a commit and its first parent (or the empty
/// tree if it's the root commit).
///
/// For merge commits (multiple parents), the diff is computed against the
/// first parent only — matching the conventional "what did this merge
/// introduce on top of its mainline parent" interpretation.
///
/// # Errors
///
/// Propagates any `git2` errors from tree lookups or diff computation.
pub fn compute_commit_diff(repo: &Repository, commit: &Commit<'_>) -> Result<CommitDiff> {
    // PROFILING NOTE (see docs/trusty-git-analytics/decisions/0002-performance-hotspots.md):
    // `commit.tree()?` and `commit.parent(0)?.tree()?` are libgit2 object
    // lookups against the on-disk ODB. Profiles on a 58K-commit monolith
    // show ~35% of `collect_window` time is spent in these two calls.
    // A future optimisation could cache the previous commit's tree across
    // adjacent revwalk steps, but `Sort::TIME` does not guarantee parent
    // adjacency on merge-heavy histories, so the cache hit rate is
    // workload-dependent. The simpler win — `find_similar` with rename
    // detection only when the diff would otherwise show many add/delete
    // pairs — is left as a follow-up.
    let tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let mut opts = DiffOptions::new();
    opts.include_typechange(true);
    let mut diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

    // Enable rename and copy detection so renamed files are reported as a single
    // `Delta::Renamed` operation with net line differences rather than as a
    // delete-of-old-file plus add-of-new-file pair (which would double-count lines).
    // This requires the mutable `find_similar` call on the diff itself.
    let mut find_opts = git2::DiffFindOptions::new();
    find_opts.renames(true).copies(true);
    diff.find_similar(Some(&mut find_opts))?;

    let stats = diff.stats()?;
    let files_cell: std::cell::RefCell<Vec<FileDiff>> =
        std::cell::RefCell::new(Vec::with_capacity(stats.files_changed()));

    diff.foreach(
        &mut |delta, _progress| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let change_type = map_change_type(delta.status());
            files_cell.borrow_mut().push(FileDiff {
                path,
                change_type,
                insertions: 0,
                deletions: 0,
            });
            true
        },
        None,
        None,
        Some(&mut |delta, _hunk, line| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut files = files_cell.borrow_mut();
            if let Some(file) = files.iter_mut().find(|f| f.path == path) {
                match line.origin() {
                    '+' => file.insertions = file.insertions.saturating_add(1),
                    '-' => file.deletions = file.deletions.saturating_add(1),
                    _ => {}
                }
            }
            true
        }),
    )?;

    Ok(CommitDiff {
        files_changed: stats.files_changed() as u32,
        insertions: stats.insertions() as u32,
        deletions: stats.deletions() as u32,
        files: files_cell.into_inner(),
    })
}

/// Translate a libgit2 `Delta` enum into our [`ChangeType`].
fn map_change_type(delta: Delta) -> ChangeType {
    match delta {
        Delta::Added | Delta::Copied | Delta::Untracked => ChangeType::Added,
        Delta::Deleted => ChangeType::Deleted,
        Delta::Renamed => ChangeType::Renamed,
        _ => ChangeType::Modified,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temporary git repository with an initial commit adding `content`
    /// to `filename`. Returns `(TempDir, sha_string)`.
    fn make_repo_with_initial_commit(filename: &str, content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init repo");

        let mut config = repo.config().expect("config");
        config.set_str("user.name", "Test User").expect("set name");
        config
            .set_str("user.email", "test@example.com")
            .expect("set email");

        // Write file.
        let file_path = dir.path().join(filename);
        std::fs::write(&file_path, content).expect("write file");

        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new(filename))
            .expect("add path");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
            .expect("initial commit");

        (dir, commit_oid.to_string())
    }

    /// Create a follow-up commit in `repo_path` that modifies `filename`.
    fn add_follow_up_commit(repo_path: &Path, filename: &str, new_content: &str) -> String {
        let repo = git2::Repository::open(repo_path).expect("open repo");

        let file_path = repo_path.join(filename);
        std::fs::write(&file_path, new_content).expect("write file");

        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new(filename))
            .expect("add path");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Test User", "test@example.com").expect("sig");
        let head = repo.head().expect("head").peel_to_commit().expect("peel");
        let commit_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "Follow-up commit",
                &tree,
                &[&head],
            )
            .expect("follow-up commit");

        commit_oid.to_string()
    }

    /// Why: a normal (non-root) commit should produce a unified diff showing
    /// the change from parent → child, including `+` lines for additions and
    /// `-` lines for removals.
    /// What: creates a two-commit repo, calls `diff_for_commit` on the second
    /// commit, asserts expected `+`/`-` markers appear in the output.
    /// Test: this test itself.
    #[test]
    fn diff_for_commit_normal_commit() {
        let (dir, _initial_sha) = make_repo_with_initial_commit("hello.txt", "hello world\n");
        let sha = add_follow_up_commit(dir.path(), "hello.txt", "hello universe\n");

        let diff = diff_for_commit(dir.path(), &sha).expect("diff_for_commit");

        assert!(
            diff.contains("+hello universe"),
            "diff should contain added line: {diff}"
        );
        assert!(
            diff.contains("-hello world"),
            "diff should contain removed line: {diff}"
        );
    }

    /// Why: the root commit has no parent; `diff_for_commit` must handle this by
    /// diffing against the empty tree so all new file content appears as `+` lines.
    /// What: creates a single-commit repo, calls `diff_for_commit` on the initial
    /// commit, asserts the file's content appears as `+` additions.
    /// Test: this test itself.
    #[test]
    fn diff_for_commit_initial_commit() {
        let (dir, sha) = make_repo_with_initial_commit("readme.txt", "# Hello\n");

        let diff = diff_for_commit(dir.path(), &sha).expect("diff_for_commit");

        assert!(
            diff.contains("+# Hello"),
            "initial commit diff should show added content: {diff}"
        );
        // No `-` lines for new files added from empty tree.
        let minus_content_lines: Vec<&str> = diff
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .collect();
        assert!(
            minus_content_lines.is_empty(),
            "initial commit should have no removed lines: {:?}",
            minus_content_lines
        );
    }

    /// Why: pathological commits on generated files could produce very large diffs;
    /// the cap must enforce a hard limit and append the truncation marker.
    /// What: creates a commit with content larger than `DIFF_BYTE_CAP`, calls
    /// `diff_for_commit`, asserts the output length is bounded and ends with the
    /// truncation marker.
    /// Test: this test itself.
    #[test]
    fn diff_for_commit_truncates_at_cap() {
        // Content significantly larger than the cap (line-by-line to be valid UTF-8).
        let line = "x".repeat(120);
        let big_content: String = (0..2000)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let (dir, _initial_sha) = make_repo_with_initial_commit("big.txt", "");
        let sha = add_follow_up_commit(dir.path(), "big.txt", &big_content);

        let diff = diff_for_commit(dir.path(), &sha).expect("diff_for_commit");

        assert!(
            diff.len() <= DIFF_BYTE_CAP + TRUNCATION_MARKER.len() + 200,
            "diff length {} should be near the byte cap",
            diff.len()
        );
        assert!(
            diff.contains("diff truncated"),
            "truncated diff must contain the marker: len={}",
            diff.len()
        );
    }

    /// Why: a non-existent SHA must produce a `CollectError::Git` error rather
    /// than panicking.
    /// What: passes a bogus SHA to `diff_for_commit` on a real repo, asserts
    /// an error is returned.
    /// Test: this test itself.
    #[test]
    fn diff_for_commit_invalid_sha_returns_error() {
        let (dir, _) = make_repo_with_initial_commit("f.txt", "content\n");
        let result = diff_for_commit(dir.path(), "0000000000000000000000000000000000000000");
        assert!(result.is_err(), "invalid SHA must return an error, not Ok");
    }
}
