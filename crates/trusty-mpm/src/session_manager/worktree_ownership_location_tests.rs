//! Unit tests for the marker-location module (#8511).
//!
//! Why: the path resolution must agree with git, the existence check must see
//! either location, and a copy that does not verify must never cost the legacy
//! marker.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{SentinelOwner, WorktreeSentinel};

fn git_path(dir: &Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-path", "trusty-mpm-worktree"])
        .output()
        .expect("spawn git");
    assert!(out.status.success());
    let raw = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if raw.is_absolute() {
        raw
    } else {
        dir.join(raw)
    }
}

/// Linked worktree, main checkout and a plain directory all resolve as git does.
#[test]
fn admin_path_matches_git_rev_parse_git_path() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("path");
    assert_eq!(admin_sentinel_path(&wt), Some(git_path(&wt)));
    assert_eq!(admin_sentinel_path(&fx.repo), Some(git_path(&fx.repo)));
    let plain = tempfile::tempdir().expect("tempdir");
    assert_eq!(admin_sentinel_path(plain.path()), None);
}

/// The removal gate still admits a tree whose only marker is in the admin dir.
#[test]
fn a_tree_marked_only_in_the_admin_dir_is_still_marked() {
    let fx = GitWorktreeFixture::new();
    let parked = fx.repos_root.join("elsewhere");
    let wt = fx.add_worktree_at(&parked, "marked");
    assert!(!super::super::decommission::removal_permitted(&wt));
    write_sentinel_bytes(&wt, b"{}").expect("write");
    assert!(!legacy_sentinel_path(&wt).exists());
    assert!(sentinel_present(&wt));
    assert!(super::super::decommission::removal_permitted(&wt));
}

/// #8368: a marker a branch already committed stays in place — through a read,
/// a migration, and a new write — so the tree never gains a deleted file.
#[test]
fn a_committed_legacy_marker_is_never_removed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("committed");
    let legacy = legacy_sentinel_path(&wt);
    std::fs::write(&legacy, b"{}").expect("write legacy");
    GitWorktreeFixture::commit_all_and_push(&wt, "commit the marker");
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Tracked);
    write_sentinel_bytes(&wt, b"{\"new\":1}").expect("write");
    assert!(legacy.exists(), "a tracked marker was removed");
    assert_eq!(
        read_sentinel_bytes_strict(&wt)
            .expect("readable")
            .map(|(_, b)| b)
            .as_deref(),
        Some(&b"{\"new\":1}"[..])
    );
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .expect("status");
    assert!(
        status.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// Every temp copy the migration left beside `admin`.
fn temp_leftovers(admin: &Path) -> Vec<PathBuf> {
    let prefix = format!("{ADMIN_SENTINEL_NAME}.migrate-");
    std::fs::read_dir(admin.parent().expect("admin dir"))
        .expect("read admin dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .collect()
}

/// A copy that writes the wrong bytes, or fails outright, leaves the legacy
/// marker, no admin marker and no temp file behind.
#[test]
fn a_copy_that_does_not_verify_keeps_the_legacy_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("verify");
    let legacy = legacy_sentinel_path(&wt);
    let admin = admin_sentinel_path(&wt).expect("admin path");
    std::fs::write(&legacy, b"{\"owner\":1}").expect("write legacy");

    let truncating = |p: &Path, b: &[u8]| std::fs::write(p, &b[..b.len() - 1]);
    let failing = |_: &Path, _: &[u8]| Err(std::io::Error::other("disk full"));
    type Copy = dyn Fn(&Path, &[u8]) -> std::io::Result<()>;
    let copies: [&Copy; 2] = [&truncating, &failing];
    for copy in copies {
        let outcome = migrate_with(&wt, copy);
        assert!(matches!(outcome, MarkerMigration::Failed(_)), "{outcome:?}");
        assert_eq!(
            std::fs::read(&legacy).ok().as_deref(),
            Some(&b"{\"owner\":1}"[..])
        );
        assert!(!admin.exists(), "an unverified admin copy was left behind");
        assert!(
            temp_leftovers(&admin).is_empty(),
            "a temp copy was left behind"
        );
    }
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Migrated);
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::NoLegacy);
}

/// Finding 1 (#8511 review): a writer that lands its admin marker while the
/// migration is copying keeps it — the migration never deletes or replaces it.
#[test]
fn a_concurrent_writers_marker_survives_the_migration() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("race-writer");
    let legacy = legacy_sentinel_path(&wt);
    let admin = admin_sentinel_path(&wt).expect("admin path");
    std::fs::write(&legacy, b"A").expect("write legacy");
    // The copy writes A where it is told to, then the writer overwrites the
    // admin marker with B before the migration looks again.
    let racing = |p: &Path, b: &[u8]| {
        std::fs::write(p, b)?;
        std::fs::write(&admin, b"B")
    };
    let outcome = migrate_with(&wt, &racing);
    assert_eq!(
        std::fs::read(&admin).ok().as_deref(),
        Some(&b"B"[..]),
        "the writer's marker was lost ({outcome:?})"
    );
    assert!(
        legacy.exists(),
        "the legacy marker was removed on a conflict"
    );
    assert!(
        temp_leftovers(&admin).is_empty(),
        "a temp copy was left behind"
    );
}

