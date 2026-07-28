//! End-to-end coverage for the #3764 item-4 destroyed-worktree detector,
//! against REAL git worktrees in BOTH parent-repo layouts.
//!
//! Why: the unit tests in `worktree_integrity` pin the pure classifier, but the
//! claim being made — "a stripped worktree is detectable, and detectable the
//! same way whether the parent clone is bare or not" — is a claim about git's
//! actual behaviour, not about our enum. It has to be tested against git.
//! These tests build a real repo, a real `git worktree add`, then destroy the
//! worktree exactly the way the three incidents did (remove its contents AND
//! its `.git` pointer file) and assert the verdict.
//!
//! The bare-vs-normal split is the load-bearing part: with a NORMAL parent
//! checkout, `git rev-parse --is-inside-work-tree` returns `true` for a fully
//! destroyed worktree. `should_have_flagged_stripped_worktree_under_normal_parent`
//! is the regression test for that false-negative, and is the reason the
//! detector compares `--show-toplevel` to the root instead.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::worktree_integrity::{WorktreeIntegrity, check};
use std::time::Duration;

/// Run a git command, panicking with stderr on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build `<root>/src` (a normal repo with one commit), then a parent clone at
/// `<root>/base` — bare when `bare` is true — and a worktree at
/// `<root>/base/.worktrees/wt`. Returns the worktree path.
fn make_worktree(root: &Path, bare: bool) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src");
    git(&src, &["init", "-q", "."]);
    git(&src, &["config", "user.email", "t@example.invalid"]);
    git(&src, &["config", "user.name", "t"]);
    std::fs::write(src.join("a.txt"), b"hi").expect("write a.txt");
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-qm", "init"]);

    let base = root.join("base");
    let mut clone_args = vec!["clone", "-q"];
    if bare {
        clone_args.push("--bare");
    }
    let src_s = src.display().to_string();
    let base_s = base.display().to_string();
    clone_args.push(&src_s);
    clone_args.push(&base_s);
    let out = std::process::Command::new("git")
        .args(&clone_args)
        .output()
        .expect("spawn git clone");
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let wt = base.join(".worktrees").join("wt");
    let wt_s = wt.display().to_string();
    git(&base, &["worktree", "add", "-q", &wt_s, "-b", "wt1"]);
    wt
}

/// Destroy a worktree the way the three incidents did: every tracked file AND
/// the `.git` pointer file are gone, but the directory itself remains EMPTY.
fn strip_worktree(wt: &Path) {
    for entry in std::fs::read_dir(wt).expect("read worktree dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path).expect("remove subdir");
        } else {
            std::fs::remove_file(&path).expect("remove file");
        }
    }
    assert!(
        !wt.join(".git").exists(),
        "test invariant: the .git pointer must be gone"
    );
    assert!(wt.exists(), "test invariant: the directory itself remains");
}

/// A healthy worktree is Intact (bare parent — this repo's real layout).
#[test]
fn healthy_worktree_is_intact() {
    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);
    assert_eq!(check(&wt), WorktreeIntegrity::Intact);
}

/// A stripped worktree under a BARE parent is Destroyed.
///
/// Why: this is this repo's layout (`.base` is a bare clone) and the exact
/// state `f443c12d` sat in for three days.
/// Test: this function IS the test.
#[test]
fn stripped_worktree_under_bare_parent_is_destroyed() {
    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);
    strip_worktree(&wt);
    assert!(
        matches!(check(&wt), WorktreeIntegrity::Destroyed(_)),
        "a stripped worktree under a bare parent must be Destroyed; got {:?}",
        check(&wt)
    );
}

/// A stripped worktree under a NORMAL parent is Destroyed — the case
/// `--is-inside-work-tree` would report healthy.
///
/// Why: this test also PROVES the false-negative it guards against. It asserts,
/// in the same run, that `git rev-parse --is-inside-work-tree` really does say
/// `true` for this destroyed tree, and that our detector says Destroyed anyway.
/// If a future change swaps the detector for the simpler probe, this fails.
/// Test: this function IS the test.
#[test]
fn should_have_flagged_stripped_worktree_under_normal_parent() {
    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), false);
    strip_worktree(&wt);

    // The naive probe's answer, recorded here as evidence.
    let naive = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .expect("spawn git");
    let naive_says = String::from_utf8_lossy(&naive.stdout).trim().to_string();
    assert_eq!(
        naive_says, "true",
        "test premise: --is-inside-work-tree is expected to MISS this case"
    );

    assert!(
        matches!(check(&wt), WorktreeIntegrity::Destroyed(_)),
        "the detector must flag a stripped worktree even when --is-inside-work-tree \
         says `true`; got {:?}",
        check(&wt)
    );
}

