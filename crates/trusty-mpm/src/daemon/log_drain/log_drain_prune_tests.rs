//! Regression tests for `prune_after_upload` (#6536).
//!
//! Why: the four properties the issue names are each a different arm of
//! [`apply`]'s gating — a manifest-confirmed object prunes once it has been a
//! candidate on two consecutive ticks, a destination whose upload just failed
//! is never even queried for candidates, an oversize file that was never
//! uploaded can never become one, and the `prune_after_upload: false` switch
//! stops candidate collection outright.
//! What: every test drives a real `file://` destination over a
//! `tempfile::TempDir`, exactly like the sibling `log_drain_tests.rs` — no
//! mock `LogDestination` is introduced here.

use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;
use trusty_common::log_drain::{DrainConfig, LogDestination, ObjectStoreDestination, run_once};

use super::*;
use crate::core::trusty_tools_config::{
    LogDrainConfig, LogDrainSourceConfig, ResolvedLogDrain, TrustyToolsConfig, resolve_log_drain,
};

const FIXTURE_OWNER: &str = "octocat";
const FIXTURE_PROJECT: &str = "fixtures";

/// An enabled `log_drain:` config draining `log_dir` to `dest_dir`.
fn fixture_config(
    dest_dir: &Path,
    log_dir: &Path,
    prune_after_upload: Option<bool>,
) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.display())),
            owner: Some(FIXTURE_OWNER.to_string()),
            project: Some(FIXTURE_PROJECT.to_string()),
            prune_after_upload,
            sources: vec![LogDrainSourceConfig {
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

fn resolve(config: &TrustyToolsConfig, home: &Path) -> ResolvedLogDrain {
    match resolve_log_drain(config, home).expect("fixture config resolves") {
        crate::core::trusty_tools_config::LogDrainSetting::Enabled(plan) => *plan,
        crate::core::trusty_tools_config::LogDrainSetting::Disabled => {
            panic!("fixture config should be enabled")
        }
    }
}

fn log_dir_with(tmp: &TempDir, body: &str) -> std::path::PathBuf {
    let dir = tmp.path().join("logs");
    std::fs::create_dir_all(&dir).expect("create log dir");
    std::fs::write(dir.join("trusty-mpm.log"), body).expect("write log file");
    dir
}

/// Whether the destination still has the object one source file drained to.
async fn object_exists(dest_dir: &Path) -> bool {
    let uri =
        trusty_common::log_drain::DestinationUri::parse(&format!("file://{}", dest_dir.display()))
            .expect("uri parses");
    let dest = ObjectStoreDestination::connect(&uri)
        .await
        .expect("connect");
    let target = trusty_common::log_drain::DrainTarget {
        owner: FIXTURE_OWNER.to_string(),
        project: FIXTURE_PROJECT.to_string(),
    };
    dest.head(&target.object_key("trusty-mpm/trusty-mpm.log"))
        .await
        .expect("head")
        .is_some()
}

/// Success path: a manifest-confirmed object prunes on the SECOND tick that
/// confirms it, never the first.
#[tokio::test]
async fn prune_after_upload_deletes_only_on_the_second_confirming_tick() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log_dir = log_dir_with(&tmp, "line one\n");
    let config = fixture_config(dest_root.path(), &log_dir, None);
    let mut plan = resolve(&config, tmp.path());
    assert!(
        plan.prune_after_upload,
        "prune_after_upload defaults ON once the section is enabled (owner ruling 2026-09-01)"
    );
    // Force every manifest entry to be immediately age-eligible, so the test
    // proves the DEBOUNCE gate rather than waiting out a 30-day window.
    plan.prune_retention = Duration::from_secs(0);

    let mut debounce = RetentionDebounce::new();

    let first = super::super::run_tick(&plan, state.path(), &mut debounce).await;
    assert_eq!(first.uploaded, 1, "first tick uploads the file");
    assert_eq!(
        first.pruned, 0,
        "a candidate seen for the first time is never pruned on the same tick"
    );
    assert!(
        object_exists(dest_root.path()).await,
        "the object must still be at the destination after tick one"
    );

    let second = super::super::run_tick(&plan, state.path(), &mut debounce).await;
    assert_eq!(
        second.pruned, 1,
        "the same candidate confirmed on a second consecutive tick is pruned"
    );
    assert!(
        !object_exists(dest_root.path()).await,
        "the destination object is gone after the confirming tick"
    );
    assert_eq!(second.outcome, DrainOutcome::Success);
}

/// A destination whose upload phase just failed gets no prune attempt at all
/// — proven directly against [`apply`]'s gate rather than by forcing a real
/// upload failure, since the invariant under test IS that gate.
#[tokio::test]
async fn prune_after_upload_never_prunes_a_failed_upload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log_dir = log_dir_with(&tmp, "line one\n");
    let config = fixture_config(dest_root.path(), &log_dir, None);
    let mut plan = resolve(&config, tmp.path());
    plan.prune_retention = Duration::from_secs(0);

    // Upload once for real, so an object AND a manifest entry genuinely
    // exist — the only way "never prunes a failed upload" is a meaningful
    // claim rather than a vacuous one.
    let group = &plan.destinations[0];
    let dest = ObjectStoreDestination::connect(&group.destination)
        .await
        .expect("connect");
    let cfg = DrainConfig::new(state.path());
    run_once(&cfg, &dest, &group.target, &group.sources)
        .await
        .expect("run_once uploads");
    assert!(object_exists(dest_root.path()).await);

    // Simulate THIS tick's upload phase having failed for that destination.
    let mut statuses = vec![LogDrainDestinationStatus::failed(
        group,
        "simulated: cannot reach the destination",
    )];
    let mut debounce: RetentionDebounce<PruneCandidate> = RetentionDebounce::new();
    apply(&plan, state.path(), &mut statuses, &mut debounce).await;

    assert_eq!(
        statuses[0].pruned, 0,
        "a destination marked Failed this tick is never even queried for prune candidates"
    );
    assert!(
        object_exists(dest_root.path()).await,
        "the object already at the destination is untouched"
    );
}

