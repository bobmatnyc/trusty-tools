//! The ownership marker lives in the git admin dir (#8511, #8368).
//!
//! Why: these tests drive the ownership API that predates #8511
//! (`write_agent_sentinel`, `read_sentinel_owner`) plus git itself, and the one
//! explicit migration entry point (`migrate_legacy_sentinel`) — readers never
//! move a marker.
//! What: real git worktrees from [`GitWorktreeFixture`]; the admin-dir path is
//! always taken from `git rev-parse --git-path trusty-mpm-worktree`, never
//! computed by the code under test.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership::{
    AgentWorktreeOwner, SentinelOwner, WorktreeSentinel, read_sentinel_owner, write_agent_sentinel,
};
use super::worktree_ownership_location::{MarkerMigration, migrate_legacy_sentinel};
use crate::core::agent::DelegationId;
use crate::core::session::SessionId;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git")
}

fn stdout(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Git's own answer for where the marker belongs.
fn git_path(wt: &Path) -> PathBuf {
    let raw = PathBuf::from(stdout(
        wt,
        &["rev-parse", "--git-path", "trusty-mpm-worktree"],
    ));
    if raw.is_absolute() { raw } else { wt.join(raw) }
}

fn agent(id: &str) -> AgentWorktreeOwner {
    AgentWorktreeOwner {
        agent_id: id.to_string(),
        delegation_id: DelegationId::new(),
        parent_session_id: SessionId::new(),
    }
}

fn payload(owner: &AgentWorktreeOwner) -> Vec<u8> {
    serde_json::to_vec(&WorktreeSentinel::for_agent(owner.clone())).expect("serialize")
}

fn agent_of(wt: &Path) -> Option<String> {
    match read_sentinel_owner(wt) {
        SentinelOwner::Agent(o, _) => Some(o.agent_id),
        _ => None,
    }
}

/// The write lands at git's admin-dir path and the tree stays clean.
#[test]
fn the_agent_marker_is_written_to_the_admin_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("write");
    write_agent_sentinel(&wt, agent("agent-w")).expect("write");
    assert!(
        git_path(&wt).is_file(),
        "no marker at git's --git-path location"
    );
    assert!(
        !wt.join(WORKTREE_SENTINEL_FILE).exists(),
        "marker written into the tree"
    );
    assert_eq!(
        stdout(&wt, &["status", "--porcelain"]),
        "",
        "the marker dirtied the tree"
    );
    assert_eq!(agent_of(&wt).as_deref(), Some("agent-w"));
}

/// A legacy in-tree marker is read in place; the explicit migration then
/// copies it byte for byte and removes it.
#[test]
fn a_legacy_marker_is_read_then_migrated() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("legacy");
    let bytes = payload(&agent("agent-l"));
    std::fs::write(wt.join(WORKTREE_SENTINEL_FILE), &bytes).expect("write legacy");
    assert_eq!(
        agent_of(&wt).as_deref(),
        Some("agent-l"),
        "ownership lost on read"
    );
    assert!(!git_path(&wt).exists(), "a read wrote the admin marker");
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Migrated);
    assert_eq!(
        std::fs::read(git_path(&wt)).ok(),
        Some(bytes),
        "copy missing or altered"
    );
    assert!(
        !wt.join(WORKTREE_SENTINEL_FILE).exists(),
        "legacy marker not removed"
    );
    assert_eq!(agent_of(&wt).as_deref(), Some("agent-l"));
    assert_eq!(stdout(&wt, &["status", "--porcelain"]), "");
}

/// Sets `path`'s mode, restoring the old one on drop.
struct Mode(PathBuf, u32);
impl Mode {
    fn set(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let old = std::fs::metadata(path).expect("stat").permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        Self(path.to_path_buf(), old)
    }
}
impl Drop for Mode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

/// A copy that cannot be made keeps the legacy marker and the owner; the next
/// migration, once the copy can succeed, moves it.
#[test]
fn a_failed_migration_keeps_the_legacy_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("readonly");
    let bytes = payload(&agent("agent-f"));
    let legacy = wt.join(WORKTREE_SENTINEL_FILE);
    std::fs::write(&legacy, &bytes).expect("write legacy");
    let admin = git_path(&wt);
    let admin_dir = admin.parent().expect("admin dir").to_path_buf();
    {
        let _ro = Mode::set(&admin_dir, 0o555);
        if std::fs::write(admin_dir.join("probe"), b"").is_ok() {
            eprintln!("skipped: a read-only directory is writable here (running as root?)");
            return;
        }
        let outcome = migrate_legacy_sentinel(&wt);
        assert!(matches!(outcome, MarkerMigration::Failed(_)), "{outcome:?}");
        assert_eq!(agent_of(&wt).as_deref(), Some("agent-f"), "ownership lost");
        assert_eq!(
            std::fs::read(&legacy).ok(),
            Some(bytes.clone()),
            "legacy marker lost"
        );
        assert!(!admin.exists(), "a partial admin copy was left behind");
    }
    assert_eq!(migrate_legacy_sentinel(&wt), MarkerMigration::Migrated);
    assert_eq!(agent_of(&wt).as_deref(), Some("agent-f"));
    assert!(
        !legacy.exists(),
        "the retry did not migrate once the copy could succeed"
    );
    assert_eq!(std::fs::read(&admin).ok(), Some(bytes));
}

/// With a marker in both places, the admin-dir marker names the owner.
#[test]
fn the_admin_marker_wins_when_both_exist() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("both");
    std::fs::write(git_path(&wt), payload(&agent("agent-admin"))).expect("write admin");
    std::fs::write(
        wt.join(WORKTREE_SENTINEL_FILE),
        payload(&agent("agent-legacy")),
    )
    .expect("write legacy");
    assert_eq!(agent_of(&wt).as_deref(), Some("agent-admin"));
    assert!(
        wt.join(WORKTREE_SENTINEL_FILE).exists(),
        "a conflicting legacy marker is kept, not deleted"
    );
}

/// Plain `git worktree remove` (no `--force`) accepts a marked tree.
#[test]
fn git_worktree_remove_succeeds_on_a_tree_with_an_admin_dir_marker() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("remove");
    write_agent_sentinel(&wt, agent("agent-r")).expect("write");
    let out = git(
        &fx.repo,
        &["worktree", "remove", wt.to_str().expect("utf8")],
    );
    assert!(
        out.status.success(),
        "git refused the removal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wt.exists());
}

/// #8368: `git add -A` in a fresh agent tree stages no marker.
#[test]
fn git_add_all_stages_no_marker_in_a_fresh_agent_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("add-all");
    write_agent_sentinel(&wt, agent("agent-a")).expect("write");
    stdout(&wt, &["add", "-A"]);
    assert_eq!(stdout(&wt, &["diff", "--cached", "--name-only"]), "");
}
