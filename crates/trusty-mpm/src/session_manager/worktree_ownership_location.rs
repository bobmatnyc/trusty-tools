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
//! location.
//!
//! Readers never write. A read answers the admin marker when present, else the
//! legacy one. Only [`migrate_legacy_sentinel`] (the startup pass, registration
//! and `tm doctor --fix --yes`) and [`write_sentinel_bytes`] change the disk.
//! The migration's decision table:
//!
//! | on disk | migration does |
//! |---|---|
//! | admin only | nothing |
//! | legacy only | copy to a private temp file, verify, link it in as the admin marker, then remove legacy |
//! | both, same bytes | remove the leftover legacy copy |
//! | both, different bytes | log the conflict, keep both (the admin marker wins on read) |
//! | either unreadable | report a failure, keep both |
//! | legacy tracked by git | nothing — never removed (#8368) |
//! | no `.git` entry | nothing |
//!
//! Concurrency: readers, the startup pass, the doctor apply and agent claims
//! can all run at once. Two rules keep ownership from being lost:
//! - every writer creates the admin marker BEFORE it removes the legacy one,
//!   and every reader reads the legacy marker BEFORE the admin one, so no
//!   interleaving answers "no marker" for a tree that always had one;
//! - the migration never overwrites or deletes an admin marker: it links its
//!   verified temp file in with no-clobber semantics, so a concurrent writer's
//!   marker always survives.
//!
//! Test: `worktree_ownership_location_tests`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
/// What: legacy file exists, or the admin-dir file exists — legacy first, the
/// module doc's read order.
/// Test: `a_tree_marked_only_in_the_admin_dir_is_still_marked`.
pub(crate) fn sentinel_present(worktree: &Path) -> bool {
    legacy_sentinel_path(worktree).exists()
        || admin_sentinel_path(worktree).is_some_and(|p| p.exists())
}

/// What one migration of a tree's legacy marker did (#8511).
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
    /// Both existed with different bytes, or a writer created the admin
    /// marker mid-copy. The admin marker wins; both are kept.
    Conflict,
    /// The legacy marker is tracked by git (#8368), so it is left in place.
    Tracked,
    /// A step failed, or a marker could not be read. The legacy marker is
    /// kept, so ownership is not lost.
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

/// The marker's location and bytes, or `Ok(None)` when neither location holds
/// one (#8511). Reads only; never writes.
/// Test: `the_admin_marker_wins_when_both_exist`,
/// `a_migration_between_the_two_reads_still_finds_the_marker`.
pub(crate) fn read_sentinel_bytes_strict(worktree: &Path) -> Found {
    read_both_with(worktree, &|| {})
}

/// The one read path, with a hook between its two reads so a test can
/// interleave a migration or a write there.
///
/// Legacy is read FIRST (see the module doc): a writer removes legacy only
/// after the admin marker exists, so a legacy miss here means the admin read
/// that follows sees the marker.
fn read_both_with(worktree: &Path, between: &dyn Fn()) -> Found {
    let legacy = legacy_sentinel_path(worktree);
    let legacy_read = read_present(&legacy);
    let Some(admin) = admin_sentinel_path(worktree) else {
        return legacy_read.map(|b| b.map(|b| (legacy, b)));
    };
    between();
    match read_present(&admin) {
        Ok(Some(bytes)) => Ok(Some((admin, bytes))),
        Ok(None) => legacy_read.map(|b| b.map(|b| (legacy, b))),
        Err(e) => {
            tracing::warn!(
                worktree = %worktree.display(),
                "ownership marker: the admin-dir marker cannot be read ({e:?}); owner unknown (#8511)"
            );
            Err(e)
        }
    }
}

/// Read and STRICTLY parse `worktree`'s ownership marker (#8511).
///
/// Why: see [`OwnerReadError`]. [`super::worktree_ownership::read_sentinel_owner`]
/// stays tolerant for every existing caller.
/// What: [`read_sentinel_bytes_strict`], then a parse; never writes.
/// `Ok(None)` — no marker in either location. `Ok(Some(owner))` — a marker is
/// present and parses; an empty (pre-#3649) marker or one naming no owner is
/// `Ok(Some(SentinelOwner::Unknown))`, which a caller must treat as PRESENT.
/// `Err(Unreadable)` — the winning location exists but cannot be read.
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

/// Writes `bytes` to a NEW file at `path` — the production migration copy step.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Move `worktree`'s legacy marker to the admin dir, if it has one (#8511).
///
/// Why: the one-shot fleet migration, registration and the doctor repair are
/// the only paths allowed to move a marker; readers never do.
/// What: the module doc's decision table. Never overwrites or deletes an admin
/// marker.
/// Test: `a_legacy_marker_is_read_then_migrated`,
/// `a_failed_migration_keeps_the_legacy_marker`.
pub(crate) fn migrate_legacy_sentinel(worktree: &Path) -> MarkerMigration {
    migrate_with(worktree, &write_new)
}

