//! Per-commit diff statistics computation via `git2`.

use git2::{Commit, Delta, DiffOptions, Repository};

use crate::collect::errors::Result;
use crate::core::models::ChangeType;

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
