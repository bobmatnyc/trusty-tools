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
    let dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake)
        .await
        .expect("manager");

    // ── Real git base clone: init + one empty commit (needed for `worktree add`) ──
    let base_dir = crate::test_support::hermetic_temp_dir();
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

    // ── Give the base clone a remote and push (the #4400-style dirty gate) ──
    // Without ANY remote, `worktree_safety::inspect_dirt`'s unpushed-commit
    // check treats every commit as unpushed (there is nothing to exclude via
    // `--not --remotes`), which would make this "clean happy path" fixture
    // spuriously dirty and refuse removal. Push to a real bare remote so the
    // ONLY thing under test in this function is the ordinary clean-removal
    // path; the dirty-refusal path has its own fixture/test below.
    push_to_bare_remote(&base);

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
    let managed_root = crate::test_support::hermetic_temp_dir();
    let (tombstone, workspace_removed) = mgr
        .decommission_with_root(&record.id, managed_root.path(), None)
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

/// Push `repo`'s current `HEAD` to a fresh bare remote and fetch it back.
///
/// Why: `worktree_safety::inspect_dirt`'s unpushed-commit check treats a
/// repository with NO remote at all as fully unpushed (there is nothing for
/// `--not --remotes` to exclude), which would make a "clean happy path"
/// fixture built with a bare `git init` + one commit spuriously dirty. Tests
/// that need a genuinely CLEAN worktree call this immediately after the base
/// clone's first commit.
/// What: `git init --bare` a throwaway remote, `remote add origin`, `push
/// origin HEAD`, `fetch origin` — panics with git's stderr on any failure.
/// Test: `manager_decommission_removes_real_git_worktree`.
fn push_to_bare_remote(repo: &std::path::Path) {
    let remote_dir = crate::test_support::hermetic_temp_dir();
    let remote = remote_dir.path().to_path_buf();
    let remote_str = remote.to_str().expect("utf8 remote path");
    let repo_str = repo.to_str().expect("utf8 repo path");

    let init_ok = std::process::Command::new("git")
        .args(["init", "--bare"])
        .current_dir(&remote)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(init_ok, "git init --bare must succeed for the test remote");

    for args in [
        vec!["-C", repo_str, "remote", "add", "origin", remote_str],
        vec!["-C", repo_str, "push", "origin", "HEAD"],
        vec!["-C", repo_str, "fetch", "origin"],
    ] {
        let ok = std::process::Command::new("git")
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} must succeed");
    }
}

/// `decommission` refuses to remove an in-project worktree that holds unsaved
/// (uncommitted/untracked) work — the data-loss fix.
///
/// Why: before this fix, `remove_session_worktree` ran `git worktree remove
/// --force` (falling back to `fs::remove_dir_all`) with NO dirty check at
/// all. `tm sessions prune --state stopped` calls `decommission` for every
/// matching record, so a stopped session with uncommitted/untracked work in
/// its in-project worktree was silently destroyed. This reuses
/// `worktree_safety::inspect_dirt` (the same check the orphan-worktree sweep
/// uses) to gate removal.
/// What: builds a real, clean, pushed git worktree (so the ONLY dirt is the
/// file this test adds), drops an untracked file into it, then decommissions.
/// Asserts: (1) `workspace_removed` is `false`; (2) the record still
/// tombstones to `Decommissioned` (consistent with every other "skip
/// removal" branch in `decommission_with_root`); (3) the worktree directory
/// AND the untracked file both survive on disk; (4) the tombstone record
/// KEEPS `workspace_path` pointing at the retained directory (#4344 review) —
/// nulling it here would strand the retained work with nothing but a
/// transient warn! log line as a trail back to it.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_decommission_refuses_dirty_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake)
        .await
        .expect("manager");

    let fixture = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let session_name = "test-session-dirty";
    let worktree_path = fixture.add_worktree(session_name);

    // Write the SM ownership sentinel, mirroring `create_session_worktree`.
    std::fs::write(worktree_path.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");

    // Dirty the worktree: an untracked file `git status` will report. This is
    // the ONLY source of dirt — the fixture's base repo is already pushed.
    std::fs::write(
        worktree_path.join("uncommitted-work.txt"),
        b"unsaved work\n",
    )
    .expect("write uncommitted file");

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

    let managed_root = crate::test_support::hermetic_temp_dir();
    let (tombstone, workspace_removed) = mgr
        .decommission_with_root(&record.id, managed_root.path(), None)
        .await
        .expect("decommission");

    assert!(
        !workspace_removed,
        "workspace_removed must be false when the worktree holds unsaved work"
    );
    assert_eq!(tombstone.state, ManagedSessionState::Decommissioned);
    assert!(
        worktree_path.exists(),
        "a dirty worktree must survive on disk — decommission must refuse to delete it"
    );
    assert!(
        worktree_path.join("uncommitted-work.txt").exists(),
        "the uncommitted file itself must survive the refused decommission"
    );
    assert_eq!(
        tombstone.workspace_path.as_deref(),
        Some(worktree_path.as_path()),
        "workspace_path must survive on the tombstone when removal was refused \
         (#4344 review) — it is the only durable pointer back to the retained work"
    );
}

