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

/// A copy that writes the wrong bytes, or fails outright, leaves the legacy
/// marker and no admin copy; the read still names the legacy bytes.
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
        let (bytes, outcome) = resolve_with(&wt, copy);
        assert!(matches!(outcome, MarkerMigration::Failed(_)), "{outcome:?}");
        assert_eq!(bytes.as_deref(), Some(&b"{\"owner\":1}"[..]));
        assert!(legacy.exists(), "legacy marker lost");
        assert!(!admin.exists(), "an unverified admin copy was left behind");
    }
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Migrated);
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::NoLegacy);
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

    // Valid, legacy location: answered, then migrated.
    let wt = fx.add_worktree("strict-valid-legacy");
    std::fs::write(legacy_sentinel_path(&wt), agent_payload("l1")).expect("write");
    assert_eq!(strict_agent(&wt).as_deref(), Some("l1"));
    assert!(!legacy_sentinel_path(&wt).exists());

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
