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

/// The `<owner>/<project>` every fixture config drains under.
///
/// Pinned rather than resolved from git: a test that read the developer's own
/// checkout would assert against their remote (#6657).
const FIXTURE_OWNER: &str = "octocat";
const FIXTURE_PROJECT: &str = "fixtures";

/// Build a `log_drain:` config aimed at `dest_dir`, collecting `log_dir`.
///
/// `owner` and `project` are pinned at the section level, so the temp log
/// directories need not be checkouts.
fn enabled_config(dest_dir: &Path, log_dir: &Path) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.display())),
            owner: Some(FIXTURE_OWNER.to_string()),
            project: Some(FIXTURE_PROJECT.to_string()),
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

/// One `sources[]` entry over `root`, optionally pinned to its own destination.
fn source_entry(
    crate_name: &str,
    root: &Path,
    destination: Option<&Path>,
) -> crate::core::trusty_tools_config::LogDrainSourceConfig {
    crate::core::trusty_tools_config::LogDrainSourceConfig {
        crate_name: Some(crate_name.to_string()),
        root: Some(root.display().to_string()),
        include: vec!["*.log".to_string()],
        destination: destination.map(|d| format!("file://{}", d.display())),
        ..Default::default()
    }
}

/// Two sources, two destinations: one inherits the section default, one
/// overrides it (#6657).
fn two_destination_config(
    default_dest: &Path,
    inheriting_logs: &Path,
    override_dest: &Path,
    overriding_logs: &Path,
) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", default_dest.display())),
            owner: Some(FIXTURE_OWNER.to_string()),
            project: Some(FIXTURE_PROJECT.to_string()),
            sources: vec![
                source_entry("trusty-mpm", inheriting_logs, None),
                source_entry("trusty-code", overriding_logs, Some(override_dest)),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Where a drained file for `crate_name` lands under `dest`.
///
/// `<owner>/<project>/<crate>/<file>` — the #6657 layout, with no session
/// segment and no `logs/` interlayer.
fn drained_path(dest: &Path, crate_name: &str, file: &str) -> std::path::PathBuf {
    dest.join(FIXTURE_OWNER)
        .join(FIXTURE_PROJECT)
        .join(crate_name)
        .join(file)
}

/// A temp directory named `name` holding one `<name>.log` file.
fn named_log_dir(tmp: &TempDir, name: &str) -> std::path::PathBuf {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).expect("create log dir");
    std::fs::write(dir.join(format!("{name}.log")), "INFO a line\n").expect("write log file");
    dir
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
    let status = run_tick(&plan, &state).await;

    assert_eq!(status.outcome, DrainOutcome::Success, "{}", status.detail);
    assert_eq!(status.uploaded, 1, "{}", status.detail);
    assert_eq!(status.destinations.len(), 1);
    assert_eq!(status.destinations[0].scheme, "file");

    // The object landed at `<owner>/<project>/<crate>/<file>`, which is the
    // #6657 key layout — proof this drained rather than merely reporting that
    // it had.
    let key = drained_path(&dest, "trusty-mpm", "trusty-mpm.log");
    assert!(
        key.exists(),
        "expected an uploaded object at {}",
        key.display()
    );
    assert_eq!(
        status.destinations[0].project,
        format!("{FIXTURE_OWNER}/{FIXTURE_PROJECT}"),
        "the record names the project it uploaded under"
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
    let first = run_tick(&plan, &state).await;
    assert_eq!(first.uploaded, 1, "{}", first.detail);

    let second = run_tick(&plan, &state).await;
    assert_eq!(second.outcome, DrainOutcome::Success, "{}", second.detail);
    assert_eq!(second.uploaded, 0, "{}", second.detail);
    assert_eq!(second.skipped_unchanged, 1, "{}", second.detail);
}

#[tokio::test]
async fn a_ceiling_skip_is_decided_once_across_ticks() {
    // #6547: the drain re-stat'd ~40 permanently-oversize files every 15-minute
    // cycle and re-logged the same WARN — 1,276 of them in 48 hours. The
    // scheduler is the loop that produced them, so the "decided once" property
    // is proved here at tick granularity, not only inside `run_once`.
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs = log_dir_with(&tmp, &"x".repeat(4096));
    let state = tmp.path().join("state");

    let mut config = enabled_config(&dest, &logs);
    config
        .log_drain
        .as_mut()
        .expect("fixture section")
        .max_file_bytes = Some(1024);
    let plan = plan_of(&config, tmp.path());
    let first = run_tick(&plan, &state).await;
    assert_eq!(first.outcome, DrainOutcome::Success, "{}", first.detail);
    assert_eq!(first.uploaded, 0, "{}", first.detail);
    assert!(
        first
            .detail
            .contains("1 over the size ceiling (1 newly recorded)"),
        "the first tick decides: {}",
        first.detail
    );

    let second = run_tick(&plan, &state).await;
    assert!(
        second
            .detail
            .contains("1 over the size ceiling (0 newly recorded)"),
        "a later tick must not re-decide an unchanged file: {}",
        second.detail
    );
}

#[tokio::test]
async fn the_wire_ceiling_reaches_the_drain_config() {
    // The knob #6547 added has to survive config → plan → `DrainConfig`, or the
    // bound that actually governs memory is unreachable from YAML.
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs = log_dir_with(&tmp, "INFO a line that will not gzip under 8 bytes\n");
    let state = tmp.path().join("state");

    let mut config = enabled_config(&dest, &logs);
    config
        .log_drain
        .as_mut()
        .expect("fixture section")
        .max_wire_bytes = Some(8);
    let plan = plan_of(&config, tmp.path());
    assert_eq!(plan.max_wire_bytes, 8);

    let status = run_tick(&plan, &state).await;
    assert_eq!(status.uploaded, 0, "{}", status.detail);
    assert!(
        status
            .detail
            .contains("1 over the size ceiling (1 newly recorded)"),
        "the wire ceiling must be reported like any other skip: {}",
        status.detail
    );
}

#[tokio::test]
async fn two_destinations_each_get_their_own_pass() {
    // #6657: the epic's whole point — one daemon, two accounts. Two `file://`
    // roots stand in for two buckets, so the property is provable with no
    // network: each source's bytes land under its OWN destination and nowhere
    // else.
    let tmp = TempDir::new().expect("tempdir");
    let dest_a = tmp.path().join("dest-a");
    let dest_b = tmp.path().join("dest-b");
    let logs_a = named_log_dir(&tmp, "alpha");
    let logs_b = named_log_dir(&tmp, "beta");
    let state = tmp.path().join("state");

    let config = two_destination_config(&dest_a, &logs_a, &dest_b, &logs_b);
    let plan = plan_of(&config, tmp.path());
    assert_eq!(
        plan.destinations.len(),
        2,
        "two groups, one per destination"
    );

    let status = run_tick(&plan, &state).await;
    assert_eq!(status.outcome, DrainOutcome::Success, "{}", status.detail);
    assert_eq!(status.uploaded, 2, "{}", status.detail);
    assert_eq!(status.destinations.len(), 2);
    assert!(
        status
            .destinations
            .iter()
            .all(|d| d.outcome == DrainOutcome::Success && d.uploaded == 1),
        "each destination uploads its own single file: {:?}",
        status.destinations
    );

    let a = drained_path(&dest_a, "trusty-mpm", "alpha.log");
    let b = drained_path(&dest_b, "trusty-code", "beta.log");
    assert!(a.exists(), "expected {}", a.display());
    assert!(b.exists(), "expected {}", b.display());
    // Neither destination received the other's bytes.
    assert!(!drained_path(&dest_a, "trusty-code", "beta.log").exists());
    assert!(!drained_path(&dest_b, "trusty-mpm", "alpha.log").exists());
}

#[tokio::test]
async fn one_failing_destination_does_not_stop_the_others() {
    // #6657 fail-closed guard: a per-source destination that cannot be reached
    // must be SKIPPED, never retried against the section default. Falling back
    // would put this project's logs in the wrong AWS account, which is exactly
    // what the override exists to prevent.
    let tmp = TempDir::new().expect("tempdir");
    let dest_a = tmp.path().join("dest-a");
    // A `file://` root nested under a regular FILE: `create_dir_all` cannot
    // make it, so connecting destination B fails.
    let blocker = tmp.path().join("not-a-directory");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let dest_b = blocker.join("dest-b");
    let logs_a = named_log_dir(&tmp, "alpha");
    let logs_b = named_log_dir(&tmp, "beta");
    let state = tmp.path().join("state");

    let config = two_destination_config(&dest_a, &logs_a, &dest_b, &logs_b);
    let plan = plan_of(&config, tmp.path());
    let status = run_tick(&plan, &state).await;

    // The tick as a whole failed, but the reachable destination still drained.
    assert_eq!(status.outcome, DrainOutcome::Failed, "{}", status.detail);
    assert_eq!(status.destinations.len(), 2);
    assert_eq!(status.destinations[0].outcome, DrainOutcome::Success);
    assert_eq!(status.destinations[0].uploaded, 1);
    assert_eq!(status.destinations[1].outcome, DrainOutcome::Failed);
    assert!(
        status.destinations[1]
            .detail
            .contains("cannot reach the destination"),
        "unhelpful detail: {}",
        status.destinations[1].detail
    );

    // The skipped source's bytes are nowhere under the working destination.
    assert!(drained_path(&dest_a, "trusty-mpm", "alpha.log").exists());
    assert!(
        !drained_path(&dest_a, "trusty-code", "beta.log").exists(),
        "a failed destination must not fall back to the section default"
    );
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
    let status = run_tick(&plan, &state).await;

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
    assert!(status.destinations.is_empty());
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

#[tokio::test]
async fn a_disabled_source_uploads_nothing_and_is_still_reported() {
    // #6657 deliverable: a project opts out with `enabled: false`. It must run
    // no pass and write no object, while the plan still names it so the doctor
    // row can say the drain is off for that project on purpose.
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    let logs_on = named_log_dir(&tmp, "alpha");
    let logs_off = named_log_dir(&tmp, "beta");
    let state = tmp.path().join("state");

    let mut config = enabled_config(&dest, &logs_on);
    let section = config.log_drain.as_mut().expect("fixture section");
    section.sources[0].crate_name = Some("trusty-mpm".to_string());
    section.sources[0].include = vec!["*.log".to_string()];
    section
        .sources
        .push(crate::core::trusty_tools_config::LogDrainSourceConfig {
            crate_name: Some("trusty-code".to_string()),
            root: Some(logs_off.display().to_string()),
            include: vec!["*.log".to_string()],
            enabled: Some(false),
            ..Default::default()
        });

    let plan = plan_of(&config, tmp.path());
    assert_eq!(plan.destinations.len(), 1, "only the enabled source drains");
    assert_eq!(plan.disabled.len(), 1);
    assert_eq!(plan.disabled[0].crate_name, "trusty-code");

    let status = run_tick(&plan, &state).await;
    assert_eq!(status.outcome, DrainOutcome::Success, "{}", status.detail);
    assert_eq!(status.uploaded, 1, "{}", status.detail);
    assert!(drained_path(&dest, "trusty-mpm", "alpha.log").exists());
    assert!(
        !drained_path(&dest, "trusty-code", "beta.log").exists(),
        "a disabled source must upload nothing"
    );
}
