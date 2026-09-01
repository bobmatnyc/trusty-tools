//! Scheduler tests for the cloud log drain (#6535).
//!
//! Why: the two properties worth proving are that a tick actually uploads and
//! dedupes, and that no failure arm can produce a success record. Both are
//! reachable without S3 — the core ships a `file://` adapter for exactly this.
//! What: every test drives a real drain against a `tempfile::TempDir`, so
//! nothing here touches the network, the developer's home directory, or their
//! real config file.

use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::core::trusty_tools_config::LogDrainConfig;

/// Build a `log_drain:` config aimed at `dest_dir`, collecting `log_dir`.
///
/// `github_id` and `session_id` are pinned so no test shells out to `gh` or
/// asserts against the developer's own GitHub account.
fn enabled_config(dest_dir: &Path, log_dir: &Path) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.display())),
            github_id: Some("octocat".to_string()),
            session_id: Some("sess-fixture".to_string()),
            sources: vec![crate::core::trusty_tools_config::LogDrainSourceConfig {
                crate_name: Some("trusty-mpm".to_string()),
                root: Some(log_dir.display().to_string()),
                include: vec!["*.log".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A config whose `log_drain:` section will not resolve.
fn broken_config(destination: &str) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            destination: Some(destination.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Resolve `config` to the plan the tick functions take.
fn plan_of(config: &TrustyToolsConfig, home: &Path) -> ResolvedLogDrain {
    match resolve_log_drain(config, home).expect("fixture config resolves") {
        LogDrainSetting::Enabled(plan) => *plan,
        LogDrainSetting::Disabled => panic!("fixture config should be enabled"),
    }
}

/// The pinned identity the fixture config uploads under.
fn fixture_target() -> DrainTarget {
    DrainTarget {
        github_id: "octocat".to_string(),
        session_id: "sess-fixture".to_string(),
    }
}

/// A temp directory holding one log file with `body`.
fn log_dir_with(tmp: &TempDir, body: &str) -> std::path::PathBuf {
    let dir = tmp.path().join("logs");
    std::fs::create_dir_all(&dir).expect("create log dir");
    std::fs::write(dir.join("trusty-mpm.log"), body).expect("write log file");
    dir
}

#[tokio::test]
async fn a_successful_tick_uploads_and_records_success() {
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs = log_dir_with(&tmp, "INFO first line\nINFO second line\n");
    let state = tmp.path().join("state");

    let config = enabled_config(&dest, &logs);
    let plan = plan_of(&config, tmp.path());
    let status = run_tick(&plan, &state, &fixture_target()).await;

    assert_eq!(status.outcome, DrainOutcome::Success, "{}", status.detail);
    assert_eq!(status.uploaded, 1, "{}", status.detail);
    assert_eq!(status.scheme.as_deref(), Some("file"));

    // The object landed at `<github-id>/<session>/logs/<crate>/<file>`, which
    // is the epic's key layout — proof this drained rather than merely
    // reporting that it had.
    let key = dest
        .join("octocat")
        .join("sess-fixture")
        .join("logs")
        .join("trusty-mpm")
        .join("trusty-mpm.log");
    assert!(
        key.exists(),
        "expected an uploaded object at {}",
        key.display()
    );
}

#[tokio::test]
async fn a_second_tick_dedupes() {
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs = log_dir_with(&tmp, "INFO only line\n");
    let state = tmp.path().join("state");

    let config = enabled_config(&dest, &logs);
    let plan = plan_of(&config, tmp.path());
    let target = fixture_target();

    let first = run_tick(&plan, &state, &target).await;
    assert_eq!(first.uploaded, 1, "{}", first.detail);

    let second = run_tick(&plan, &state, &target).await;
    assert_eq!(second.outcome, DrainOutcome::Success, "{}", second.detail);
    assert_eq!(second.uploaded, 0, "{}", second.detail);
    assert_eq!(second.skipped_unchanged, 1, "{}", second.detail);
}

#[tokio::test]
async fn a_failing_destination_records_failed() {
    let tmp = TempDir::new().expect("tempdir");
    // A `file://` root nested under a regular FILE: `create_dir_all` cannot
    // make it, so `ObjectStoreDestination::connect` fails.
    let blocker = tmp.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let dest = blocker.join("dest");
    let logs = log_dir_with(&tmp, "INFO line\n");
    let state = tmp.path().join("state");

    let config = enabled_config(&dest, &logs);
    let plan = plan_of(&config, tmp.path());
    let status = run_tick(&plan, &state, &fixture_target()).await;

    // The fail-open guard: a destination that cannot be reached must never
    // record a drained pass.
    assert_eq!(status.outcome, DrainOutcome::Failed, "{}", status.detail);
    assert_eq!(status.uploaded, 0);
    assert!(
        status.detail.contains("cannot reach the destination"),
        "unhelpful detail: {}",
        status.detail
    );
}

#[tokio::test]
async fn a_failed_run_is_not_reported_as_drained_by_the_doctor_row() {
    let tmp = TempDir::new().expect("tempdir");
    let blocker = tmp.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let dest = blocker.join("dest");
    let logs = log_dir_with(&tmp, "INFO line\n");
    let root = tmp.path().join("framework");

    let config = enabled_config(&dest, &logs);
    let status = drain_once(&config, &root, tmp.path()).await;
    assert_eq!(status.outcome, DrainOutcome::Failed, "{}", status.detail);

    // And the record survives to the daemonless doctor read, which is the only
    // channel between the scheduler and `tm doctor`.
    let persisted = load_status(&state_dir(&root)).expect("status was written");
    assert_eq!(persisted.outcome, DrainOutcome::Failed);
    assert_eq!(persisted.detail, status.detail);
}

#[tokio::test]
async fn a_disabled_config_records_skipped() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("framework");
    let status = drain_once(&TrustyToolsConfig::default(), &root, tmp.path()).await;
    assert_eq!(status.outcome, DrainOutcome::SkippedDisabled);
    assert_eq!(status.destination, None);
}

#[tokio::test]
async fn a_config_error_records_failed() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("framework");
    let config = broken_config("not-a-uri");

    // A malformed section is a hard error, never a silent skip — so it records
    // FAILED, not SkippedDisabled.
    let status = drain_once(&config, &root, tmp.path()).await;
    assert_eq!(status.outcome, DrainOutcome::Failed, "{}", status.detail);
    assert!(
        status.detail.starts_with("config error:"),
        "unhelpful detail: {}",
        status.detail
    );
}

#[test]
fn should_spawn_matches_the_setting() {
    let tmp = TempDir::new().expect("tempdir");
    assert_eq!(
        should_spawn(&TrustyToolsConfig::default(), tmp.path()),
        Ok(false)
    );

    let config = enabled_config(&tmp.path().join("dest"), &tmp.path().join("logs"));
    assert_eq!(should_spawn(&config, tmp.path()), Ok(true));

    let broken = broken_config("gs://bucket/prefix");
    assert!(
        should_spawn(&broken, tmp.path()).is_err(),
        "a reserved scheme must refuse startup rather than spawn a doomed loop"
    );
}

#[test]
fn status_round_trips_through_the_state_dir() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("state");
    assert_eq!(load_status(&dir), None, "nothing recorded yet");

    let status = config_error_status("destination is invalid");
    save_status(&dir, &status);
    assert_eq!(load_status(&dir).as_ref(), Some(&status));
}

#[test]
fn session_id_is_stable_across_calls() {
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs = tmp.path().join("logs");
    let state = tmp.path().join("state");

    // With no configured id the drain mints one and persists it: a per-boot id
    // would re-upload every log file under a fresh prefix on every restart.
    let mut config = enabled_config(&dest, &logs);
    config
        .log_drain
        .as_mut()
        .expect("section present")
        .session_id = None;
    let plan = plan_of(&config, tmp.path());

    let first = resolve_session_id(&plan, &state).expect("mints an id");
    let second = resolve_session_id(&plan, &state).expect("reads the persisted id");
    assert!(!first.is_empty());
    assert_eq!(first, second);
}

#[test]
fn a_configured_session_id_wins() {
    let tmp = TempDir::new().expect("tempdir");
    let config = enabled_config(&tmp.path().join("dest"), &tmp.path().join("logs"));
    let plan = plan_of(&config, tmp.path());
    assert_eq!(
        resolve_session_id(&plan, &tmp.path().join("state")).expect("resolves"),
        "sess-fixture"
    );
}

#[tokio::test]
async fn the_loop_exits_on_cancel() {
    let tmp = TempDir::new().expect("tempdir");
    let cancel = tokio_util::sync::CancellationToken::new();
    // Cancelled BEFORE the loop starts, so the pre-tick guard returns without a
    // pass — this test must never read the developer's real config file or
    // upload anything.
    cancel.cancel();
    let handle = tokio::spawn(log_drain_loop(
        tmp.path().join("framework"),
        tmp.path().to_path_buf(),
        cancel.child_token(),
    ));
    // A loop that ignored its token would hang here until the test harness
    // timed out; joining is the assertion.
    handle.await.expect("the loop exits cleanly on cancel");
}