/// Finding 2 (#8511 review): a migration that runs between the reader's two
/// reads cannot make a marked tree read as unmarked.
#[test]
fn a_migration_between_the_two_reads_still_finds_the_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("race-reader");
    let bytes = agent_payload("r1");
    std::fs::write(legacy_sentinel_path(&wt), &bytes).expect("write legacy");
    let found = read_both_with(&wt, &|| {
        assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Migrated);
    });
    assert_eq!(
        found.expect("readable").map(|(_, b)| b),
        Some(bytes),
        "a marked tree read as unmarked"
    );
}

/// Finding 5 (#8511 review): an unreadable legacy marker is a reported
/// failure, never a silent "nothing to move".
#[test]
fn an_unreadable_legacy_marker_fails_the_migration() {
    let fx = GitWorktreeFixture::new();
    for (name, with_admin) in [("unreadable-alone", false), ("unreadable-both", true)] {
        let wt = fx.add_worktree(name);
        let legacy = legacy_sentinel_path(&wt);
        std::fs::write(&legacy, b"{}").expect("write legacy");
        if with_admin {
            std::fs::write(admin_sentinel_path(&wt).expect("admin"), b"{}").expect("write admin");
        }
        let _restore = crate::session_manager::worktree_git_fixture::deny_all(&legacy);
        if std::fs::read(&legacy).is_ok() {
            eprintln!("skipped {name}: a mode-000 file is readable here (running as root?)");
            continue;
        }
        let outcome = migrate_legacy_sentinel(&wt);
        assert!(
            matches!(outcome, MarkerMigration::Failed(_)),
            "{name}: {outcome:?}"
        );
        assert!(legacy.exists(), "{name}: the unreadable marker was removed");
    }
}

/// A valid agent payload for the strict-reader matrix.
fn agent_payload(id: &str) -> Vec<u8> {
    let owner = crate::session_manager::worktree_ownership::AgentWorktreeOwner {
        agent_id: id.to_string(),
        delegation_id: crate::core::agent::DelegationId::new(),
        parent_session_id: crate::core::session::SessionId::new(),
    };
    serde_json::to_vec(&WorktreeSentinel::for_agent(owner)).expect("serialize")
}

fn strict_agent(wt: &Path) -> Option<String> {
    match read_sentinel_owner_strict(wt) {
        Ok(Some(SentinelOwner::Agent(o, _))) => Some(o.agent_id),
        other => panic!("expected a valid agent owner, got {other:?}"),
    }
}

/// Missing, valid, empty, corrupt and unreadable — at both locations — each
/// get their own answer; the tolerant reader still folds the failures into
/// owner-unknown.
#[test]
fn strict_read_separates_missing_from_unreadable_and_corrupt() {
    use crate::session_manager::worktree_ownership::read_sentinel_owner;
    let fx = GitWorktreeFixture::new();

    let wt = fx.add_worktree("strict-missing");
    assert_eq!(read_sentinel_owner_strict(&wt), Ok(None));

    // Valid, admin location.
    let wt = fx.add_worktree("strict-valid-admin");
    let admin = admin_sentinel_path(&wt).expect("admin");
    std::fs::write(&admin, agent_payload("a1")).expect("write");
    assert_eq!(strict_agent(&wt).as_deref(), Some("a1"));

    // Valid, legacy location: answered, and the read writes nothing (finding 3).
    let wt = fx.add_worktree("strict-valid-legacy");
    std::fs::write(legacy_sentinel_path(&wt), agent_payload("l1")).expect("write");
    assert_eq!(strict_agent(&wt).as_deref(), Some("l1"));
    assert!(matches!(read_sentinel_owner(&wt), SentinelOwner::Agent(o, _) if o.agent_id == "l1"));
    assert!(
        legacy_sentinel_path(&wt).exists(),
        "a read moved the marker"
    );
    assert!(
        !admin_sentinel_path(&wt).expect("admin").exists(),
        "a read wrote the admin marker"
    );

    // Empty (pre-#3649): present, naming no owner.
    let wt = fx.add_worktree("strict-empty");
    std::fs::write(legacy_sentinel_path(&wt), b"").expect("write");
    assert_eq!(
        read_sentinel_owner_strict(&wt),
        Ok(Some(SentinelOwner::Unknown))
    );

    // Corrupt, at each location.
    for (name, in_admin) in [
        ("strict-corrupt-admin", true),
        ("strict-corrupt-legacy", false),
    ] {
        let wt = fx.add_worktree(name);
        let path = if in_admin {
            admin_sentinel_path(&wt).expect("admin")
        } else {
            legacy_sentinel_path(&wt)
        };
        std::fs::write(&path, b"{not json").expect("write");
        match read_sentinel_owner_strict(&wt) {
            Err(OwnerReadError::Corrupt { path: p, .. }) => assert_eq!(p, path),
            other => panic!("{name}: expected Corrupt, got {other:?}"),
        }
        assert_eq!(read_sentinel_owner(&wt), SentinelOwner::Unknown);
    }

    // Unreadable, at each location.
    for (name, in_admin) in [
        ("strict-denied-admin", true),
        ("strict-denied-legacy", false),
    ] {
        let wt = fx.add_worktree(name);
        let path = if in_admin {
            admin_sentinel_path(&wt).expect("admin")
        } else {
            legacy_sentinel_path(&wt)
        };
        std::fs::write(&path, agent_payload("locked")).expect("write");
        let _restore = crate::session_manager::worktree_git_fixture::deny_all(&path);
        if std::fs::read(&path).is_ok() {
            eprintln!("skipped {name}: a mode-000 file is readable here (running as root?)");
            continue;
        }
        match read_sentinel_owner_strict(&wt) {
            Err(OwnerReadError::Unreadable { path: p, .. }) => assert_eq!(p, path),
            other => panic!("{name}: expected Unreadable, got {other:?}"),
        }
        assert_eq!(read_sentinel_owner(&wt), SentinelOwner::Unknown);
        assert!(path.exists(), "{name}: an unreadable marker was removed");
    }
}

