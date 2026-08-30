//! Unit tests for the #4091 dirty-worktree guard in `worktree_safety.rs`.
//!
//! Why: these exercise [`super::inspect_dirt`] in isolation against REAL git
//! repositories built under `tempfile::tempdir()` — never a live worktree —
//! so each "dirty" definition (modified tracked, untracked-not-ignored,
//! committed-but-unpushed) and each fail-safe error path is pinned
//! independently of the reclaim path that consumes them. The wired-up
//! behaviour is covered separately by the `#4091` tests in
//! `prune_orphan_tests`.
//! What: the clean/pushed baseline, the three dirty definitions, the
//! ignored-files carve-out, the non-git-directory classification, and the
//! error-means-dirty invariant.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The default policy must be the SAFE one. If this ever flips, every caller
/// that relies on `..Default::default()` silently gains the power to destroy
/// uncommitted work.
#[test]
fn dirty_policy_defaults_to_skip() {
    assert_eq!(
        DirtyWorktreePolicy::default(),
        DirtyWorktreePolicy::Skip,
        "the default dirty policy must never be ForceDiscard"
    );
}

/// Baseline: a worktree with no local edits, whose HEAD is already on a
/// remote-tracking ref, is PROVABLY clean — the guard must return `None` so
/// reclamation still works. Without this the guard would be a permanent leak.
#[test]
fn inspect_dirt_clean_pushed_worktree_is_none() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("clean");
    assert!(
        inspect_dirt(&wt).is_none(),
        "a clean, fully-pushed worktree must be reclaimable; got {:?}",
        inspect_dirt(&wt)
    );
}

/// Definition 1a of dirty: a MODIFIED TRACKED file.
#[test]
fn inspect_dirt_reports_modified_tracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("modified");
    std::fs::write(wt.join("README.md"), "edited but never committed\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("a modified tracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 0);
    assert_eq!(dirt.path, wt);
}

/// Definition 1b of dirty: an UNTRACKED, non-ignored file. Untracked work is
/// the most easily lost kind — it exists nowhere but that directory.
#[test]
fn inspect_dirt_reports_untracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("untracked");
    std::fs::write(wt.join("notes.md"), "hand-written, never added\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("an untracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
}

