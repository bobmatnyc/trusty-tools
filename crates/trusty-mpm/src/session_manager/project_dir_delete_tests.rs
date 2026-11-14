//! Tests for `project_dir_delete` (#10992). Hermetic: every directory is a
//! real git checkout or plain directory inside a fresh `TempDir`, and the
//! presence check is a fake `Authenticator` — no Touch ID prompt is shown.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use super::super::worktree_git_fixture::{
    GitWorktreeFixture, StandingProcess, probe_cannot_answer,
};
use super::*;
use crate::core::presence_local::{Authenticator, PresenceOutcome};

/// A fake presence check that answers `outcome` and counts its calls.
struct FakeAuth {
    outcome: PresenceOutcome,
    calls: Cell<usize>,
}

impl FakeAuth {
    fn new(outcome: PresenceOutcome) -> Self {
        Self {
            outcome,
            calls: Cell::new(0),
        }
    }
    fn granted() -> Self {
        Self::new(PresenceOutcome::Granted)
    }
}

impl Authenticator for FakeAuth {
    fn authenticate(&self, _reason: &str) -> PresenceOutcome {
        self.calls.set(self.calls.get() + 1);
        self.outcome.clone()
    }
}

/// Probes with injected answers; `dirt_after` makes the Nth dirt call dirty.
#[derive(Default)]
struct FakeProbes {
    dirt: Option<String>,
    live: Option<String>,
    dirt_from_call: Option<usize>,
    dirt_calls: Cell<usize>,
}

impl DirProbes for FakeProbes {
    fn dirt(&self, _dir: &Path) -> Option<String> {
        let n = self.dirt_calls.get() + 1;
        self.dirt_calls.set(n);
        if self.dirt_from_call.is_some_and(|from| n >= from) {
            return Some("1 uncommitted/untracked file(s), 0 unpushed commit(s)".into());
        }
        self.dirt.clone()
    }
    fn live_process(&self, dir: &Path) -> Option<String> {
        self.live.clone().or_else(|| {
            // Keep the probe honest: a real process check on a fixture dir.
            OsProbes.live_process(dir)
        })
    }
}

fn session(id: &str, dir: &Path, kind: Option<SessionKind>, state: &str) -> DirSession {
    DirSession {
        id: id.into(),
        name: format!("tm-{id}"),
        kind,
        states: vec![state.into()],
        workspace: Some(dir.to_path_buf()),
        cwd: None,
    }
}

fn input<'a>(
    sessions: &'a [DirSession],
    root: &'a Path,
    arch: &'a [PathBuf],
) -> DirDeleteInput<'a> {
    DirDeleteInput {
        session_id: "s1",
        sessions,
        root,
        architect_dirs: arch,
    }
}

fn plan(sessions: &[DirSession], root: &Path) -> Result<DirDeletePlan, DirDeleteError> {
    plan_dir_delete(input(sessions, root, &[]), &OsProbes)
}

fn yes(_: &DirDeletePlan) -> Confirmation {
    Confirmation::Yes
}

/// A plain directory with one file, outside `fx.repos_root`.
fn outside_dir(fx: &GitWorktreeFixture) -> PathBuf {
    let out = fx.repos_root.parent().expect("tmp root").join("outside");
    std::fs::create_dir_all(&out).expect("mkdir outside");
    std::fs::write(out.join("keep.txt"), "keep\n").expect("write keep");
    out
}

// ---- AC2: containment ------------------------------------------------------

