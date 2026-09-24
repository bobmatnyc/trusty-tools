//! Where a worktree's ownership marker lives, and moving it there (#8511).
//!
//! Why: the marker used to sit in the working tree as `.trusty-mpm-worktree`.
//! In a project that does not ignore it, git counts it as untracked, so the
//! tree reads dirty: `git worktree remove` without `--force` refuses, Claude
//! Code's own cleanup refuses, and `git add -A` can commit it (#8368). Claude
//! Code keeps its own marker in git metadata, outside the tree; this module
//! puts ours there too.
//! What: the marker now lives at `git rev-parse --git-path trusty-mpm-worktree`
//! for the tree — `<common-dir>/worktrees/<name>/trusty-mpm-worktree` for a
//! linked worktree, `.git/trusty-mpm-worktree` for a main checkout. It is
//! resolved from the tree's own `.git` entry ([`admin_sentinel_path`]), never
//! guessed. A directory with no `.git` entry is not a git working tree, so no
//! git clean check can count a file in it; there the in-tree path is the only
//! location. The decision table:
//!
//! | on disk | read answers | side effect |
//! |---|---|---|
//! | admin only | admin | none |
//! | legacy only | legacy | copy to admin, verify byte for byte, then remove legacy |
//! | both, same bytes | admin | remove the leftover legacy copy |
//! | both, different bytes | admin | log the conflict, keep both |
//! | admin unreadable | nothing (owner unknown) | log |
//! | legacy tracked by git | admin if present, else legacy | none — never removed |
//! | no `.git` entry | legacy | none |
//!
//! A legacy marker a branch has already COMMITTED (#8368) is never removed:
//! deleting a tracked file would make the tree dirty, the opposite of the fix.
//!
//! Fail-open: a copy or verification that fails removes only the admin copy
//! this module created and keeps the legacy file, so ownership is never lost.
//! Test: `worktree_ownership_location_tests`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::worktree_ownership::{SentinelOwner, WorktreeSentinel, owner_from_payload};

/// The marker's file name inside the git admin directory (#8511).
///
/// Why: `git rev-parse --git-path trusty-mpm-worktree` is the documented
/// location; spelling the name once keeps the writer and every reader on it.
pub(crate) const ADMIN_SENTINEL_NAME: &str = "trusty-mpm-worktree";

/// The admin-dir marker path for the tree rooted at `worktree`, when it is a git
/// working tree (#8511).
///
/// Why: the path must match what `git rev-parse --git-path` prints, and must be
/// cheap — readers call this once per candidate in every sweep, so a `git`
/// subprocess per call is not affordable.
/// What: reads `<worktree>/.git`. A directory is the git dir itself; a file is
/// a linked worktree's `gitdir: <path>` pointer, resolved against `worktree`
/// when relative, and accepted only when it names an existing directory.
/// Anything else — no `.git`, an unreadable or malformed pointer — is `None`.
/// Test: `admin_path_matches_git_rev_parse_git_path`.
pub(crate) fn admin_sentinel_path(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.join(ADMIN_SENTINEL_NAME));
    }
    if !meta.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let raw = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    if raw.is_empty() {
        return None;
    }
    let git_dir = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        worktree.join(raw)
    };
    git_dir.is_dir().then(|| git_dir.join(ADMIN_SENTINEL_NAME))
}

/// The legacy in-tree marker path, `<worktree>/.trusty-mpm-worktree`.
pub(crate) fn legacy_sentinel_path(worktree: &Path) -> PathBuf {
    worktree.join(WORKTREE_SENTINEL_FILE)
}

/// Does `worktree` carry an ownership marker in either location (#8511)?
///
/// Why: the removal gates and the workspace scanner ask "was this tree
/// provisioned by trusty-mpm?" by the marker's existence; a marker moved to
/// the admin dir must still answer yes.
/// What: legacy file exists, or the admin-dir file exists.
/// Test: `a_tree_marked_only_in_the_admin_dir_is_still_marked`.
pub(crate) fn sentinel_present(worktree: &Path) -> bool {
    legacy_sentinel_path(worktree).exists()
        || admin_sentinel_path(worktree).is_some_and(|p| p.exists())
}

/// What one look at a tree's legacy marker did (#8511).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkerMigration {
    /// No legacy in-tree marker: nothing to move.
    NoLegacy,
    /// Not a git working tree, so the in-tree location is the only one.
    NoAdminDir,
    /// The legacy marker was copied, verified, and removed.
    Migrated,
    /// Both existed with identical bytes; the leftover legacy copy was removed.
    DuplicateRemoved,
    /// Both existed with different bytes. The admin marker wins; both are kept.
    Conflict,
    /// The legacy marker is tracked by git (#8368), so it is left in place.
    Tracked,
    /// A step failed. The legacy marker is kept, so ownership is not lost.
    Failed(String),
}