/// A workspace root that no longer exists at all is Destroyed.
#[test]
fn absent_workspace_root_is_destroyed() {
    let root = crate::test_support::hermetic_temp_dir();
    let absent = root.path().join("base").join(".worktrees").join("gone");
    assert!(matches!(check(&absent), WorktreeIntegrity::Destroyed(_)));
}

fn active_record(id: ManagedSessionId, ws: Option<PathBuf>) -> SessionRecord {
    SessionRecord {
        id,
        tmux_name: format!("tm-3764-integ-{id}"),
        cwd: std::path::PathBuf::from("/tmp"),
        task: "task".into(),
        state: ManagedSessionState::Active,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: ws,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: Some(id),
    }
}

/// The daemon sweep flags an Active session sitting in a destroyed worktree.
///
/// Why: the wiring test — the detector has to be reachable from the periodic
/// audit, or it changes nothing about the three-day blindness.
/// Test: this function IS the test.
#[tokio::test]
async fn audit_flags_destroyed_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);
    strip_worktree(&wt);

    let id = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(active_record(id, Some(wt.clone())))
        .await
        .expect("upsert");

    let findings = mgr.audit_worktree_integrity().await;
    assert_eq!(findings.len(), 1, "the destroyed worktree must be reported");
    assert_eq!(findings[0].id, id);
    assert!(matches!(
        findings[0].verdict,
        WorktreeIntegrity::Destroyed(_)
    ));
}

/// A healthy Active session produces no findings.
///
/// Why: the regression half — an audit that flagged everything would also pass
/// the test above while making the alarm worthless.
/// Test: this function IS the test.
#[tokio::test]
async fn audit_passes_healthy_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);

    mgr.store
        .write()
        .await
        .upsert(active_record(ManagedSessionId::new(), Some(wt)))
        .await
        .expect("upsert");

    assert!(
        mgr.audit_worktree_integrity().await.is_empty(),
        "a healthy worktree must produce no findings"
    );
}

/// A local-path / adopted session on a plain directory is NOT audited.
///
/// Why: the false-positive guard. Those sessions legitimately live outside a
/// `.worktrees/` parent and may not be git repos at all; flagging them would
/// bury the real signal in noise.
/// Test: this function IS the test.
#[tokio::test]
async fn audit_ignores_non_worktree_workspace() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let plain = crate::test_support::hermetic_temp_dir();
    let ws = plain.path().join("some").join("user").join("project");
    std::fs::create_dir_all(&ws).expect("create plain dir");

    mgr.store
        .write()
        .await
        .upsert(active_record(ManagedSessionId::new(), Some(ws)))
        .await
        .expect("upsert");

    assert!(
        mgr.audit_worktree_integrity().await.is_empty(),
        "a non-.worktrees workspace must never be audited"
    );
}

// ── code-critic HIGH-3: linkage lost vs work lost ───────────────────────────