/// A file the manifest never recorded — here, skipped for exceeding
/// `max_file_bytes` — can never become a prune candidate: nothing but a
/// [`trusty_common::log_drain::ManifestEntry`] ever counts as "confirmed".
#[tokio::test]
async fn prune_after_upload_never_prunes_a_file_that_was_never_fully_uploaded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    // Oversize relative to the 16-byte ceiling set below.
    let log_dir = log_dir_with(&tmp, &"x".repeat(4096));
    let mut config = fixture_config(dest_root.path(), &log_dir, None);
    config.log_drain.as_mut().expect("section").max_file_bytes = Some(16);
    let mut plan = resolve(&config, tmp.path());
    plan.prune_retention = Duration::from_secs(0);

    let mut debounce = RetentionDebounce::new();
    let first = super::super::run_tick(&plan, state.path(), &mut debounce).await;
    assert_eq!(
        first.uploaded, 0,
        "the file is over the ceiling; never uploaded"
    );
    let second = super::super::run_tick(&plan, state.path(), &mut debounce).await;

    assert_eq!(first.pruned, 0);
    assert_eq!(
        second.pruned, 0,
        "an oversize file has no manifest entry on any tick, so it is never a candidate"
    );
    assert!(
        !object_exists(dest_root.path()).await,
        "it was never uploaded in the first place"
    );
}

/// `prune_after_upload: false` stops candidate collection outright, even past
/// the retention window and across many ticks.
#[tokio::test]
async fn prune_after_upload_respects_the_off_switch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log_dir = log_dir_with(&tmp, "line one\n");
    let config = fixture_config(dest_root.path(), &log_dir, Some(false));
    let mut plan = resolve(&config, tmp.path());
    assert!(
        !plan.prune_after_upload,
        "the operator's `false` must survive resolution"
    );
    plan.prune_retention = Duration::from_secs(0);

    let mut debounce = RetentionDebounce::new();
    let first = super::super::run_tick(&plan, state.path(), &mut debounce).await;
    let second = super::super::run_tick(&plan, state.path(), &mut debounce).await;

    assert_eq!(first.uploaded, 1);
    assert_eq!(first.pruned, 0);
    assert_eq!(
        second.pruned, 0,
        "pruning never runs while the switch is off"
    );
    assert!(
        object_exists(dest_root.path()).await,
        "the object survives indefinitely with pruning switched off"
    );
}