/// The carve-out that keeps the guard usable: IGNORED files (build artefacts)
/// are not work, and must not pin a worktree as dirty forever. A `target/`
/// directory in every checkout would otherwise make the guard refuse
/// everything, which is indistinguishable from having no reclamation at all.
#[test]
fn inspect_dirt_ignores_gitignored_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored");
    std::fs::write(wt.join(".gitignore"), "build-output/\n").unwrap();
    std::fs::create_dir_all(wt.join("build-output")).unwrap();
    std::fs::write(wt.join("build-output").join("artifact.bin"), "junk\n").unwrap();
    // Commit the .gitignore itself so only the IGNORED path remains on disk.
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["add", ".gitignore"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["commit", "-m", "ignore build output"])
        .output()
        .unwrap();
    assert!(commit.status.success());

    // The commit itself is unpushed, so the worktree IS dirty — but for the
    // commit, not for the ignored artefact. Assert on the file count.
    let dirt = inspect_dirt(&wt).expect("the new commit is unpushed, so this is dirty");
    assert_eq!(
        dirt.dirty_files, 0,
        "ignored build output must not count as uncommitted work; reason: {}",
        dirt.reason
    );
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// trusty-mpm's OWN untracked ownership sentinel must not count as work.
///
/// Why this test exists: it caught a real self-inflicted bug. Every managed
/// worktree carries a `.trusty-mpm-worktree` sentinel, and most host projects
/// do not gitignore it — so counting it marked EVERY managed worktree
/// permanently dirty, which would have shipped a guard that silently disabled
/// all reclamation.
#[test]
fn inspect_dirt_excludes_own_sentinel() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("sentinel-only");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    assert!(
        inspect_dirt(&wt).is_none(),
        "trusty-mpm's own sentinel must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// The two DISPOSABLE `.trusty-mpm/` artefacts — and only those — must not
/// count as work. The daemon rewrites both on every snapshot.
#[test]
fn inspect_dirt_excludes_untracked_trusty_mpm_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("scrollback");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("scrollback.txt"), "pane dump\n").unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("last-instructions.md"),
        "brief\n",
    )
    .unwrap();

    assert!(
        inspect_dirt(&wt).is_none(),
        "the daemon's own scrollback artefacts must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// THE REGRESSION THIS ROUND EXISTS FOR: hand-rescued source parked under
/// `.trusty-mpm/` must read as work.
///
/// Why this exact shape: on 2026-07-27 the live session worktree
/// `.base/.worktrees/2eb72dca-…` held
/// `.trusty-mpm/wip-backup-20260727-jira/` containing twelve files of
/// uncommitted `trusty-git-analytics` source, deliberately preserved by an
/// earlier rescue operation. The first cut of this guard excused the whole
/// `.trusty-mpm/` subtree by prefix, so `dirty_files` stayed 0, `inspect_dirt`
/// returned `None`, and `git worktree remove --force` would have destroyed it
/// silently. The carve-out is now an allowlist of two exact paths.
#[test]
fn inspect_dirt_counts_wip_backup_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("wip-backup");
    let wip = wt.join(".trusty-mpm").join("wip-backup-20260727-jira");
    std::fs::create_dir_all(wip.join("crates").join("foo").join("src")).unwrap();
    std::fs::write(
        wip.join("crates").join("foo").join("src").join("main.rs"),
        "fn main() { /* rescued, uncommitted */ }\n",
    )
    .unwrap();
    // The disposable artefacts sit alongside it, exactly as they do live.
    std::fs::write(wt.join(".trusty-mpm").join("scrollback.txt"), "pane dump\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("rescued source under .trusty-mpm/ must read as dirty");
    assert_eq!(
        dirt.dirty_files, 1,
        "the wip-backup tree must count and the scrollback must not; reason: {}",
        dirt.reason
    );
}

/// Pause snapshots under `.trusty-mpm/sessions/` are what `/tm-session-resume`
/// reads back, and this PR's own skill change tells operators to trust the
/// pause path. They are not disposable.
#[test]
fn inspect_dirt_counts_pause_snapshots_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("pause-snapshots");
    std::fs::create_dir_all(wt.join(".trusty-mpm").join("sessions")).unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("sessions").join("s-01.md"),
        "# paused session\n",
    )
    .unwrap();

    let dirt = inspect_dirt(&wt).expect("pause snapshots must read as dirty");
    assert!(dirt.dirty_files >= 1, "reason: {}", dirt.reason);
}

/// The SAME `.trusty-mpm/` content must read as dirty when the project
/// GITIGNORES it, which is how this repo's `main` spells it (`.gitignore:49`,
/// `.trusty-mpm/*`). Plain `git status` cannot see it at all in that spelling —
/// only the per-file `--ignored=matching` pass can. Both spellings are live in
/// the fleet simultaneously, on different branches of this one repo.
#[test]
fn inspect_dirt_counts_gitignored_trusty_mpm_work() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-tm");
    std::fs::write(wt.join(".gitignore"), ".trusty-mpm/*\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&wt, "gitignore .trusty-mpm");
    std::fs::create_dir_all(wt.join(".trusty-mpm").join("wip-backup-x")).unwrap();
    std::fs::write(
        wt.join(".trusty-mpm").join("wip-backup-x").join("main.rs"),
        "fn main() {}\n",
    )
    .unwrap();

    // Prove the premise: plain porcelain really is blind to it.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: plain porcelain should not see the gitignored subtree"
    );

    let dirt = inspect_dirt(&wt).expect("gitignored .trusty-mpm/ work must still read as dirty");
    assert!(dirt.dirty_files >= 1, "reason: {}", dirt.reason);
}

/// The counterweight: a TRACKED file under `.trusty-mpm/` that someone edited
/// IS real work, and the bookkeeping exclusion must not swallow it. The
/// exclusion is scoped to untracked entries precisely so this stays protected.
#[test]
fn inspect_dirt_counts_tracked_trusty_mpm_edit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("tracked-config");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("INSTRUCTIONS.md"), "v1\n").unwrap();
    for args in [
        vec!["add", ".trusty-mpm/INSTRUCTIONS.md"],
        vec!["commit", "-m", "track instructions"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {:?}", out);
    }
    std::fs::write(
        wt.join(".trusty-mpm").join("INSTRUCTIONS.md"),
        "v2 edited\n",
    )
    .unwrap();

    let dirt = inspect_dirt(&wt).expect("a tracked edit is real work, wherever it lives");
    assert_eq!(
        dirt.dirty_files, 1,
        "a tracked .trusty-mpm/ edit must still count; reason: {}",
        dirt.reason
    );
}