/// Is `worktree`'s legacy marker tracked by git (#8368)?
///
/// Why: removing a committed marker would dirty the tree. Only asked when a
/// legacy marker exists, so the subprocess stays off the common path.
/// What: `git ls-files -- .trusty-mpm-worktree` is non-empty. A git failure
/// answers `true` — keeping a file is the safe direction.
/// Test: `a_committed_legacy_marker_is_never_removed`.
pub(crate) fn legacy_is_tracked(worktree: &Path) -> bool {
    match trusty_common::git::command_in(worktree)
        .args(["ls-files", "--", WORKTREE_SENTINEL_FILE])
        .output()
    {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

/// Copies `bytes` to a NEW file at `path` — the production migration writer.
///
/// `create_new` makes a concurrent writer's admin marker win: this copy never
/// overwrites one.
fn copy_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Move `worktree`'s legacy marker to the admin dir, if it has one (#8511).
///
/// Why: the one-shot fleet migration and the doctor repair need the outcome,
/// not the bytes.
/// Test: `a_legacy_marker_is_read_then_migrated`.
pub(crate) fn migrate_legacy_sentinel(worktree: &Path) -> MarkerMigration {
    resolve_with(worktree, &copy_new).1
}

/// The tolerant form of [`read_sentinel_bytes_strict`] — any failure reads as
/// no marker — with an injectable copy step, so a test can make the copy lie
/// (#8511).
/// Test: `a_copy_that_does_not_verify_keeps_the_legacy_marker`.
pub(crate) fn resolve_with(
    worktree: &Path,
    copy: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> (Option<Vec<u8>>, MarkerMigration) {
    let (found, outcome) = resolve_strict_with(worktree, copy);
    (found.ok().flatten().map(|(_, bytes)| bytes), outcome)
}

/// A marker that exists but could not be read, or does not parse (#8511).
///
/// Why: a reclaim gate must keep a tree whose marker is present but
/// unreadable, yet may permit one with no marker; the tolerant reader answers
/// both as owner-unknown, so the strict reader names the difference.
/// Test: `strict_read_separates_missing_from_unreadable_and_corrupt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnerReadError {
    /// The marker exists at `path` but reading it failed.
    Unreadable { path: PathBuf, reason: String },
    /// The marker at `path` was read but is not a valid ownership payload.
    Corrupt { path: PathBuf, reason: String },
}

/// The marker's location and bytes, or `Ok(None)` when neither location holds
/// one — the module doc's decision table (#8511).
/// Test: `a_legacy_marker_is_read_then_migrated`,
/// `the_admin_marker_wins_when_both_exist`,
/// `a_failed_migration_keeps_the_legacy_marker`.
pub(crate) fn read_sentinel_bytes_strict(
    worktree: &Path,
) -> Result<Option<(PathBuf, Vec<u8>)>, OwnerReadError> {
    resolve_strict_with(worktree, &copy_new).0
}

/// Read `path`: `Ok(None)` when absent, `Err` for any other failure.
fn read_present(path: &Path) -> Result<Option<Vec<u8>>, OwnerReadError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(OwnerReadError::Unreadable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
}

/// Where the winning marker was found and its bytes, `Ok(None)` for no marker.
type Found = Result<Option<(PathBuf, Vec<u8>)>, OwnerReadError>;

/// The one read path: the module doc's decision table, with the unreadable
/// case kept apart from the missing one.
fn resolve_strict_with(
    worktree: &Path,
    copy: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> (Found, MarkerMigration) {
    let legacy = legacy_sentinel_path(worktree);
    let Some(admin) = admin_sentinel_path(worktree) else {
        let found = read_present(&legacy).map(|b| b.map(|b| (legacy, b)));
        return (found, MarkerMigration::NoAdminDir);
    };
    match read_present(&admin) {
        Ok(Some(bytes)) => {
            let outcome = settle_leftover_legacy(&legacy, &bytes, worktree);
            (Ok(Some((admin, bytes))), outcome)
        }
        Ok(None) => {
            let legacy_bytes = match read_present(&legacy) {
                Ok(Some(b)) => b,
                Ok(None) => return (Ok(None), MarkerMigration::NoLegacy),
                Err(e) => return (Err(e), MarkerMigration::NoLegacy),
            };
            if legacy_is_tracked(worktree) {
                return (Ok(Some((legacy, legacy_bytes))), MarkerMigration::Tracked);
            }
            let outcome = migrate_bytes(&legacy, &admin, &legacy_bytes, copy);
            if let MarkerMigration::Failed(reason) = &outcome {
                tracing::warn!(
                    worktree = %worktree.display(),
                    "ownership marker: legacy marker kept in the tree — {reason} (#8511)"
                );
            }
            (Ok(Some((legacy, legacy_bytes))), outcome)
        }
        Err(e) => {
            tracing::warn!(
                worktree = %worktree.display(),
                "ownership marker: the admin-dir marker cannot be read ({e:?}); owner unknown (#8511)"
            );
            let reason = format!("admin marker unreadable: {e:?}");
            (Err(e), MarkerMigration::Failed(reason))
        }
    }
}

/// Read and STRICTLY parse `worktree`'s ownership marker (#8511).
///
/// Why: see [`OwnerReadError`]. [`super::worktree_ownership::read_sentinel_owner`]
/// stays tolerant for every existing caller.
/// What: admin dir first, legacy second, with the same migration as the
/// tolerant read. `Ok(None)` — no marker in either location. `Ok(Some(owner))`
/// — a marker is present and parses; an empty (pre-#3649) marker or one naming
/// no owner is `Ok(Some(SentinelOwner::Unknown))`, which a caller must treat as
/// PRESENT. `Err(Unreadable)` — the winning location exists but cannot be read.
/// `Err(Corrupt)` — it was read but is not a valid payload.
/// Test: `strict_read_separates_missing_from_unreadable_and_corrupt`.
pub(crate) fn read_sentinel_owner_strict(
    worktree: &Path,
) -> Result<Option<SentinelOwner>, OwnerReadError> {
    let Some((path, bytes)) = read_sentinel_bytes_strict(worktree)? else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(Some(SentinelOwner::Unknown));
    }
    serde_json::from_slice::<WorktreeSentinel>(&bytes)
        .map(|payload| Some(owner_from_payload(payload)))
        .map_err(|e| OwnerReadError::Corrupt {
            path,
            reason: e.to_string(),
        })
}

/// Both locations may hold a marker: finish an interrupted migration, or log a
/// conflict the admin marker wins.
fn settle_leftover_legacy(legacy: &Path, admin_bytes: &[u8], worktree: &Path) -> MarkerMigration {
    let Ok(legacy_bytes) = std::fs::read(legacy) else {
        return MarkerMigration::NoLegacy;
    };
    if legacy_is_tracked(worktree) {
        return MarkerMigration::Tracked;
    }
    if legacy_bytes != admin_bytes {
        tracing::warn!(
            worktree = %worktree.display(),
            "ownership marker: the admin-dir and in-tree markers differ; the admin-dir \
             marker wins and both are kept (#8511)"
        );
        return MarkerMigration::Conflict;
    }
    match std::fs::remove_file(legacy) {
        Ok(()) => MarkerMigration::DuplicateRemoved,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MarkerMigration::DuplicateRemoved,
        Err(e) => MarkerMigration::Failed(format!("could not remove the duplicate: {e}")),
    }
}

/// Copy, verify byte for byte, then remove the legacy marker — the Fail-Open
/// order (#8511). Any failure before the removal deletes only the admin copy.
fn migrate_bytes(
    legacy: &Path,
    admin: &Path,
    bytes: &[u8],
    copy: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> MarkerMigration {
    if let Err(e) = copy(admin, bytes) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            // A writer won the race; its marker is authoritative.
            return MarkerMigration::Conflict;
        }
        let _ = std::fs::remove_file(admin);
        return MarkerMigration::Failed(format!("copy to {} failed: {e}", admin.display()));
    }
    match std::fs::read(admin) {
        Ok(copied) if copied == bytes => {}
        Ok(_) => {
            let _ = std::fs::remove_file(admin);
            return MarkerMigration::Failed("the copy did not verify byte for byte".to_string());
        }
        Err(e) => {
            let _ = std::fs::remove_file(admin);
            return MarkerMigration::Failed(format!("the copy could not be read back: {e}"));
        }
    }
    match std::fs::remove_file(legacy) {
        Ok(()) => MarkerMigration::Migrated,
        // Both copies now match; the next read removes the duplicate.
        Err(e) => MarkerMigration::Failed(format!("verified copy made, legacy not removed: {e}")),
    }
}

/// Write `bytes` as `worktree`'s ownership marker (#8511).
///
/// Why: every marker writer — the agent claim, session provisioning, adoption —
/// goes through here, so a new marker never lands in the working tree.
/// What: writes the admin-dir marker when the tree has one, then removes any
/// legacy in-tree marker unless git tracks it (the new write supersedes it; a
/// failed removal is logged, and the admin marker still wins on read). With no admin dir it
/// writes the in-tree path. Write errors propagate.
/// Test: `the_agent_marker_is_written_to_the_admin_dir`.
pub(crate) fn write_sentinel_bytes(worktree: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let legacy = legacy_sentinel_path(worktree);
    let Some(admin) = admin_sentinel_path(worktree) else {
        return std::fs::write(legacy, bytes);
    };
    std::fs::write(&admin, bytes)?;
    if !legacy.exists() || legacy_is_tracked(worktree) {
        return Ok(());
    }
    match std::fs::remove_file(&legacy) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            worktree = %worktree.display(),
            "ownership marker: written to the admin dir, but the old in-tree marker could \
             not be removed ({e}) (#8511)"
        ),
    }
    Ok(())
}

#[cfg(test)]
#[path = "worktree_ownership_location_tests.rs"]
mod worktree_ownership_location_tests;