/// Round 2 finding 1 (#8511): a migrated tree whose `.git` pointer dangles (the
/// main checkout moved) or cannot be read is protected, and the strict reader
/// calls its marker unreadable, never missing.
#[test]
fn a_migrated_tree_with_an_unresolvable_git_entry_stays_protected() {
    let fx = GitWorktreeFixture::new();
    let parked = fx.repos_root.join("elsewhere");
    let names = trusty_common::workspace_layout::WorktreeDirNames::from_configured(None);
    for (name, dangling) in [("dangling-gitdir", true), ("denied-gitdir", false)] {
        let wt = fx.add_worktree_at(&parked, name);
        write_sentinel_bytes(&wt, &agent_payload(name)).expect("write");
        assert!(!legacy_sentinel_path(&wt).exists(), "{name}: not migrated");
        let dot_git = wt.join(".git");
        let _restore = if dangling {
            let gone = fx.repos_root.join("moved").join(".git").join("worktrees");
            std::fs::write(&dot_git, format!("gitdir: {}\n", gone.join(name).display()))
                .expect("rewrite .git");
            None
        } else {
            let guard = crate::session_manager::worktree_git_fixture::deny_all(&dot_git);
            if std::fs::read(&dot_git).is_ok() {
                eprintln!("skipped {name}: a mode-000 file is readable here (running as root?)");
                continue;
            }
            Some(guard)
        };
        let strict = read_sentinel_owner_strict(&wt);
        assert!(
            matches!(strict, Err(OwnerReadError::Unreadable { .. })),
            "{name}: {strict:?}"
        );
        assert!(
            super::super::retention::workspace_needs_protection(Some(&wt), &names, |p| {
                p.try_exists()
            }),
            "{name}: a marked tree lost its protection"
        );
    }
}

/// Round 2 finding 5 (#8511): a duplicate legacy marker that cannot be removed
/// is a failure logged for the tree, like every other failed step.
#[test]
fn a_failed_duplicate_removal_is_logged_for_the_tree() {
    use std::os::unix::fs::PermissionsExt;
    use tracing_subscriber::layer::SubscriberExt;
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stuck-duplicate");
    let legacy = legacy_sentinel_path(&wt);
    std::fs::write(&legacy, b"{}").expect("write legacy");
    std::fs::write(admin_sentinel_path(&wt).expect("admin"), b"{}").expect("write admin");
    let mode = std::fs::metadata(&wt).expect("stat").permissions().mode();
    std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let outcome = tracing::subscriber::with_default(subscriber, || migrate_legacy_sentinel(&wt));
    std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(mode)).expect("restore");
    if !legacy.exists() {
        eprintln!("skipped: a read-only directory allowed the removal (running as root?)");
        return;
    }
    assert!(matches!(outcome, MarkerMigration::Failed(_)), "{outcome:?}");
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("legacy marker kept")),
        "no per-tree warning: {lines:?}"
    );
}

/// Round 2 finding 2 (#8511): a concurrent migration that links the same bytes
/// first settles the tree — no conflict, and the leftover legacy copy goes.
#[test]
fn identical_bytes_from_a_concurrent_migration_settle_cleanly() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("race-same-bytes");
    let legacy = legacy_sentinel_path(&wt);
    let admin = admin_sentinel_path(&wt).expect("admin path");
    std::fs::write(&legacy, b"A").expect("write legacy");
    let racing = |p: &Path, b: &[u8]| {
        std::fs::write(p, b)?;
        std::fs::write(&admin, b)
    };
    assert_eq!(
        migrate_with(&wt, &racing),
        MarkerMigration::DuplicateRemoved
    );
    assert!(!legacy.exists(), "the settled legacy copy was kept");
    assert_eq!(std::fs::read(&admin).ok().as_deref(), Some(&b"A"[..]));
    assert!(
        temp_leftovers(&admin).is_empty(),
        "a temp copy was left behind"
    );
}
