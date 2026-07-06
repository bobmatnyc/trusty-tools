//! Integration test: `decommission` removes a REAL on-disk git worktree (#1806).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! #1806-specific coverage lives here so neither file grows past its limit.
//! Keeping it in a focused sibling also makes the fix easy to locate. Uses
//! `FakeTmuxDriver` from the sibling `tests` module, mirroring the pattern
//! established by `backfill_tests.rs`.
//! What: creates a real git base clone + a real `git worktree add` under
//! `<base>/.worktrees/<id>`, decommissions the session via the public
//! `SessionManager` API, and asserts the worktree directory, git worktree
//! metadata, and branch ref are all gone.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::FakeTmuxDriver;

/// `decommission` on an in-project (`workspace_owned = false`) session whose
/// workspace is a REAL `git worktree` removes the worktree directory from
/// disk AND prunes the git worktree metadata + branch ref from the base clone
/// (#1806).
///
/// Why: #1806 reported that `tm session decommission` tombstones the record
/// but leaves the on-disk worktree directory behind, requiring a manual
/// `git worktree remove`. The `workspace_owned = false` + `.worktrees/`
/// exception (added for #1840/#1845) is unit-tested for the free function
/// `remove_session_worktree` in `decommission.rs`, but no test previously
/// exercised the FULL round trip through the public `SessionManager` API
/// with a real git repository — this closes that verification gap.
/// What: creates a real git base clone (`git init` + one empty commit), adds
/// a real `git worktree add` under `<base>/.worktrees/<id>` with the SM
/// ownership sentinel file (mirroring `create_session_worktree`), creates a
/// session record pointing at it with `workspace_owned = false`, calls
/// `decommission_with_root`, and asserts: (1) `workspace_removed` is true;
/// (2) the worktree directory is gone from disk; (3) `git worktree list`
/// on the base clone no longer references the removed path; (4) the session
/// branch ref was deleted.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_decommission_removes_real_git_worktree() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake)
        .await
        .expect("manager");

    // ── Real git base clone: init + one empty commit (needed for `worktree add`) ──
    let base_dir = TempDir::new().unwrap();
    let base = base_dir.path().to_path_buf();
    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(&base)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !git_init_ok {
        eprintln!("manager_decommission_removes_real_git_worktree: git unavailable, skipping");
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "config",
            "user.email",
            "ci@test.invalid",
        ])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C", base.to_str().unwrap(), "config", "user.name", "CI"])
        .status();
    let commit_ok = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !commit_ok {
        eprintln!("manager_decommission_removes_real_git_worktree: git commit failed, skipping");
        return;
    }

    // ── Real `git worktree add` under <base>/.worktrees/<session-name> ───────
    // Branch is `session/<session_name>` (issue #2032 fix) — this MUST mirror
    // the exact convention `create_session_worktree`/`worktree_branch_for`
    // use in production, or this fixture would silently re-encode the very
    // bug #2032 fixed (a pre-#2032 version of this fixture used the bare,
    // unprefixed name here, which happened to match `decommission.rs`'s
    // then-buggy `git branch -D <leaf>` and masked the missing-prefix bug).
    let session_name = "test-session-1806";
    let branch_name = crate::core::worktree_naming::worktree_branch_for(session_name);
    let worktrees_dir = base.join(".worktrees");
    std::fs::create_dir_all(&worktrees_dir).unwrap();
    let worktree_path = worktrees_dir.join(session_name);
    let add_ok = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "worktree",
            "add",
            "-b",
            &branch_name,
        ])
        .arg(&worktree_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(add_ok, "git worktree add must succeed in this test fixture");

    // Write the SM ownership sentinel, mirroring `create_session_worktree`.
    std::fs::write(worktree_path.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");

    // ── Create the session record: in-project worktree, workspace_owned=false ──
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(worktree_path.clone()),
            None,
            Some(worktree_path.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false, // owned: false — in-project worktree, not a full clone
        )
        .await
        .expect("create");

    // The managed root is irrelevant for the unowned/worktree branch, but the
    // API requires one — point it at an unrelated temp dir.
    let managed_root = TempDir::new().unwrap();
    let (tombstone, workspace_removed) = mgr
        .decommission_with_root(&record.id, managed_root.path())
        .await
        .expect("decommission");

    assert!(
        workspace_removed,
        "workspace_removed must be true for an in-project git worktree (#1806)"
    );
    assert_eq!(tombstone.state, ManagedSessionState::Decommissioned);
    assert!(
        !worktree_path.exists(),
        "the worktree directory must be removed from disk after decommission (#1806)"
    );

    // `git worktree list` on the base clone must no longer reference the path.
    let list_out = std::process::Command::new("git")
        .args(["-C", base.to_str().unwrap(), "worktree", "list"])
        .output()
        .expect("git worktree list");
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        !list_stdout.contains(session_name),
        "git worktree list must not reference the removed worktree; got: {list_stdout}"
    );

    // The session branch ref (`session/<name>`, #2032) must have been deleted.
    let branch_out = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "branch",
            "--list",
            &branch_name,
        ])
        .output()
        .expect("git branch --list");
    let branch_stdout = String::from_utf8_lossy(&branch_out.stdout);
    assert!(
        branch_stdout.trim().is_empty(),
        "the session branch ref must be deleted after decommission; got: {branch_stdout}"
    );
}
