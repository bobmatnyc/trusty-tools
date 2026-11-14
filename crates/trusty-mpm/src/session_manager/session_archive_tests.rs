//! Tests for [`SessionManager::archive_session`] (#10991). Hermetic: a scratch
//! manager over the recording `FakeTmuxDriver`, a scratch registry, and a
//! scratch archive root.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::super::archive_move::{ARCHIVE_DIR_MODE, read_manifest};
use super::super::tests::{FakeTmuxDriver, seed_record};
use super::{ArchiveOptions, SessionArchiveError};
use crate::project::{Project, ProjectRegistry};
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionKind, SessionManager};

const REPO: &str = "https://github.com/acme/widget";

struct Fixture {
    dir: tempfile::TempDir,
    reg_dir: tempfile::TempDir,
    archive: tempfile::TempDir,
    mgr: SessionManager,
    registry: ProjectRegistry,
}

impl Fixture {
    fn root(&self) -> PathBuf {
        self.archive.path().join("archive")
    }
}

async fn fixture() -> Fixture {
    let dir = crate::test_support::hermetic_temp_dir();
    let reg_dir = crate::test_support::hermetic_temp_dir();
    let archive = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");
    let registry = ProjectRegistry::load(reg_dir.path())
        .await
        .expect("registry");
    registry
        .register(Project {
            name: "widget".into(),
            repo_url: REPO.into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
            project_key: Some("acme/widget".into()),
            decommissioned_at: None,
            archived_at: None,
        })
        .await
        .expect("register");
    Fixture {
        dir,
        reg_dir,
        archive,
        mgr,
        registry,
    }
}

/// Seed a record bound to `widget`; return its id and workspace (with a file).
async fn seed(
    f: &Fixture,
    state: ManagedSessionState,
    kind: SessionKind,
) -> (ManagedSessionId, PathBuf) {
    let id = ManagedSessionId::new();
    let ws = seed_record(&f.mgr, &f.dir, id, state, false).await;
    std::fs::write(ws.join("notes.md"), "keep me").expect("write");
    let mut record = f.mgr.get(&id).await.expect("seeded");
    record.repo_url = Some(REPO.into());
    record.kind = kind;
    f.mgr
        .store
        .write()
        .await
        .upsert(record)
        .await
        .expect("bind");
    (id, ws)
}

async fn archive(
    f: &Fixture,
    id: &ManagedSessionId,
    dry_run: bool,
    force: bool,
) -> Result<super::SessionArchiveReport, SessionArchiveError> {
    f.mgr
        .archive_session(
            &f.registry,
            id,
            ArchiveOptions { dry_run, force },
            &f.root(),
        )
        .await
}

async fn archived(f: &Fixture) -> bool {
    f.registry.get("widget").await.expect("row").is_archived()
}

/// Closure 1: the directory moves under a 0700 root, the manifest records the
/// origin, the project is stamped, and the record is kept.
#[tokio::test]
async fn archive_session_moves_the_directory_and_marks_the_project() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Stopped, SessionKind::default()).await;

    let report = archive(&f, &id, false, false).await.expect("archive");

    assert!(!ws.exists(), "the directory was not moved");
    let manifest = read_manifest(&report.plan.entry).expect("manifest on disk");
    assert_eq!(
        manifest.origin_path,
        std::fs::canonicalize(f.dir.path())
            .expect("canon")
            .join(ws.file_name().expect("leaf"))
    );
    assert_eq!(
        std::fs::read_to_string(manifest.archived_path.join("notes.md")).expect("moved"),
        "keep me"
    );
    assert_eq!(
        std::fs::metadata(f.root())
            .expect("root")
            .permissions()
            .mode()
            & 0o777,
        ARCHIVE_DIR_MODE
    );
    assert!(archived(&f).await, "the project was not marked archived");
    assert!(f.mgr.get(&id).await.is_ok(), "the record must be kept");
}

/// #10991 critic HIGH: archiving one worktree session of a project that still
/// has another session elsewhere moves that worktree but does not mark (and so
/// does not hide) the project.
#[tokio::test]
async fn archiving_one_worktree_session_leaves_a_live_project_unarchived() {
    let f = fixture().await;
    let (done, done_ws) = seed(&f, ManagedSessionState::Stopped, SessionKind::default()).await;
    let (_other, other_ws) = seed(&f, ManagedSessionState::Stopped, SessionKind::default()).await;

    let report = archive(&f, &done, false, false).await.expect("archive");

    assert!(!done_ws.exists(), "the worktree was not moved");
    assert!(other_ws.is_dir(), "the other session's worktree moved");
    assert!(!report.project_archived, "the report claims a stamp");
    assert!(
        !archived(&f).await,
        "a project with another live session was marked archived"
    );
}