/// #4166: a tracked file DELETED from `.trusty-mpm/` must be counted by
/// exactly one of the two passes, and before this it was counted by neither.
///
/// Why: pass 1 skips every `.trusty-mpm/`-scoped status line on the promise
/// that pass 2 owns them, and pass 2 returned `Ok(0)` without running git
/// whenever the directory was absent. Deleting the last file under
/// `.trusty-mpm/` removes the directory too, so the deletion fell through the
/// handover and `dirty_files` read 0 — the broken invariant, not a large loss:
/// for a registered worktree both the index and the object store live outside
/// the candidate, so nothing unique dies.
#[test]
fn inspect_dirt_counts_a_deleted_tracked_file_under_trusty_mpm() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("deleted-tm");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("notes.md"), "v1\n").unwrap();
    // Pushed, so the commit itself is not the dirt under test.
    GitWorktreeFixture::commit_all_and_push(&wt, "track notes under .trusty-mpm");
    std::fs::remove_dir_all(wt.join(".trusty-mpm")).unwrap();

    // Premise: the directory really is gone, which is what used to short-circuit
    // pass 2 before it ever asked git.
    assert!(
        !wt.join(".trusty-mpm").exists(),
        "premise broken: the whole directory should be gone"
    );

    let dirt =
        inspect_dirt(&wt).expect("a deleted tracked file under .trusty-mpm/ must read as dirty");
    assert_eq!(
        dirt.dirty_files, 1,
        "the deletion must be counted by pass 2; reason: {}",
        dirt.reason
    );
}