#[test]
fn a_path_outside_the_root_is_never_removed() {
    let fx = GitWorktreeFixture::new();
    let out = outside_dir(&fx);
    let sessions = [session("s1", &out, Some(SessionKind::Ordinary), "stopped")];
    let err = plan(&sessions, &fx.repos_root).expect_err("outside the root");
    assert!(matches!(err, DirDeleteError::Uncontained { .. }), "{err}");
    // A forged plan naming the outside path must not get a capability either.
    let forged = DirDeletePlan {
        session_id: "s1".into(),
        dir: out.clone(),
        root: fx.repos_root.clone(),
        confirm_name: "outside".into(),
    };
    let probes = FakeProbes::default();
    let err =
        DirDeleteCapability::acquire(&forged, &Confirmation::Yes, &FakeAuth::granted(), &probes)
            .expect_err("forged plan");
    assert!(matches!(err, DirDeleteError::Uncontained { .. }), "{err}");
    assert!(
        out.join("keep.txt").exists(),
        "the outside directory was touched"
    );
}

#[test]
fn a_dotdot_escape_is_refused() {
    let fx = GitWorktreeFixture::new();
    let out = outside_dir(&fx);
    let escape = fx
        .repos_root
        .join("owner")
        .join("..")
        .join("..")
        .join("outside");
    let sessions = [session(
        "s1",
        &escape,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("`..` escape");
    assert!(matches!(err, DirDeleteError::Uncontained { .. }), "{err}");
    assert!(out.join("keep.txt").exists());
}

#[test]
fn a_symlink_leaf_is_refused() {
    let fx = GitWorktreeFixture::new();
    let out = outside_dir(&fx);
    for target in [out.clone(), fx.repo.clone()] {
        let link = fx.repos_root.join("owner").join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let sessions = [session("s1", &link, Some(SessionKind::Ordinary), "stopped")];
        let err = plan(&sessions, &fx.repos_root).expect_err("symlink leaf");
        assert!(
            matches!(&err, DirDeleteError::Uncontained { reason, .. } if reason.contains("symlink")),
            "{err}"
        );
        assert!(target.exists());
    }
}

#[test]
fn a_symlinked_parent_escape_is_refused() {
    let fx = GitWorktreeFixture::new();
    let out = outside_dir(&fx);
    std::fs::create_dir_all(out.join("proj")).expect("mkdir proj");
    let link = fx.repos_root.join("escape");
    std::os::unix::fs::symlink(&out, &link).expect("symlink");
    let sessions = [session(
        "s1",
        &link.join("proj"),
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("symlinked parent");
    assert!(matches!(err, DirDeleteError::Uncontained { .. }), "{err}");
    assert!(out.join("proj").exists());
}

#[test]
fn the_root_itself_is_refused() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repos_root,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("the root");
    assert!(matches!(err, DirDeleteError::Uncontained { .. }), "{err}");
    assert!(fx.repo.exists());
}

// ---- AC3: refusals ---------------------------------------------------------

#[test]
fn dirty_work_is_refused() {
    let fx = GitWorktreeFixture::new();
    std::fs::write(fx.repo.join("wip.txt"), "unsaved\n").expect("write wip");
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("dirty");
    assert!(matches!(err, DirDeleteError::Dirty { .. }), "{err}");
    assert!(fx.repo.join("wip.txt").exists());
}

#[test]
fn unpushed_work_is_refused() {
    let fx = GitWorktreeFixture::new();
    GitWorktreeFixture::commit_unpushed(&fx.repo);
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("unpushed");
    assert!(
        matches!(&err, DirDeleteError::Dirty { reason, .. } if reason.contains("1 unpushed")),
        "{err}"
    );
    assert!(fx.repo.exists());
}

#[test]
fn a_failed_dirt_check_refuses() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let probes = FakeProbes {
        dirt: Some("dirty-check failed: git timed out".into()),
        ..FakeProbes::default()
    };
    let err = plan_dir_delete(input(&sessions, &fx.repos_root, &[]), &probes).expect_err("unknown");
    assert!(matches!(err, DirDeleteError::Dirty { .. }), "{err}");
}

#[test]
fn a_live_process_in_the_directory_is_refused() {
    let fx = GitWorktreeFixture::new();
    let sub = fx.repo.join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir sub");
    std::fs::write(sub.join(".gitkeep"), "").expect("write");
    GitWorktreeFixture::commit_all_and_push(&fx.repo, "sub");
    let proc = StandingProcess::in_dir(&sub);
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan(&sessions, &fx.repos_root).expect_err("live process under the dir");
    match &err {
        DirDeleteError::LiveProcess { reason, .. } => assert!(proc.named_in(reason), "{reason}"),
        other => panic!("expected LiveProcess, got {other}"),
    }
    assert!(fx.repo.exists());
}

#[test]
fn an_unanswerable_liveness_probe_refuses() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let probes = FakeProbes {
        live: probe_cannot_answer(&fx.repo),
        ..FakeProbes::default()
    };
    let err = plan_dir_delete(input(&sessions, &fx.repos_root, &[]), &probes).expect_err("unknown");
    assert!(matches!(err, DirDeleteError::LiveProcess { .. }), "{err}");
}

#[test]
fn an_architect_session_is_refused() {
    let fx = GitWorktreeFixture::new();
    for kind in [SessionKind::Architect, SessionKind::ArchitectAux] {
        // Even a stopped Architect record protects its directory.
        let sessions = [session("s1", &fx.repo, Some(kind), "stopped")];
        let err = plan(&sessions, &fx.repos_root).expect_err("architect");
        assert!(
            matches!(&err, DirDeleteError::Protected(m) if m.contains("does not delete")),
            "{err}"
        );
    }
    // Another session's Architect record working inside the directory.
    let sessions = [
        session("s1", &fx.repo, Some(SessionKind::Ordinary), "stopped"),
        session(
            "a1",
            &fx.repo.join("sub"),
            Some(SessionKind::Architect),
            "active",
        ),
    ];
    let err = plan(&sessions, &fx.repos_root).expect_err("architect occupant");
    assert!(matches!(err, DirDeleteError::Protected(_)), "{err}");
    assert!(fx.repo.exists());
}

#[test]
fn an_unreported_kind_is_refused() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session("s1", &fx.repo, None, "stopped")];
    let err = plan(&sessions, &fx.repos_root).expect_err("no kind");
    assert!(matches!(err, DirDeleteError::Protected(_)), "{err}");
}