/// `tm sessions prune --state stopped` (via `prune_managed`) surfaces a
/// dirty-worktree refusal through its own report, instead of printing the
/// same `decommissioned` line whether or not the worktree was actually
/// removed (#4344 review).
///
/// Why: `prune_managed` discarded the `bool` half of `decommission`'s
/// `(SessionRecord, bool)` return, and `PruneAction` had no variant for
/// "refused, worktree retained" — so `tm sessions prune --state stopped`
/// gave no visible signal, at the ONE surface an operator actually reads,
/// that a worktree was silently kept dirty on disk instead of removed.
/// What: seeds a `Stopped` record (via the real `stop()` teardown, mirroring
/// how a genuinely stopped session gets there) pointing at a real,
/// dirtied in-project worktree, runs
/// `prune_managed(PruneFilter::Stopped, …)`, and asserts the single
/// returned row is `PruneAction::DecommissionedWorktreeRetained` with
/// `retained_workspace_path` set to the worktree's path — plus the usual
/// on-disk survival assertions.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_reports_dirty_worktree_retained() {
    let dir = crate::test_support::hermetic_temp_dir();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake)
        .await
        .expect("manager");

    let fixture = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let session_name = "test-session-prune-dirty";
    let worktree_path = fixture.add_worktree(session_name);
    std::fs::write(worktree_path.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    std::fs::write(
        worktree_path.join("uncommitted-work.txt"),
        b"unsaved work\n",
    )
    .expect("write uncommitted file");

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

    // Transition to Stopped exactly like a real "runtime exited" session —
    // `prune --state stopped` only ever targets genuinely Stopped records.
    mgr.stop(&record.id).await.expect("stop");

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Stopped,
            false,
            false,
            None,
        )
        .await
        .expect("prune stopped");

    assert_eq!(
        outcome.count(),
        1,
        "the dirty stopped session is the only candidate"
    );
    let pruned = &outcome.sessions[0];
    assert_eq!(
        pruned.action,
        crate::session_manager::PruneAction::DecommissionedWorktreeRetained,
        "a dirty in-project worktree must report as retained, not a plain decommission; got {:?}",
        pruned.action
    );
    assert_eq!(
        pruned.retained_workspace_path.as_deref(),
        Some(worktree_path.as_path()),
        "the prune report must echo back WHERE the retained work lives"
    );

    // The usual on-disk guarantees still hold at the prune surface too.
    assert!(
        worktree_path.exists(),
        "the dirty worktree must survive a prune sweep"
    );
    assert!(
        worktree_path.join("uncommitted-work.txt").exists(),
        "the uncommitted file must survive a prune sweep"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// #4732: git declining is a REFUSAL, not a failure to work around.
//
// Every test below fails against the pre-#4732 remover, which fell through to
// `std::fs::remove_dir_all` on ANY non-zero git exit and on any failure to
// resolve the owning checkout. The two "cleans up" tests are the other half of
// the contract: the one legitimate fallback case must still work, or this fix
// would have traded a data-loss bug for a leak.
// ─────────────────────────────────────────────────────────────────────────

use super::decommission::remove_session_worktree;
use super::worktree_git_fixture::{GitWorktreeFixture, deny_all};

/// A worktree whose admin directory was removed out of band must be preserved
/// (#4732).
///
/// Why: `git rev-parse` from inside it answers `not a git repository: (null)`,
/// which `registry_root_for` reports as `None` — and the remover read that
/// `None` as "a plain directory nothing claims". The working tree, including
/// work that was never committed anywhere, is entirely intact. This is the
/// state ~70 worktrees on this machine were left in on 2026-07-21.
#[test]
fn remove_refuses_a_stale_worktree_pointer() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stale-pointer-e2e-4732");
    std::fs::write(wt.join("precious.txt"), "never committed\n").expect("write precious file");
    std::fs::remove_dir_all(fx.repo.join(".git").join("worktrees")).expect("drop admin dir");

    let outcome = remove_session_worktree(&wt);
    assert!(
        wt.join("precious.txt").exists(),
        "a stale pointer must not cost the working tree: {outcome:?}"
    );
    assert!(
        !outcome.removed(),
        "and must report NOT removed: {outcome:?}"
    );
}