/// Definition 2 of dirty: a COMMITTED but UNPUSHED commit. This looks safe —
/// the work is committed — but `remove_session_worktree` deletes the
/// `session/<leaf>` branch after removal, so the commit loses its last
/// reachable ref.
#[test]
fn inspect_dirt_reports_unpushed_commit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unpushed");
    GitWorktreeFixture::commit_unpushed(&wt);

    let dirt = inspect_dirt(&wt).expect("an unpushed commit must read as dirty");
    assert_eq!(dirt.dirty_files, 0, "reason: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// THE CRITICAL: a parent worktree containing a GITIGNORED nested worktree
/// holding uncommitted work must read as DIRTY.
///
/// Why: `.gitignore:40` on this repo's `main` ignores `.claude/worktrees/`, the
/// exact shape `find_orphaned_worktrees` location 5 exists to walk. The nested
/// worktrees carry no ownership sentinel, so the #3649 gate routes them to
/// `owner_unknown` and REFUSES to delete them — and then the sweep would delete
/// the parent they live inside, having certified it clean. The guard was
/// bypassable by deleting its own parent.
#[test]
fn inspect_dirt_reports_nested_gitignored_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("parent");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");
    let nested = fx.add_nested_worktree(&parent, ".claude/worktrees", "agentA");
    std::fs::write(nested.join("patch.rs"), "// agent work, never committed\n").unwrap();

    // Prove the premise: the parent's own porcelain really is blind to it.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&parent)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: plain porcelain should not see the gitignored nested worktree"
    );

    let dirt = inspect_dirt(&parent)
        .expect("a nested worktree holding uncommitted work must make the parent dirty");
    assert!(
        dirt.reason.contains("nested git worktree/repository"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The same hole, but via an UNREGISTERED repository — an independent clone
/// dropped into a gitignored directory. `git worktree list` has never heard of
/// it, so only the on-disk scan can find it.
#[test]
fn inspect_dirt_reports_unregistered_nested_repo_in_ignored_dir() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("with-clone");
    std::fs::write(parent.join(".gitignore"), "scratch/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore scratch");
    let inner = parent.join("scratch").join("side-project");
    std::fs::create_dir_all(&inner).unwrap();
    for args in [vec!["init", "--initial-branch=main"], vec!["status"]] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&inner)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    std::fs::write(inner.join("work.rs"), "// never committed anywhere\n").unwrap();

    let dirt = inspect_dirt(&parent)
        .expect("an unregistered nested repo holding work must make the parent dirty");
    assert!(
        dirt.reason.contains("nested git worktree/repository"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The counterweight, and it matters as much as the finding: a nested worktree
/// that is itself CLEAN and pushed must NOT pin the parent.
///
/// Why: without this, any session that ever spawned an agent worktree would be
/// permanently unreclaimable, which is the "guard that silently disables
/// reclamation" failure mode — indistinguishable from having no sweep at all.
#[test]
fn inspect_dirt_allows_clean_nested_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("clean-parent");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");
    let nested = fx.add_nested_worktree(&parent, ".claude/worktrees", "agentClean");
    std::fs::write(nested.join("done.rs"), "// finished agent work\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&nested, "agent work, pushed");

    assert!(
        inspect_dirt(&parent).is_none(),
        "a clean, pushed nested worktree must not pin the parent; got {:?}",
        inspect_dirt(&parent)
    );
}

/// A dirty nested worktree sitting BEHIND a clean one must still be found.
///
/// Why this test exists: it pins a real bug. The first cut of the scan used
/// `?` on the per-root inspection, so the first CLEAN nested root returned
/// `None` for the whole scan and every root after it went unexamined — a parent
/// with one finished agent worktree and one in-flight one read CLEAN. Roots are
/// visited in sorted order, so `aaa-clean` is checked before `zzz-dirty`.
#[test]
fn inspect_dirt_keeps_scanning_past_a_clean_nested_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("two-nested");
    std::fs::write(parent.join(".gitignore"), ".claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore agent worktrees");

    let clean = fx.add_nested_worktree(&parent, ".claude/worktrees", "aaa-clean");
    std::fs::write(clean.join("done.rs"), "// finished\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&clean, "finished, pushed");

    let dirty = fx.add_nested_worktree(&parent, ".claude/worktrees", "zzz-dirty");
    std::fs::write(dirty.join("wip.rs"), "// in flight, never committed\n").unwrap();

    let dirt = inspect_dirt(&parent)
        .expect("a dirty nested worktree behind a clean one must still make the parent dirty");
    assert!(
        dirt.reason.contains("zzz-dirty"),
        "the reason must name the nested worktree that is actually dirty; got: {}",
        dirt.reason
    );
}

/// Documents the residual, so it is a decision rather than an accident: the
/// nested-repo scan does NOT descend into disposable build directories, so a
/// repository inside `target/` is invisible. Skipping them by name is what
/// keeps the scan affordable across ~95 candidates.
#[test]
fn inspect_dirt_does_not_scan_disposable_build_dirs() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("disposable");
    std::fs::write(parent.join(".gitignore"), "target/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore target");
    let buried = parent.join("target").join("debug").join("checkout");
    std::fs::create_dir_all(buried.join(".git")).unwrap();
    std::fs::write(buried.join("thing.rs"), "// inside target/\n").unwrap();

    assert!(
        inspect_dirt(&parent).is_none(),
        "target/ is deliberately not scanned; got {:?}",
        inspect_dirt(&parent)
    );
}

/// HIGH-1: `status.showUntrackedFiles=no` makes bare `git status --porcelain`
/// exit 0 with EMPTY output while untracked work sits on disk. Its natural home
/// is the SHARED `.base/.git/config` every worktree in the sweep reads, so one
/// line there would blind the guard fleet-wide. The mode is pinned on the
/// command line, which beats every config file.
#[test]
fn inspect_dirt_survives_hostile_show_untracked_files_config() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("hostile-untracked");
    std::fs::write(wt.join("notes.md"), "hand-written, never added\n").unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["config", "status.showUntrackedFiles", "no"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Prove the premise: unpinned porcelain really does go silent.
    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: the hostile config should have silenced plain porcelain"
    );

    let dirt = inspect_dirt(&wt).expect("a hostile config must not be able to hide untracked work");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
}

/// The same class of hijack via `core.excludesFile`: a user-global excludes
/// file naming the work hides it exactly as `showUntrackedFiles=no` does.
/// Pinned empty on the command line.
#[test]
fn inspect_dirt_survives_hostile_global_excludes_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("hostile-excludes");
    let excludes = fx.repo.join("hostile-excludes.txt");
    std::fs::write(&excludes, "secret-work.txt\n").unwrap();
    std::fs::write(wt.join("secret-work.txt"), "REAL WORK\n").unwrap();
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["config", "core.excludesFile"])
        .arg(&excludes)
        .output()
        .unwrap();
    assert!(out.status.success());

    let plain = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: the excludes file should have hidden the work"
    );

    let dirt = inspect_dirt(&wt).expect("a hostile excludes file must not hide untracked work");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
}