#[test]
fn an_architect_directory_is_refused() {
    let fx = GitWorktreeFixture::new();
    let arch = [fx.repo.join("architect")];
    std::fs::create_dir_all(&arch[0]).expect("mkdir");
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let err = plan_dir_delete(input(&sessions, &fx.repos_root, &arch), &OsProbes)
        .expect_err("architect dir");
    assert!(matches!(err, DirDeleteError::ArchitectDir { .. }), "{err}");
}

#[test]
fn a_running_session_is_refused() {
    let fx = GitWorktreeFixture::new();
    for state in ["active", "provisioning", "something-new"] {
        let sessions = [session("s1", &fx.repo, Some(SessionKind::Ordinary), state)];
        let err = plan(&sessions, &fx.repos_root).expect_err(state);
        assert!(
            matches!(err, DirDeleteError::Running { .. }),
            "{state}: {err}"
        );
    }
    // A sibling session running in a subdirectory also refuses.
    let sessions = [
        session("s1", &fx.repo, Some(SessionKind::Ordinary), "stopped"),
        session(
            "s2",
            &fx.repo.join(".worktrees/x"),
            Some(SessionKind::Ordinary),
            "active",
        ),
    ];
    let err = plan(&sessions, &fx.repos_root).expect_err("occupant running");
    assert!(
        matches!(&err, DirDeleteError::Running { running, .. } if running == &["tm-s2".to_string()]),
        "{err}"
    );
    assert!(fx.repo.exists());
}

// ---- AC4: confirmation and dry run -----------------------------------------

