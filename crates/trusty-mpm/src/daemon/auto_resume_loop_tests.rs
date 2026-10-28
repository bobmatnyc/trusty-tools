//! Tests for the daemon's auto-resume loop and parked notice (#10356).
//!
//! Why: owner ruling D74 moves auto-resume into the daemon and D73 adds the
//! boot grace and one parked-sessions notice. These tests drive the loop's
//! `tick` against a real [`SessionManager`] on a fake tmux driver, with a
//! scripted relauncher standing in for the daemon's real one.
//! What: the cold-start resume, the relaunch failure arm, the console gate
//! (#5208, moved here from the supervisor), and the notice's timing and shape.
//! Test: this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use super::{AutoResumeLoop, park_notice};
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::relaunch::RuntimeRelauncher;
use crate::session_manager::resume_breaker::ResumeBreakerConfig;
use crate::session_manager::{
    FakeNoopTmuxDriver, ManagedSessionState, SessionManager, SessionRecord, StopCause,
};
use crate::supervisor::SupervisorConfig;

/// A relauncher with a fixed verdict that counts its calls.
struct Scripted {
    verdict: Result<(), String>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl RuntimeRelauncher for Scripted {
    async fn relaunch(&self, _record: &SessionRecord) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.verdict.clone()
    }
}

fn scripted(verdict: Result<(), String>) -> Arc<Scripted> {
    Arc::new(Scripted {
        verdict,
        calls: AtomicUsize::new(0),
    })
}

/// A manager with `boot_grace`, a relauncher, and `n` sessions whose runtime
/// exited on its own (`Stopped` / `Unexpected`), each with a real workdir.
async fn fleet(
    dir: &TempDir,
    n: usize,
    boot_grace: Duration,
    relauncher: Arc<Scripted>,
) -> (Arc<SessionManager>, Vec<ManagedSessionId>) {
    let mut mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
        .await
        .expect("session manager");
    mgr.set_resume_breaker_config(ResumeBreakerConfig {
        boot_grace,
        ..ResumeBreakerConfig::default()
    });
    let mgr = Arc::new(mgr);
    assert!(mgr.install_relauncher(relauncher));
    let mut ids = Vec::new();
    for i in 0..n {
        let ws = dir.path().join(format!("ws-{i}"));
        std::fs::create_dir_all(&ws).expect("workspace");
        let record = mgr
            .create(
                format!("cold-{i}"),
                Some(ws.clone()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        mgr.set_workspace(&record.id, ws, ManagedSessionState::Active)
            .await
            .expect("set Active");
        let stopped = mgr
            .mark_runtime_exited_stopped(&record.id)
            .await
            .expect("mark stopped");
        assert_eq!(stopped.stop_cause, Some(StopCause::Unexpected));
        ids.push(record.id);
    }
    (mgr, ids)
}

/// Stamp `cause` on a record directly in the store.
async fn set_cause(mgr: &SessionManager, id: &ManagedSessionId, cause: StopCause) {
    let mut store = mgr.store.write().await;
    let mut record = store.cached_get(id).expect("record");
    record.stop_cause = Some(cause);
    store.upsert(record).await.expect("upsert");
}

/// A sweep config with the given boot flag.
fn cfg(boot_flag: bool) -> SupervisorConfig {
    SupervisorConfig {
        auto_resume: boot_flag,
        classify_idle: false,
        ..SupervisorConfig::default()
    }
}

/// #10356 (owner ruling D74): a cold start resumes the fleet from the daemon.
///
/// What: three `Stopped`/`Unexpected` sessions, no live panes, a daemon loop
/// with auto-resume on. One tick gives each a launch through the relauncher,
/// and each ends `Active`.
/// Test: this function IS the test.
#[tokio::test]
async fn the_daemon_loop_resumes_a_cold_start_fleet() {
    let dir = TempDir::new().expect("tempdir");
    let relauncher = scripted(Ok(()));
    let (mgr, ids) = fleet(&dir, 3, Duration::from_secs(600), relauncher.clone()).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(true), dir.path().join("absent"));

    let tick = driver.tick().await;

    assert_eq!(tick.report.resumed.len(), 3, "{tick:?}");
    assert_eq!(tick.report.resume_failures, 0);
    assert_eq!(
        relauncher.calls.load(Ordering::SeqCst),
        3,
        "each eligible session must get a real launch attempt"
    );
    for id in &ids {
        assert_eq!(
            mgr.get(id).await.expect("record").state,
            ManagedSessionState::Active
        );
    }
}

/// #10356 failure path: a relaunch that finds no runtime, on the daemon path.
///
/// What: the relauncher fails. The tick counts one failure, the record ends
/// `Errored` with exactly one note naming the failure, and the next tick does
/// not retry it (it is no longer `Stopped`).
/// Test: this function IS the test.
#[tokio::test]
async fn a_relaunch_failure_on_the_daemon_path_is_recorded() {
    let dir = TempDir::new().expect("tempdir");
    let relauncher = scripted(Err("no runtime came up in pane 'tm-x'".to_owned()));
    let (mgr, ids) = fleet(&dir, 1, Duration::from_secs(600), relauncher.clone()).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(true), dir.path().join("absent"));

    let first = driver.tick().await;
    assert!(first.report.resumed.is_empty());
    assert_eq!(first.report.resume_failures, 1);
    let record = mgr.get(&ids[0]).await.expect("record");
    assert_eq!(record.state, ManagedSessionState::Errored);
    assert_eq!(
        record.task.matches("[error:").count(),
        1,
        "one failure, one note: {}",
        record.task
    );
    assert!(
        record.task.contains("no runtime came up"),
        "{}",
        record.task
    );

    let second = driver.tick().await;
    assert_eq!(
        second.report.resume_failures, 0,
        "an errored session is not retried"
    );
    assert_eq!(relauncher.calls.load(Ordering::SeqCst), 1);
}