/// MEDIUM: `decommission` force-deletes `session/<leaf>` derived from the
/// DIRECTORY NAME, not from `HEAD`. When HEAD has been switched off that branch
/// — routine; the live `.base/.worktrees/2eb72dca-…` sits on `fix/4061-…` —
/// HEAD reads fully pushed while the branch about to be destroyed is not.
#[test]
fn inspect_dirt_reports_unpushed_session_branch_when_head_moved() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("moved-head");
    GitWorktreeFixture::commit_unpushed(&wt);
    // Switch HEAD off session/moved-head onto the pushed base commit.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["switch", "--detach", "origin/main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let dirt = inspect_dirt(&wt)
        .expect("an unpushed commit on the branch decommission deletes must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "HEAD is pushed but session/moved-head is not; reason: {}",
        dirt.reason
    );
}

/// The `.base/.worktrees/<session-id>` shape — the DOMINANT population of the
/// sweep — has its branch named BARE by `provisioner/workspace.rs:803`
/// (`session_id.to_string()`), not `session/<id>`. Checking only the prefixed
/// spelling left this leg inert exactly where most worktrees live.
///
/// Deliberately conservative: `branch -D session/<id>` currently MISSES the
/// bare branch, so it survives removal today. The divergence is a live
/// pre-existing bug that will be repaired on one side or the other, and if it
/// is repaired on the `remove_session_worktree` side then bare branches start
/// being destroyed. A guard that goes silently inert because somebody fixed an
/// unrelated naming bug is the failure mode this module exists to prevent.
#[test]
fn inspect_dirt_reports_unpushed_bare_leaf_branch_when_head_moved() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("bare-branch");
    // Re-point the worktree at a BARE branch named after the leaf, the way the
    // provisioner names it, then commit on it and switch HEAD away.
    for args in [
        vec!["switch", "-c", "bare-branch"],
        vec!["branch", "-D", "session/bare-branch"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    GitWorktreeFixture::commit_unpushed(&wt);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["switch", "--detach", "origin/main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let dirt = inspect_dirt(&wt)
        .expect("an unpushed commit on the bare leaf-named branch must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "HEAD is pushed but the bare `bare-branch` is not; reason: {}",
        dirt.reason
    );
}

/// The counterweight to the test above: when the session branch IS the
/// checked-out branch, its commits must be counted ONCE, not twice.
#[test]
fn inspect_dirt_does_not_double_count_the_checked_out_session_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("single-count");
    GitWorktreeFixture::commit_unpushed(&wt);

    let dirt = inspect_dirt(&wt).expect("an unpushed commit must read as dirty");
    assert_eq!(
        dirt.unpushed_commits, 1,
        "the same commit must not be counted by both legs; reason: {}",
        dirt.reason
    );
}

/// MEDIUM: `GIT_DIR` + `GIT_WORK_TREE` pointing at a clean repository make
/// `git -C <candidate> status` answer for THAT repository — a false CLEAN. The
/// daemon inherits whatever environment launched it. Asserted on the command
/// itself rather than by mutating process-global environment, which is racy
/// under a parallel test runner.
#[test]
fn git_command_strips_repository_redirecting_env() {
    let cmd = super::git_command(std::path::Path::new("/tmp"), &["status"]);
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(
            removed.contains(&key),
            "{key} must be removed from the child environment; removed: {removed:?}"
        );
    }
}