/// [`migrate_legacy_sentinel`] with an injectable copy step, so a test can make
/// the copy lie or race a writer.
/// Test: `a_copy_that_does_not_verify_keeps_the_legacy_marker`,
/// `a_concurrent_writers_marker_survives_the_migration`.
fn migrate_with(
    worktree: &Path,
    copy: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> MarkerMigration {
    let legacy = legacy_sentinel_path(worktree);
    let Some(admin) = admin_sentinel_path(worktree) else {
        return MarkerMigration::NoAdminDir;
    };
    let legacy_bytes = match read_present(&legacy) {
        Ok(Some(b)) => b,
        Ok(None) => return MarkerMigration::NoLegacy,
        // An unreadable legacy marker is a failure the operator must see.
        Err(e) => return failed(worktree, format!("the in-tree marker is unreadable: {e:?}")),
    };
    if legacy_is_tracked(worktree) {
        return MarkerMigration::Tracked;
    }
    match read_present(&admin) {
        Ok(Some(admin_bytes)) => {
            settle_leftover_legacy(&legacy, &legacy_bytes, &admin_bytes, worktree)
        }
        Ok(None) => match migrate_bytes(&legacy, &admin, &legacy_bytes, copy) {
            MarkerMigration::Failed(reason) => failed(worktree, reason),
            outcome => outcome,
        },
        Err(e) => failed(
            worktree,
            format!("the admin-dir marker is unreadable: {e:?}"),
        ),
    }
}

/// Log and return [`MarkerMigration::Failed`].
fn failed(worktree: &Path, reason: String) -> MarkerMigration {
    tracing::warn!(
        worktree = %worktree.display(),
        "ownership marker: legacy marker kept in the tree — {reason} (#8511)"
    );
    MarkerMigration::Failed(reason)
}

/// Both locations hold a marker: finish an interrupted migration, or log a
/// conflict the admin marker wins.
fn settle_leftover_legacy(
    legacy: &Path,
    legacy_bytes: &[u8],
    admin_bytes: &[u8],
    worktree: &Path,
) -> MarkerMigration {
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

/// A fresh temp-file path beside `admin`, unique within and across processes.
fn temp_beside(admin: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!(
        "{ADMIN_SENTINEL_NAME}.migrate-{}-{nanos}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    admin.with_file_name(name)
}

/// Copy to a private temp file, verify it byte for byte, link it in as the
/// admin marker, then remove the legacy marker (#8511).
///
/// The verification runs on a file only this call knows, and the link fails
/// rather than replace an existing admin marker — so this never overwrites or
/// deletes a concurrent writer's marker. Any failure removes only the temp
/// file and keeps the legacy marker.
fn migrate_bytes(
    legacy: &Path,
    admin: &Path,
    bytes: &[u8],
    copy: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> MarkerMigration {
    let temp = temp_beside(admin);
    let staged = copy(&temp, bytes)
        .map_err(|e| format!("copy to {} failed: {e}", temp.display()))
        .and_then(|()| match std::fs::read(&temp) {
            Ok(copied) if copied == bytes => Ok(()),
            Ok(_) => Err("the copy did not verify byte for byte".to_string()),
            Err(e) => Err(format!("the copy could not be read back: {e}")),
        });
    let linked = staged.and_then(|()| match std::fs::hard_link(&temp, admin) {
        Ok(()) => Ok(true),
        // A writer created the admin marker first; it is authoritative.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("could not link {}: {e}", admin.display())),
    });
    if let Err(e) = std::fs::remove_file(&temp)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(temp = %temp.display(), "ownership marker: temp copy not removed ({e}) (#8511)");
    }
    match linked {
        Err(reason) => MarkerMigration::Failed(reason),
        Ok(false) => MarkerMigration::Conflict,
        Ok(true) => match std::fs::remove_file(legacy) {
            Ok(()) => MarkerMigration::Migrated,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => MarkerMigration::Migrated,
            // Both copies now match; the next migration removes the duplicate.
            Err(e) => {
                MarkerMigration::Failed(format!("verified copy made, legacy not removed: {e}"))
            }
        },
    }
}

/// Write `bytes` as `worktree`'s ownership marker (#8511).
///
/// Why: every marker writer — the agent claim, session provisioning, adoption —
/// goes through here, so a new marker never lands in the working tree.
/// What: writes the admin-dir marker when the tree has one, THEN removes any
/// legacy in-tree marker unless git tracks it (the new write supersedes it; a
/// failed removal is logged, and the admin marker still wins on read). The
/// order is the module doc's writer rule. With no admin dir it writes the
/// in-tree path. Write errors propagate.
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