/// #10356 (owner ruling D73, item C): one notice, after the grace window,
/// naming every parked session.
///
/// What: two parked sessions and one deliberately stopped one. No notice
/// inside the grace window; the first tick after it emits one notice naming
/// both parked sessions and not the third; the next tick emits nothing.
/// Test: this function IS the test.
#[tokio::test]
async fn the_park_notice_waits_for_the_grace_window_and_fires_once() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, ids) = fleet(&dir, 3, Duration::from_secs(3600), scripted(Ok(()))).await;
    set_cause(&mgr, &ids[0], StopCause::ResumeFlapping).await;
    set_cause(&mgr, &ids[1], StopCause::ResumeFlapping).await;
    set_cause(&mgr, &ids[2], StopCause::Deliberate).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(false), dir.path().join("absent"));

    assert_eq!(
        driver.tick().await.park_notice,
        None,
        "inside the grace window"
    );

    mgr.end_resume_boot_grace().await;
    let notice = driver
        .tick()
        .await
        .park_notice
        .expect("the first tick after the grace window emits the notice");
    assert!(notice.contains("2 session(s)"), "{notice}");
    for id in &ids[..2] {
        assert!(notice.contains(&id.to_string()), "{notice}");
    }
    assert!(!notice.contains(&ids[2].to_string()), "{notice}");

    assert_eq!(driver.tick().await.park_notice, None, "one notice per boot");
}

/// No parked session, no notice — and the latch still closes, so a session
/// parked later in this boot does not produce a second, late notice.
///
/// Test: this function IS the test.
#[tokio::test]
async fn no_park_notice_when_nothing_is_parked() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, ids) = fleet(&dir, 1, Duration::ZERO, scripted(Ok(()))).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(false), dir.path().join("absent"));
    assert_eq!(driver.tick().await.park_notice, None);
    set_cause(&mgr, &ids[0], StopCause::ResumeFlapping).await;
    assert_eq!(driver.tick().await.park_notice, None);
    assert!(
        park_notice(&mgr.list().await).is_some(),
        "the pure helper still sees it"
    );
}

// ── #5208: the console desired-state drives the DAEMON's sweep ──────────────

/// The console's write, and only that write, makes a running daemon loop
/// resume a stopped session, with no restart and no env var.
///
/// Test: this function IS the test.
#[tokio::test]
async fn the_console_toggle_enables_a_running_daemon_sweep() {
    let dir = TempDir::new().expect("tempdir");
    let state = TempDir::new().expect("tempdir");
    let desired = state.path().join("auto_resume");
    let (mgr, ids) = fleet(&dir, 1, Duration::from_secs(600), scripted(Ok(()))).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(false), &desired);

    assert!(
        driver.tick().await.report.resumed.is_empty(),
        "boot flag off"
    );
    assert_eq!(
        mgr.get(&ids[0]).await.expect("record").state,
        ManagedSessionState::Stopped
    );

    crate::core::auto_resume::write_desired_at(&desired, true).expect("console write");
    assert_eq!(driver.tick().await.report.resumed.len(), 1);
    assert_eq!(
        mgr.get(&ids[0]).await.expect("record").state,
        ManagedSessionState::Active
    );
}

/// An explicit console `false` outranks an env-enabled boot flag.
///
/// Test: this function IS the test.
#[tokio::test]
async fn a_console_disable_overrides_an_env_enabled_daemon() {
    let dir = TempDir::new().expect("tempdir");
    let state = TempDir::new().expect("tempdir");
    let desired = state.path().join("auto_resume");
    crate::core::auto_resume::write_desired_at(&desired, false).expect("console write");
    let (mgr, _ids) = fleet(&dir, 2, Duration::from_secs(600), scripted(Ok(()))).await;
    let mut driver = AutoResumeLoop::new(mgr.clone(), cfg(true), &desired);

    assert!(driver.tick().await.report.resumed.is_empty());
    assert!(
        mgr.list()
            .await
            .iter()
            .all(|r| r.state == ManagedSessionState::Stopped)
    );
}

/// No override file leaves an env-enabled boot flag in force.
///
/// Test: this function IS the test.
#[tokio::test]
async fn an_absent_desired_file_keeps_the_boot_flag() {
    let dir = TempDir::new().expect("tempdir");
    let (mgr, _ids) = fleet(&dir, 1, Duration::from_secs(600), scripted(Ok(()))).await;
    let mut driver = AutoResumeLoop::new(mgr, cfg(true), dir.path().join("absent"));
    assert_eq!(driver.tick().await.report.resumed.len(), 1);
}

/// An unreadable desired-state file must not fail open to auto-resume off.
///
/// What: a DIRECTORY sits where the file belongs, so the read fails with
/// something other than NotFound; an env-enabled loop still resumes.
/// Test: this function IS the test.
#[tokio::test]
async fn an_unreadable_desired_file_does_not_disable_resume() {
    let dir = TempDir::new().expect("tempdir");
    let state = TempDir::new().expect("tempdir");
    let desired = state.path().join("auto_resume");
    std::fs::create_dir(&desired).expect("mkdir over the desired-state path");
    let (mgr, _ids) = fleet(&dir, 1, Duration::from_secs(600), scripted(Ok(()))).await;
    let mut driver = AutoResumeLoop::new(mgr, cfg(true), &desired);
    assert_eq!(driver.tick().await.report.resumed.len(), 1);
}