/// #6391 REGRESSION: the REMOVAL is subject to the same redirect its gates are
/// hardened against, and until #6391 it was the one call that was not.
///
/// Why: `git worktree` resolves its repository from `GIT_DIR` ahead of `-C`.
/// Measured directly: with `GIT_DIR`/`GIT_WORK_TREE` naming another repository,
/// `git -C <repo> worktree list` lists that OTHER repository's worktrees, and
/// operating on one of `<repo>`'s own worktrees exits 128 with
/// `is not a working tree`. Every gate ahead of the removal goes through
/// [`super::git_command`] and so examines the real worktree; the removal used a
/// bare `Command::new("git")` and did not. The result is a reap that passes
/// every gate, deletes nothing, and reports the refusal to a `warn!` no test
/// reads — `agent_worktree_reap_tests::await_gone` then panics with
/// `was never removed` and no reason (#6391, and the `is not a working tree`
/// text #6099 recorded).
///
/// Against the pre-fix body this fails outright: a bare command removes no
/// variables at all, so `removed` is empty.
#[test]
fn worktree_remove_command_strips_repository_redirecting_env() {
    let cmd = super::worktree_remove_command(
        std::path::Path::new("/r"),
        std::path::Path::new("/r/.claude/worktrees/agent-x"),
    );
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_COUNT",
    ] {
        assert!(
            removed.contains(&key),
            "{key} must be removed from the removal's child environment; removed: {removed:?}"
        );
    }
}

/// The hardening must not reorder the command: `--force` still precedes the
/// target, and the target is still the last argument.
///
/// Why: [`super::git_command`] appends its own pinned globals and `-C <dir>`
/// before the caller's arguments, so routing the removal through it is only
/// correct if the subcommand and its operand still land in that order. A
/// silently reordered `git worktree remove <path> --force` is a different
/// command.
#[test]
fn worktree_remove_command_ends_in_the_target_path() {
    let target = std::path::Path::new("/r/.claude/worktrees/agent-x");
    let cmd = super::worktree_remove_command(std::path::Path::new("/r"), target);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.ends_with(&[
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            target.display().to_string(),
        ]),
        "the removal must end in `worktree remove --force <path>`; args: {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w[0] == "-C" && w[1] == "/r"),
        "the repository root must still be named by `-C`; args: {args:?}"
    );
}

/// Every git invocation must carry the pins, not just the ones someone
/// remembered — that is the point of routing them through one builder.
#[test]
fn git_command_pins_untracked_and_excludes_config() {
    let cmd = super::git_command(std::path::Path::new("/tmp"), &["status"]);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for pin in [
        "core.excludesFile=",
        "status.showUntrackedFiles=normal",
        "core.quotePath=false",
    ] {
        assert!(
            args.iter().any(|a| a == pin),
            "`{pin}` must be pinned on every git call; args: {args:?}"
        );
    }
}

/// FAIL-SAFE: a path git cannot examine at all reads as DIRTY, never as clean.
/// An error on the check must never become a green light to delete.
#[test]
fn inspect_dirt_treats_missing_path_as_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does").join("not").join("exist");
    let dirt = inspect_dirt(&missing).expect("an unexaminable path must read as dirty");
    assert!(
        dirt.reason.contains("unreadable") || dirt.reason.contains("not a git worktree"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// A plain directory that is NOT a git worktree root but holds files cannot be
/// assessed by git at all — and the fail-safe answer is DIRTY. This also pins
/// the identity check: the directory sits INSIDE a real checkout, so a naive
/// `git status` would have reported the ENCLOSING repo's (clean) status and
/// wrongly approved deletion.
#[test]
fn inspect_dirt_treats_non_worktree_with_files_as_dirty() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join(".worktrees").join("not-a-worktree");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("leftover.txt"), "who knows\n").unwrap();

    let dirt = inspect_dirt(&plain).expect("a non-worktree dir holding files must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
    assert!(
        dirt.reason.contains("not a git worktree root"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The counterweight to the test above: an EMPTY leftover shell (nothing but
/// the ownership sentinel) is provably free of work and stays reclaimable.
/// This is the #1838 case the orphan sweep exists for — one project grew 94 of
/// these — so the guard must not turn it into a permanent leak.
#[test]
fn inspect_dirt_allows_empty_non_git_leftover() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = tmp.path().join(".worktrees").join("empty-shell");
    std::fs::create_dir_all(&shell).unwrap();
    std::fs::write(
        shell.join(super::super::decommission::WORKTREE_SENTINEL_FILE),
        "{}",
    )
    .unwrap();

    assert!(
        inspect_dirt(&shell).is_none(),
        "an empty leftover shell must stay reclaimable; got {:?}",
        inspect_dirt(&shell)
    );
}