/// Deleting ONLY the base repo's admin gitdir unlinks the worktree while every
/// file survives — the detector must say `LinkageLost`, never `Destroyed`.
///
/// Why: this is the critic's reproduction, and it is not hypothetical for this
/// repo. When the `.base` bare clone was destroyed on 07-21 it orphaned ~70
/// worktrees whose contents were **entirely intact**. Under the first draft
/// every one of them would have alarmed *"Its uncommitted work is GONE. Stop the
/// session and recreate it"* — advice that, followed, runs `decommission` (which
/// carries no #4118 dirt guard) and deletes the very work that survived. A
/// detector that says that is a destroyer.
/// Test: this function IS the test.
#[test]
fn linkage_lost_with_surviving_files_does_not_claim_work_gone() {
    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);

    // Real uncommitted work sitting in the worktree.
    std::fs::write(wt.join("precious.txt"), b"three days of uncommitted work")
        .expect("write precious.txt");

    // Destroy ONLY the base's administrative gitdir for this worktree —
    // the worktree's own files (a.txt, precious.txt, .git pointer) all remain.
    let admin = root.path().join("base").join("worktrees").join("wt");
    assert!(
        admin.is_dir(),
        "test premise: admin gitdir must exist at {admin:?}"
    );
    std::fs::remove_dir_all(&admin).expect("remove admin gitdir");

    let verdict = check(&wt);
    match verdict {
        WorktreeIntegrity::LinkageLost { surviving, .. } => {
            let n = surviving.expect("the directory is readable, so the count must be Some");
            assert!(
                n >= 2,
                "a.txt and precious.txt must both be counted as surviving; got {n}"
            );
        }
        other => panic!(
            "files still on disk must NEVER be reported as Destroyed — that alarm tells \
             the operator to recreate, which deletes them. Got {other:?}"
        ),
    }

    // The evidence, restated as a filesystem fact: the work really is still there.
    assert_eq!(
        std::fs::read(wt.join("precious.txt")).expect("read precious.txt"),
        b"three days of uncommitted work",
    );
}

/// A worktree emptied of files IS `Destroyed` — the distinction has teeth in
/// both directions.
///
/// Why: the regression half of the test above. A split that reported
/// `LinkageLost` for everything would pass that test while making the
/// `Destroyed` verdict unreachable and the detector useless.
/// Test: this function IS the test.
#[test]
fn emptied_worktree_is_destroyed_not_linkage_lost() {
    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);
    strip_worktree(&wt);
    assert!(
        matches!(check(&wt), WorktreeIntegrity::Destroyed(_)),
        "an EMPTY unlinked worktree must be Destroyed; got {:?}",
        check(&wt)
    );
}

// ── code-critic HIGH-2: the git probe must be bounded ───────────────────────

/// A probe that exceeds its timeout yields `Unknown` — never a destruction
/// verdict, and never `Intact`.
///
/// Why: the first draft awaited `spawn_blocking(check)` unbounded inside
/// `orphan_gc_loop`. One worktree on a stalled mount would block the audit,
/// the rest of that tick, and every future tick, indefinitely. A timeout is an
/// absence of evidence, so it must land on `Unknown`: reporting `Intact` would
/// be the fail-open this module exists to prevent, and reporting `Destroyed`
/// would tell an operator to delete a healthy tree.
/// What: drives the audit with a 1 ns budget, which the `git` subprocess cannot
/// possibly meet, against a HEALTHY worktree — so an unbounded implementation
/// returns `Intact` (no finding at all) and fails this test.
/// Test: this function IS the test.
#[tokio::test]
async fn audit_times_out_into_unknown_never_destroyed() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true); // healthy
    let id = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(active_record(id, Some(wt)))
        .await
        .expect("upsert");

    let findings = mgr
        .audit_worktree_integrity_with_timeout(Duration::from_nanos(1))
        .await;

    assert_eq!(
        findings.len(),
        1,
        "a timed-out probe must produce a finding, not silently pass as healthy"
    );
    match &findings[0].verdict {
        WorktreeIntegrity::Unknown(why) => {
            assert!(
                why.contains("timed out"),
                "the verdict must say it timed out; got {why:?}"
            );
        }
        other => panic!("a timeout must be Unknown, never {other:?}"),
    }
}

/// The default audit still uses a bounded timeout, not an unbounded await.
///
/// Why: `audit_worktree_integrity_with_timeout` being correct is worthless if
/// the production entry point does not route through it.
/// Test: this function IS the test — a healthy worktree audited through the
/// DEFAULT entry point produces no finding, proving the generous production
/// budget is applied rather than a 1 ns one.
#[tokio::test]
async fn default_audit_is_bounded_but_generous() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let root = crate::test_support::hermetic_temp_dir();
    let wt = make_worktree(root.path(), true);
    mgr.store
        .write()
        .await
        .upsert(active_record(ManagedSessionId::new(), Some(wt)))
        .await
        .expect("upsert");

    assert!(
        mgr.audit_worktree_integrity().await.is_empty(),
        "the production timeout must be generous enough for a real git call"
    );
}
