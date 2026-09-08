//! The `trusty-agents` daemon's log-drain scheduler (#6537).
//!
//! Why: `trusty_common::log_drain::run_tick` is one pass with no memory of
//! what it last did. This is the interval loop that makes it a running
//! service — spawned once, fire-and-forget, from
//! `api::server::routes::serve_with_config`, beside
//! `listeners::poll::spawn_listeners` (the same "read config, spawn a
//! background task per enabled feature, never fail server startup" pattern
//! that call site already establishes). This daemon exits with its whole
//! process, so an ungraceful loop shutdown loses nothing the next pass would
//! not reattempt.
//! What: [`spawn`] starts the loop as a background task; each iteration
//! re-reads `~/.trusty-agents/config.toml` (an operator's edit takes effect
//! without a restart) and calls `trusty_common::log_drain::run_tick` — the
//! pass itself (resolve, connect, `run_once`, log the verdict) now lives
//! there, shared with trusty-code's identical scheduler (#6537 code-review
//! fix round). `state_dir` — where the local idempotency-manifest cache
//! lives — is a parameter of [`spawn`] rather than resolved with
//! `dirs::home_dir()` inside this file: the production call site
//! (`api::server::routes::serve_with_config`) supplies the real
//! `~/.trusty-agents/log-drain`, and every test drives `tick` with a tempdir
//! instead, so no test ever touches a real home directory.
//! Test: `tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};
use trusty_common::log_drain::{DrainOutcome, run_tick};

use super::resolve::LogDrainConfig;

const CRATE_NAME: &str = "trusty-agents";
const DEFAULT_INCLUDE: &str = "daemon-*.log*";

/// Start the interval loop as a background task.
///
/// `project_root` is the current project's root (`ctrl::detect_self_project()`)
/// — `None` when this daemon serves no particular project. `log_root` is
/// `crate::service::log_dir()`. `state_dir` is where the local idempotency
/// manifest cache lives — the production call site passes
/// `~/.trusty-agents/log-drain`.
pub fn spawn(project_root: Option<PathBuf>, log_root: PathBuf, state_dir: PathBuf) {
    tokio::task::spawn(run_loop(project_root, log_root, state_dir));
}

/// Tick forever: reload config, one pass, then sleep for whatever interval
/// that pass named.
async fn run_loop(project_root: Option<PathBuf>, log_root: PathBuf, state_dir: PathBuf) {
    loop {
        let cfg = crate::mcp::config::GlobalConfig::load().await.log_drain;
        let (outcome, interval) = tick(&cfg, project_root.as_deref(), &log_root, &state_dir).await;
        match outcome {
            DrainOutcome::Success => info!("log_drain: pass complete"),
            DrainOutcome::SkippedDisabled => {}
            DrainOutcome::Failed => warn!("log_drain: pass failed; retrying next tick"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// One full pass over an already-loaded config — a thin wrapper over
/// `trusty_common::log_drain::run_tick` naming this daemon's own crate and
/// include glob. Returns the outcome and the interval the NEXT sleep should
/// use.
async fn tick(
    cfg: &LogDrainConfig,
    project_root: Option<&Path>,
    log_root: &Path,
    state_dir: &Path,
) -> (DrainOutcome, Duration) {
    run_tick(
        cfg,
        log_root,
        project_root,
        state_dir,
        CRATE_NAME,
        DEFAULT_INCLUDE,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::log_drain::DEFAULT_INTERVAL_SECS;

    /// Disabled config: `tick` never touches the destination or the network.
    #[tokio::test]
    async fn opt_out_skips_the_pass() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("daemon-abc-8080.log"), "hello\n").expect("seed log");
        let state_dir = tempfile::tempdir().expect("tempdir");

        let (outcome, interval) = tick(
            &LogDrainConfig::default(),
            None,
            log_root.path(),
            state_dir.path(),
        )
        .await;
        assert_eq!(outcome, DrainOutcome::SkippedDisabled);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }

    /// An enabled plan against a `file://` destination uploads the seeded
    /// daemon log on the first tick — into the INJECTED `state_dir`, never a
    /// real `~/.trusty-agents/log-drain` (#6537 code-review fix round) — and
    /// a second tick — nothing changed — is idempotent.
    #[tokio::test]
    async fn an_enabled_tick_uploads_into_the_injected_state_dir_and_is_idempotent() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("daemon-abc-8080.log"), "hello\n").expect("seed log");
        let dest_dir = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");

        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..LogDrainConfig::default()
        };

        let (first, _) = tick(&cfg, None, log_root.path(), state_dir.path()).await;
        assert_eq!(first, DrainOutcome::Success);
        let uploaded = std::fs::read_dir(dest_dir.path().join("acme").join("widgets"))
            .expect("read the target's key prefix")
            .count();
        assert!(
            uploaded > 0,
            "run_once must have written under acme/widgets"
        );
        assert!(
            std::fs::read_dir(state_dir.path())
                .expect("read state_dir")
                .count()
                > 0,
            "the manifest cache must land under the injected state_dir, not a real home directory"
        );

        let (second, _) = tick(&cfg, None, log_root.path(), state_dir.path()).await;
        assert_eq!(
            second,
            DrainOutcome::Success,
            "an unchanged file must still report success on the next tick"
        );
    }

    /// `tick` reports `Failed`, never a panic, when the destination cannot be
    /// reached.
    #[tokio::test]
    async fn an_unreachable_destination_reports_failed() {
        let log_root = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let blocker = tempfile::NamedTempFile::new().expect("tempfile");
        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!(
                "file://{}",
                blocker.path().join("nested").display()
            )),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..LogDrainConfig::default()
        };

        let (outcome, _) = tick(&cfg, None, log_root.path(), state_dir.path()).await;
        assert_eq!(outcome, DrainOutcome::Failed);
    }

    /// A malformed config (bad destination) reports `Failed` rather than
    /// panicking the loop.
    #[tokio::test]
    async fn a_config_error_reports_failed() {
        let log_root = tempfile::tempdir().expect("tempdir");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            destination: Some("ftp://nope".to_string()),
            ..LogDrainConfig::default()
        };
        let (outcome, interval) = tick(&cfg, None, log_root.path(), state_dir.path()).await;
        assert_eq!(outcome, DrainOutcome::Failed);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }
}
