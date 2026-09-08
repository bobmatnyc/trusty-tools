//! The `trusty-agents` daemon's log-drain scheduler (#6537).
//!
//! Why: `trusty_common::log_drain::run_once` is one pass with no memory of
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
//! without a restart) and calls [`tick`] — resolve, connect, `run_once`, log
//! the verdict. `tick` takes the config as a parameter rather than reading it
//! itself, so tests drive it with an in-memory
//! [`LogDrainConfig`](super::LogDrainConfig) instead of the real config file.
//! Test: `tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};
use trusty_common::log_drain::{DrainConfig, ObjectStoreDestination, run_once};

use super::resolve::{DEFAULT_INTERVAL_SECS, LogDrainConfig, LogDrainSetting, resolve_log_drain};

/// What one pass did, in the three states an operator distinguishes — mirrors
/// trusty-mpm's `daemon::log_drain::DrainOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Every collected file either uploaded or was provably already there.
    Success,
    /// The drain is configured off; nothing was attempted.
    SkippedDisabled,
    /// The pass errored, or completed with at least one per-file failure.
    Failed,
}

/// Start the interval loop as a background task.
///
/// `project_root` is the current project's root (`ctrl::detect_self_project()`)
/// — `None` when this daemon serves no particular project. `log_root` is
/// `crate::service::log_dir()`.
pub fn spawn(project_root: Option<PathBuf>, log_root: PathBuf) {
    tokio::task::spawn(run_loop(project_root, log_root));
}

/// Tick forever: reload config, one pass, then sleep for whatever interval
/// that pass named (or [`DEFAULT_INTERVAL_SECS`] on a config-load error, so a
/// startup typo does not spin-loop).
async fn run_loop(project_root: Option<PathBuf>, log_root: PathBuf) {
    loop {
        let cfg = crate::mcp::config::GlobalConfig::load().await.log_drain;
        let (outcome, interval) = tick(&cfg, project_root.as_deref(), &log_root).await;
        match outcome {
            DrainOutcome::Success => info!("log_drain: pass complete"),
            DrainOutcome::SkippedDisabled => {}
            DrainOutcome::Failed => warn!("log_drain: pass failed; retrying next tick"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// One full pass over an already-loaded config: resolve, connect, `run_once`.
/// Returns the outcome and the interval the NEXT sleep should use.
async fn tick(
    cfg: &LogDrainConfig,
    project_root: Option<&Path>,
    log_root: &Path,
) -> (DrainOutcome, Duration) {
    let default_interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
    match resolve_log_drain(cfg, log_root, project_root) {
        Err(e) => {
            warn!("log_drain: {e}");
            (DrainOutcome::Failed, default_interval)
        }
        Ok(LogDrainSetting::Disabled) => (DrainOutcome::SkippedDisabled, default_interval),
        Ok(LogDrainSetting::Enabled(plan)) => {
            let interval = plan.interval;
            let dest = match ObjectStoreDestination::connect(&plan.destination).await {
                Ok(dest) => dest,
                Err(e) => {
                    warn!(
                        destination = %plan.destination_display,
                        "log_drain: cannot reach destination: {e}"
                    );
                    return (DrainOutcome::Failed, interval);
                }
            };
            let state_dir = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".trusty-agents")
                .join("log-drain");
            let drain_cfg = DrainConfig::new(state_dir)
                .with_secrets(plan.secrets.clone())
                .with_max_file_bytes(plan.max_file_bytes)
                .with_max_wire_bytes(plan.max_wire_bytes);
            let outcome = match run_once(
                &drain_cfg,
                &dest,
                &plan.target,
                std::slice::from_ref(&plan.source),
            )
            .await
            {
                Ok(report) if report.errors.is_empty() => {
                    info!(
                        uploaded = report.uploaded,
                        skipped_unchanged = report.skipped_unchanged,
                        "log_drain: pass complete"
                    );
                    DrainOutcome::Success
                }
                Ok(report) => {
                    warn!(
                        errors = report.errors.len(),
                        first = ?report.errors.first(),
                        "log_drain: pass completed with per-file errors"
                    );
                    DrainOutcome::Failed
                }
                Err(e) => {
                    warn!("log_drain: run_once failed: {e}");
                    DrainOutcome::Failed
                }
            };
            (outcome, interval)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disabled config: `tick` never touches the destination or the network.
    #[tokio::test]
    async fn opt_out_skips_the_pass() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("daemon-abc-8080.log"), "hello\n").expect("seed log");

        let (outcome, interval) = tick(&LogDrainConfig::default(), None, log_root.path()).await;
        assert_eq!(outcome, DrainOutcome::SkippedDisabled);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }

    /// An enabled plan against a `file://` destination uploads the seeded
    /// daemon log on the first tick, and a second tick — nothing changed — is
    /// idempotent: `run_once` was genuinely invoked with the CONFIGURED
    /// destination both times.
    #[tokio::test]
    async fn an_enabled_tick_uploads_and_a_second_tick_is_idempotent() {
        let log_root = tempfile::tempdir().expect("tempdir");
        std::fs::write(log_root.path().join("daemon-abc-8080.log"), "hello\n").expect("seed log");
        let dest_dir = tempfile::tempdir().expect("tempdir");

        let cfg = LogDrainConfig {
            enabled: Some(true),
            destination: Some(format!("file://{}", dest_dir.path().display())),
            owner: Some("acme".to_string()),
            project: Some("widgets".to_string()),
            ..LogDrainConfig::default()
        };

        let (first, _) = tick(&cfg, None, log_root.path()).await;
        assert_eq!(first, DrainOutcome::Success);
        let uploaded = std::fs::read_dir(dest_dir.path().join("acme").join("widgets"))
            .expect("read the target's key prefix")
            .count();
        assert!(
            uploaded > 0,
            "run_once must have written under acme/widgets"
        );

        let (second, _) = tick(&cfg, None, log_root.path()).await;
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

        let (outcome, _) = tick(&cfg, None, log_root.path()).await;
        assert_eq!(outcome, DrainOutcome::Failed);
    }

    /// A malformed config (bad destination) reports `Failed` rather than
    /// panicking the loop.
    #[tokio::test]
    async fn a_config_error_reports_failed() {
        let log_root = tempfile::tempdir().expect("tempdir");
        let cfg = LogDrainConfig {
            destination: Some("ftp://nope".to_string()),
            ..LogDrainConfig::default()
        };
        let (outcome, interval) = tick(&cfg, None, log_root.path()).await;
        assert_eq!(outcome, DrainOutcome::Failed);
        assert_eq!(interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
    }
}