#[test]
fn dry_run_removes_nothing_and_reports_the_same_plan() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let auth = FakeAuth::granted();
    let mut asked = 0;
    let report = run_dir_delete(
        input(&sessions, &fx.repos_root, &[]),
        DirDeleteOptions { dry_run: true },
        &mut |_| {
            asked += 1;
            Confirmation::Yes
        },
        &auth,
        &OsProbes,
    )
    .expect("dry run");
    assert!(report.dry_run && !report.removed);
    assert_eq!(asked, 0, "a dry run asks for no confirmation");
    assert_eq!(auth.calls.get(), 0, "a dry run asks for no presence");
    assert!(
        fx.repo.join("README.md").exists(),
        "a dry run removed something"
    );
    // The real run removes exactly the path the dry run previewed.
    let mut seen = None;
    let real = run_dir_delete(
        input(&sessions, &fx.repos_root, &[]),
        DirDeleteOptions::default(),
        &mut |p| {
            seen = Some(p.clone());
            Confirmation::Typed("repo".into())
        },
        &auth,
        &OsProbes,
    )
    .expect("real run");
    assert_eq!(seen.as_ref(), Some(&report.plan));
    assert_eq!(real.plan, report.plan);
    assert!(real.removed && !fx.repo.exists());
}

#[test]
fn acquire_refuses_a_mismatched_typed_name() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let auth = FakeAuth::granted();
    for typed in ["", "repo2", "owner/repo"] {
        let err = run_dir_delete(
            input(&sessions, &fx.repos_root, &[]),
            DirDeleteOptions::default(),
            &mut |_| Confirmation::Typed(typed.into()),
            &auth,
            &OsProbes,
        )
        .expect_err(typed);
        assert!(matches!(err, DirDeleteError::NotConfirmed { .. }), "{err}");
    }
    assert_eq!(
        auth.calls.get(),
        0,
        "no presence prompt before a confirmation"
    );
    assert!(fx.repo.exists());
}

// ---- AC5: presence ---------------------------------------------------------

#[test]
fn acquire_refuses_without_presence() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    for outcome in [
        PresenceOutcome::Declined("cancelled".into()),
        PresenceOutcome::Unavailable("over SSH".into()),
    ] {
        let auth = FakeAuth::new(outcome);
        let err = run_dir_delete(
            input(&sessions, &fx.repos_root, &[]),
            DirDeleteOptions::default(),
            &mut yes,
            &auth,
            &OsProbes,
        )
        .expect_err("no presence");
        assert!(matches!(err, DirDeleteError::Presence(_)), "{err}");
        assert_eq!(auth.calls.get(), 1);
    }
    assert!(fx.repo.join("README.md").exists());
}

#[test]
fn acquire_rechecks_the_disk_guards_after_presence() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    // Clean at plan time (call 1), dirty by the time presence is granted.
    let probes = FakeProbes {
        dirt_from_call: Some(2),
        ..FakeProbes::default()
    };
    let auth = FakeAuth::granted();
    let err = run_dir_delete(
        input(&sessions, &fx.repos_root, &[]),
        DirDeleteOptions::default(),
        &mut yes,
        &auth,
        &probes,
    )
    .expect_err("dirty after the prompt");
    assert!(matches!(err, DirDeleteError::Dirty { .. }), "{err}");
    assert_eq!(auth.calls.get(), 1);
    assert!(fx.repo.exists());
}

#[test]
fn a_confirmed_present_delete_removes_a_clean_pushed_checkout() {
    let fx = GitWorktreeFixture::new();
    let sessions = [session(
        "s1",
        &fx.repo,
        Some(SessionKind::Ordinary),
        "stopped",
    )];
    let auth = FakeAuth::granted();
    let report = run_dir_delete(
        input(&sessions, &fx.repos_root, &[]),
        DirDeleteOptions::default(),
        &mut yes,
        &auth,
        &OsProbes,
    )
    .expect("delete");
    assert!(report.removed);
    assert_eq!(auth.calls.get(), 1);
    assert!(!fx.repo.exists(), "the checkout is still there");
    assert!(
        fx.repos_root.join("owner").exists(),
        "only the session's directory goes"
    );
}