/// A `.git` git cannot read is a worktree, not an absent one (#4732).
#[test]
fn remove_refuses_an_unreadable_git_entry() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unreadable-git-e2e-4732");
    std::fs::write(wt.join("precious.txt"), "never committed\n").expect("write precious file");
    let _restore = deny_all(&wt.join(".git"));

    let outcome = remove_session_worktree(&wt);
    assert!(
        wt.join("precious.txt").exists(),
        "an unreadable .git must not cost the working tree: {outcome:?}"
    );
    assert!(
        !outcome.removed(),
        "and must report NOT removed: {outcome:?}"
    );
}

/// `is not a .git file` is git validating and declining, not git reporting an
/// empty directory (#4732).
#[test]
fn remove_refuses_a_worktree_with_a_broken_git_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("broken-git-e2e-4732");
    std::fs::write(wt.join("precious.txt"), "never committed\n").expect("write precious file");
    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/xyz\n").expect("corrupt .git");

    let outcome = remove_session_worktree(&wt);
    assert!(
        wt.join("precious.txt").exists(),
        "a broken .git must not cost the working tree: {outcome:?}"
    );
    assert!(
        !outcome.removed(),
        "and must report NOT removed: {outcome:?}"
    );
}

/// The surviving fallback, half one: a trusty-mpm-owned directory with no
/// repository above it at all (#4732).
///
/// Why: pinning this is what keeps the fix from silently becoming "never clean
/// anything up". Nothing can be claiming a directory outside every repository,
/// so there is no git state to protect and a direct removal is the only way to
/// reclaim it.
#[test]
fn remove_cleans_up_a_directory_no_repository_claims() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let leftover = tmp.path().join("leftover-4732");
    std::fs::create_dir_all(&leftover).expect("mkdir");
    std::fs::write(leftover.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");

    let outcome = remove_session_worktree(&leftover);
    assert!(outcome.removed(), "{outcome:?}");
    assert!(!leftover.exists(), "the leftover directory must be gone");
}

/// The surviving fallback, half two: a leftover inside a real repository whose
/// registry positively does not name it (#4732).
///
/// Why: this is the ordinary shape — a worktree git already pruned, or one
/// whose creation never completed registration. Git holds nothing, so the
/// directory is the only thing to clean up.
#[test]
fn remove_cleans_up_an_unregistered_leftover_inside_a_repo() {
    let fx = GitWorktreeFixture::new();
    let leftover = fx.repo.join(".worktrees").join("unregistered-e2e-4732");
    std::fs::create_dir_all(&leftover).expect("mkdir");

    let outcome = remove_session_worktree(&leftover);
    assert!(outcome.removed(), "{outcome:?}");
    assert!(!leftover.exists(), "the leftover directory must be gone");
}

/// The happy path is unchanged: git removes a healthy worktree, and the ref
/// cleanup still runs behind it (#4732 regression guard).
#[test]
fn remove_still_removes_a_healthy_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("healthy-4732");

    let outcome = remove_session_worktree(&wt);
    assert!(outcome.removed(), "{outcome:?}");
    assert!(!wt.exists(), "the worktree directory must be gone");

    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git worktree list");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("healthy-4732"),
        "the registry entry must be pruned too"
    );
    let branches = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["branch", "--list", "session/healthy-4732"])
        .output()
        .expect("git branch --list");
    assert!(
        String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "the session branch must be deleted too"
    );
}
