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

/// An enabled `log_drain:` config with TWO destinations, each fed by its own
/// source over the same crate name and log filename — so the two destinations'
/// candidates share one `relative_file`, and only their destination identity
/// tells them apart (#7154).
fn fixture_config_two_destinations(
    dest_a: &Path,
    dest_b: &Path,
    log_a: &Path,
    log_b: &Path,
) -> TrustyToolsConfig {
    TrustyToolsConfig {
        log_drain: Some(LogDrainConfig {
            enabled: Some(true),
            owner: Some(FIXTURE_OWNER.to_string()),
            project: Some(FIXTURE_PROJECT.to_string()),
            sources: vec![
                LogDrainSourceConfig {
                    crate_name: Some("trusty-mpm".to_string()),
                    root: Some(log_a.display().to_string()),
                    include: vec!["*.log".to_string()],
                    destination: Some(format!("file://{}", dest_a.display())),
                    ..Default::default()
                },
                LogDrainSourceConfig {
                    crate_name: Some("trusty-mpm".to_string()),
                    root: Some(log_b.display().to_string()),
                    include: vec!["*.log".to_string()],
                    destination: Some(format!("file://{}", dest_b.display())),
                    ..Default::default()
                },
            ],
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

/// #7154: a config reorder between ticks must never transfer one
/// destination's armed prune state to a different destination that now
/// happens to sit at the same plan index.
#[tokio::test]
async fn prune_after_upload_survives_a_destination_reorder_between_ticks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_a = tempfile::tempdir().expect("tempdir");
    let dest_b = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    // Tick 1: only A has a file to drain — B's source directory is empty, so
    // B contributes no prune candidate this tick at all.
    let log_a = log_dir_with(&tmp, "line one\n");
    let log_b = tmp.path().join("logs-b");
    std::fs::create_dir_all(&log_b).expect("create empty log dir for b");

    let config = fixture_config_two_destinations(dest_a.path(), dest_b.path(), &log_a, &log_b);
    let mut plan = resolve(&config, tmp.path());
    plan.prune_retention = Duration::from_secs(0);
    assert_eq!(
        plan.destinations.len(),
        2,
        "two distinct file:// destinations"
    );

    let mut debounce = RetentionDebounce::new();
    let first = super::super::run_tick(&plan, state.path(), &mut debounce).await;
    assert_eq!(first.uploaded, 1, "only A's file uploads this tick");
    assert_eq!(first.pruned, 0, "a first-time candidate is never pruned");

    // Between ticks: B gets its own file for the first time, and the plan is
    // reordered so B now sits at index 0 — the index A's candidate was armed
    // under on tick one.
    std::fs::write(log_b.join("trusty-mpm.log"), "line one\n").expect("write b's file");
    plan.destinations.swap(0, 1);

    let second = super::super::run_tick(&plan, state.path(), &mut debounce).await;

    assert!(
        object_exists(dest_b.path()).await,
        "B's object must survive its own FIRST tick as a candidate, even though \
         its destination now occupies A's old index"
    );
    // A's identity survives the reorder intact: its second real consecutive
    // observation confirms and prunes it, exactly as if the plan had never
    // been reordered — proving the fix resolves by identity, not position.
    assert!(
        !object_exists(dest_a.path()).await,
        "A's own two-tick history, tracked by identity rather than position, \
         still confirms and prunes A"
    );
    assert_eq!(second.outcome, DrainOutcome::Success);
}

/// #7154: a confirmed batch over the sanity cap is refused outright — nothing
/// deleted — and the refusal is visible in the destination's status without
/// marking it `Failed`.
#[tokio::test]
async fn prune_after_upload_refuses_a_batch_over_the_count_ceiling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log_dir = log_dir_with(&tmp, "line one\n");
    let config = fixture_config(dest_root.path(), &log_dir, None);
    let mut plan = resolve(&config, tmp.path());
    plan.prune_retention = Duration::from_secs(0);

    let group = &plan.destinations[0];
    let dest = ObjectStoreDestination::connect(&group.destination)
        .await
        .expect("connect");

    // Seed a manifest whose entry count is over `PRUNE_BATCH_COUNT_CEILING`,
    // bypassing real uploads — `prune_confirmed` only reads keys and deletes,
    // it never re-verifies the underlying file.
    let mut manifest = trusty_common::log_drain::DrainManifest::default();
    let overflow = trusty_common::log_drain::PRUNE_BATCH_COUNT_CEILING + 1;
    for i in 0..overflow {
        manifest.record(trusty_common::log_drain::ManifestEntry {
            relative_file: format!("trusty-mpm/{i}.log"),
            size: 1,
            mtime_unix: 0,
            sha256: "deadbeef".to_string(),
            uploaded_at: chrono::Utc::now().to_rfc3339(),
        });
    }
    manifest
        .save(
            &dest,
            state.path(),
            &group.target.manifest_key(),
            "synthetic",
        )
        .await
        .expect("manifest saves");

    let mut statuses = vec![LogDrainDestinationStatus::failed(group, "placeholder")];
    statuses[0].outcome = DrainOutcome::Success;
    let mut debounce: RetentionDebounce<PruneCandidate> = RetentionDebounce::new();

    // First tick only arms every candidate; the second confirms the whole
    // (oversize) batch and is where the cap must refuse it.
    apply(&plan, state.path(), &mut statuses, &mut debounce).await;
    assert_eq!(
        statuses[0].pruned, 0,
        "a first-time candidate is never pruned"
    );

    apply(&plan, state.path(), &mut statuses, &mut debounce).await;

    assert_eq!(
        statuses[0].pruned, 0,
        "a batch over the count ceiling deletes nothing"
    );
    assert_eq!(
        statuses[0].outcome,
        DrainOutcome::Success,
        "a sanity-cap refusal is not a destination failure"
    );
    assert!(
        statuses[0].detail.contains("prune refused"),
        "the refusal is visible in the detail line: {}",
        statuses[0].detail
    );
}
