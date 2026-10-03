//! Two disk facts a worktree removal checks around `git worktree remove` (#8782).
//!
//! Why: a scanned path is not an identity — the directory at it can be replaced
//! by a symlink to another registered worktree, and `git worktree remove
//! --force` resolves the symlink and removes the target. And git can exit
//! non-zero after it has already deleted part of the tree; reporting that as
//! "kept" tells the operator the work is safe when it is not.
//! What: [`identity_refusal`] refuses a path that no longer resolves to itself.
//! [`content_count`] and [`partial_removal`] tell a failed removal that deleted
//! nothing from one that deleted some or all of the tree.
//! Test: `a_merged_pr_path_replaced_by_a_symlink_is_not_removed`,
//! `a_scanned_path_replaced_by_a_symlink_is_not_removed`,
//! `a_git_failure_after_a_partial_delete_is_reported_as_partially_removed`.

use std::path::{Path, PathBuf};

/// Why `path` must not be removed now, or `None` when it still resolves to
/// itself (#8782).
///
/// Why: every scanned worktree path is canonical, so a canonical form that
/// differs means the path was replaced since the scan. A path that no longer
/// resolves at all is refused too.
/// What: `std::fs::canonicalize(path) == path`, else a reason naming what the
/// path now resolves to.
/// Test: `a_merged_pr_path_replaced_by_a_symlink_is_not_removed`,
/// `a_scanned_path_replaced_by_a_symlink_is_not_removed`.
pub(crate) fn identity_refusal(path: &Path) -> Option<String> {
    match std::fs::canonicalize(path) {
        Ok(resolved) if resolved == path => None,
        Ok(resolved) => Some(format!(
            "the path no longer resolves to the scanned worktree — it now resolves to {} (#8782)",
            resolved.display()
        )),
        Err(e) => Some(format!(
            "the path no longer resolves to the scanned worktree: {e} (#8782)"
        )),
    }
}

/// Every entry below `path`, counted without following symlinks (#8782).
///
/// `None` when `path` is not a real directory. An unreadable subdirectory
/// counts as itself only, the same way before and after a removal.
pub(crate) fn content_count(path: &Path) -> Option<u64> {
    if !std::fs::symlink_metadata(path).ok()?.is_dir() {
        return None;
    }
    let mut count = 0u64;
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            count += 1;
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    Some(count)
}

/// The report for a failed removal that deleted content, or `None` when it
/// deleted nothing (#8782).
///
/// Why: `git worktree remove --force` deletes the tree before it reports some
/// failures, so a non-zero exit is not proof the tree is intact.
/// What: `failure` is git's exit and error. The path is judged against
/// `before`, its [`content_count`] taken just before the call: missing, empty,
/// or fewer entries than `before` is a partial removal.
/// Test: `a_git_failure_after_a_partial_delete_is_reported_as_partially_removed`.
pub(crate) fn partial_removal(path: &Path, before: Option<u64>, failure: &str) -> Option<String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Some(format!(
            "removed with an error — {failure}; the directory is gone (#8782)"
        ));
    }
    let deleted = match (before, content_count(path)) {
        (Some(was), Some(now)) if now < was => format!("{} of its {was} entries", was - now),
        (was, Some(0)) if was != Some(0) => "every entry in it".to_string(),
        _ => return None,
    };
    Some(format!(
        "partially removed — {failure}, after deleting {deleted}; what is left is still on \
         disk (#8782)"
    ))
}