/// Closure 2: the dry run is the same path, stopping before any change.
#[tokio::test]
async fn archive_dry_run_changes_nothing() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Active, SessionKind::default()).await;

    let report = archive(&f, &id, true, true).await.expect("dry run");

    assert!(report.dry_run && report.manifest.is_none());
    assert_eq!(report.stopped, vec![id.to_string()], "names who would stop");
    assert!(
        ws.join("notes.md").is_file(),
        "a dry run moved the directory"
    );
    assert!(!f.root().exists(), "a dry run created the archive root");
    assert_eq!(
        f.mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Active
    );
    assert!(!archived(&f).await);
}

/// Closure 2: a running session refuses without `force` — in the dry run too.
#[tokio::test]
async fn archive_refuses_a_running_session_without_force() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Active, SessionKind::default()).await;

    for dry_run in [true, false] {
        let err = archive(&f, &id, dry_run, false)
            .await
            .expect_err("must refuse");
        assert!(
            matches!(&err, SessionArchiveError::Running { running, .. } if running == &vec![id.to_string()]),
            "{err}"
        );
    }
    assert!(ws.join("notes.md").is_file());
    assert!(!archived(&f).await);
}

#[tokio::test]
async fn archive_force_stops_the_session_first() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Active, SessionKind::default()).await;

    let report = archive(&f, &id, false, true).await.expect("forced");

    assert_eq!(report.stopped, vec![id.to_string()]);
    assert_eq!(
        f.mgr.get(&id).await.expect("record").state,
        ManagedSessionState::Stopped
    );
    assert!(!ws.exists());
}

#[tokio::test]
async fn archive_refuses_an_architect_session() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Active, SessionKind::Architect).await;

    let err = archive(&f, &id, false, true).await.expect_err("protected");

    assert!(
        matches!(&err, SessionArchiveError::Protected(why) if why.contains("archive")),
        "{err}"
    );
    assert!(ws.is_dir());
}

/// Fail-open check: a forced stop that fails moves nothing.
#[tokio::test]
async fn archive_failed_stop_moves_nothing() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Active, SessionKind::default()).await;
    // A corrupt Architect sidecar makes the kill floor undeterminable, so the
    // stop aborts (see `decommission_project_failed_stop_leaves_the_project_active`).
    let sidecars = f.dir.path().join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    std::fs::write(sidecars.join("1.architect-session"), "not json").expect("corrupt");

    let err = archive(&f, &id, false, true).await.expect_err("stop fails");

    assert!(
        matches!(&err, SessionArchiveError::StopFailed { failed, .. } if failed.len() == 1),
        "{err}"
    );
    assert!(ws.join("notes.md").is_file(), "moved after a failed stop");
    assert!(!archived(&f).await);
}

/// Fail-open check: an unreadable registry stops the archive before the move.
#[tokio::test]
async fn archive_unreadable_registry_moves_nothing() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Stopped, SessionKind::default()).await;
    std::fs::write(f.reg_dir.path().join("projects.json"), "{ not json").expect("corrupt");

    let err = archive(&f, &id, false, false).await.expect_err("registry");

    assert!(matches!(err, SessionArchiveError::Registry(_)), "{err}");
    assert!(ws.join("notes.md").is_file());
}

/// Fail-open check: a move that completed but whose project stamp failed is an
/// error naming the archive entry, never a silent success.
#[tokio::test]
async fn archive_reports_a_failed_project_stamp() {
    let f = fixture().await;
    let (id, ws) = seed(&f, ManagedSessionState::Stopped, SessionKind::default()).await;
    // Reads still work; the registry's write (a temp file + rename) does not.
    let reg = f.reg_dir.path();
    std::fs::set_permissions(reg, std::fs::Permissions::from_mode(0o500)).expect("lock");

    let result = archive(&f, &id, false, false).await;
    std::fs::set_permissions(reg, std::fs::Permissions::from_mode(0o755)).expect("unlock");

    let err = result.expect_err("the stamp must fail");
    match &err {
        SessionArchiveError::MarkFailed { entry, project, .. } => {
            assert_eq!(project, "widget");
            assert!(read_manifest(entry).is_ok(), "the archive is complete");
        }
        other => panic!("expected MarkFailed, got {other}"),
    }
    assert!(!ws.exists());
}

#[tokio::test]
async fn archive_unknown_session_errors() {
    let f = fixture().await;
    let err = archive(&f, &ManagedSessionId::new(), true, false)
        .await
        .expect_err("unknown");
    assert!(matches!(err, SessionArchiveError::Session(_)), "{err}");
}
