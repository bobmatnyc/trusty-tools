//! The daemon's log-drain scheduler (#6535, Phase 3 of #6533).
//!
//! Why: `trusty_common::log_drain::run_once` is one pass with no locking, no
//! identity resolution, and no memory of what it last did. A daemon that only
//! ever called it would upload once and never again, under whatever identity
//! the caller guessed. This module is the half that makes it a running service:
//! an interval loop beside `orphan_gc_loop` (private, in `daemon/mod.rs`),
//! `gh`-resolved identity, and a persisted last-run verdict the `log_drain`
//! doctor row reads.
//!
//! What: [`log_drain_loop`](crate::daemon::log_drain::log_drain_loop) ticks on
//! the configured interval until its
//! [`CancellationToken`](tokio_util::sync::CancellationToken) fires;
//! [`drain_once`](crate::daemon::log_drain::drain_once) is one full pass
//! (resolve config, resolve identity, connect, `run_once`, record);
//! [`LogDrainStatus`](crate::daemon::log_drain::LogDrainStatus) is what it
//! writes to `<state_dir>/status.json`.
//!
//! Test: `tests` submodule — every case drives a real `file://` destination in
//! a `tempfile::TempDir`, so nothing here needs S3 or the network.
//!
//! # It never reports "drained" for a run that failed
//!
//! [`DrainOutcome`](crate::daemon::log_drain::DrainOutcome) has exactly three
//! values, and the mapping is total: `run_once` returning `Err` is
//! [`DrainOutcome::Failed`](crate::daemon::log_drain::DrainOutcome::Failed), and so is an `Ok`
//! report carrying per-file errors — the core deliberately continues past an
//! unreadable file, which means an `Ok` return is NOT proof every file landed.
//! Collapsing either into "success" would let the doctor row read green while
//! nothing reached the bucket, which is the exact fail-open shape the epic's
//! design brief called out.
//!
//! # Single-flight
//!
//! The loop awaits each pass to completion before arming the next tick, so two
//! passes can never race the one manifest. `tokio::time::interval` compensates
//! for a slow pass by firing immediately rather than by overlapping.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use trusty_common::log_drain::{
    DrainConfig, DrainReport, DrainTarget, ObjectStoreDestination, run_once,
};

use crate::core::trusty_tools_config::{
    LOG_DRAIN_STATE_SUBDIR, LogDrainSetting, ResolvedLogDrain, TrustyToolsConfig, resolve_log_drain,
};

/// Filename of the last-run record inside the drain state directory.
const STATUS_FILENAME: &str = "status.json";

/// Filename of the persisted per-install session id.
const SESSION_ID_FILENAME: &str = "session-id";

/// How long the `gh api user` identity probe may take before the pass gives up.
///
/// A pass that hangs on `gh` holds the single-flight slot and delays every
/// later tick, so the probe is bounded rather than left to `gh`'s own defaults.
const GH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What one drain pass did, in the three states an operator distinguishes.
///
/// Why: "nothing uploaded" is ambiguous on its own — it is equally the shape of
/// a healthy idle run, a disabled drain, and a total failure. Naming the three
/// separately is what lets the doctor row be honest. See the module docs.
/// What: a serde enum persisted in `status.json`.
/// Test: `tests::a_failing_destination_records_failed`,
/// `tests::a_successful_tick_uploads_and_records_success`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainOutcome {
    /// Every collected file either uploaded or was provably already there.
    Success,
    /// The drain is configured off; nothing was attempted.
    SkippedDisabled,
    /// The pass errored, or completed with at least one per-file failure.
    Failed,
}

/// The persisted result of the most recent drain pass.
///
/// Why: `tm doctor` runs daemonless (see [`super::doctor::run_doctor`]), so the
/// doctor row cannot read the scheduler's memory. A small JSON file is the only
/// channel between the two.
/// What: the outcome, when it happened, the destination it was aimed at, the
/// upload counts, and a human-readable detail line.
/// Test: `tests::status_round_trips_through_the_state_dir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainStatus {
    /// Which of the three states this pass ended in.
    pub outcome: DrainOutcome,
    /// RFC 3339 timestamp of the pass.
    pub at: String,
    /// The destination as the operator wrote it, or `null` when disabled.
    pub destination: Option<String>,
    /// Destination scheme (`s3`, `file`), or `null` when disabled.
    pub scheme: Option<String>,
    /// Files uploaded this pass.
    pub uploaded: usize,
    /// Files the manifest proved were already uploaded.
    pub skipped_unchanged: usize,
    /// One line an operator can act on.
    pub detail: String,
}

impl LogDrainStatus {
    /// Build the disabled record.
    fn disabled(detail: impl Into<String>) -> Self {
        Self {
            outcome: DrainOutcome::SkippedDisabled,
            at: chrono::Utc::now().to_rfc3339(),
            destination: None,
            scheme: None,
            uploaded: 0,
            skipped_unchanged: 0,
            detail: detail.into(),
        }
    }

    /// Build a failure record for a config that would not resolve.
    ///
    /// There is no plan to name a destination from, which is exactly why the
    /// record has to exist: without it a refused startup and a host that never
    /// configured a drain would look identical to `tm doctor`.
    fn config_error(reason: &str) -> Self {
        Self {
            outcome: DrainOutcome::Failed,
            at: chrono::Utc::now().to_rfc3339(),
            destination: None,
            scheme: None,
            uploaded: 0,
            skipped_unchanged: 0,
            detail: format!("config error: {reason}"),
        }
    }

    /// Build a failure record for `plan`.
    fn failed(plan: &ResolvedLogDrain, detail: impl Into<String>) -> Self {
        Self {
            outcome: DrainOutcome::Failed,
            at: chrono::Utc::now().to_rfc3339(),
            destination: Some(plan.destination_display.clone()),
            scheme: Some(plan.scheme().to_string()),
            uploaded: 0,
            skipped_unchanged: 0,
            detail: detail.into(),
        }
    }

    /// Build the record for a completed `run_once`.
    ///
    /// A report carrying per-file errors is [`DrainOutcome::Failed`] even
    /// though `run_once` returned `Ok` — see the module docs.
    fn from_report(plan: &ResolvedLogDrain, report: &DrainReport) -> Self {
        let failed = !report.errors.is_empty();
        let detail = if failed {
            let (key, message) = &report.errors[0];
            format!(
                "{} file(s) uploaded, {} file error(s) — first: {key}: {message}",
                report.uploaded,
                report.errors.len()
            )
        } else {
            // #6547: the second number is what says whether the backlog is
            // settled. A pass that recorded no new decision logged no warning.
            format!(
                "{} file(s) uploaded ({} B), {} unchanged, {} over the size ceiling \
                 ({} newly recorded)",
                report.uploaded,
                report.bytes_plain,
                report.skipped_unchanged,
                report.skipped_too_large,
                report.skips_recorded
            )
        };
        Self {
            outcome: if failed {
                DrainOutcome::Failed
            } else {
                DrainOutcome::Success
            },
            at: chrono::Utc::now().to_rfc3339(),
            destination: Some(plan.destination_display.clone()),
            scheme: Some(plan.scheme().to_string()),
            uploaded: report.uploaded,
            skipped_unchanged: report.skipped_unchanged,
            detail,
        }
    }
}

/// The drain's state directory: `<framework root>/log-drain`.
///
/// Holds the manifest cache, the persisted session id, and `status.json`.
pub fn state_dir(framework_root: &Path) -> PathBuf {
    framework_root.join(LOG_DRAIN_STATE_SUBDIR)
}

/// Path of the last-run record inside `state_dir`.
pub fn status_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATUS_FILENAME)
}

/// Read the last recorded pass, or `None` when nothing has run.
///
/// An unreadable or undecodable file reads as `None`: the doctor row then says
/// "no run recorded", which is the truthful answer either way.
/// Test: `tests::status_round_trips_through_the_state_dir`.
pub fn load_status(state_dir: &Path) -> Option<LogDrainStatus> {
    let raw = std::fs::read_to_string(status_path(state_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist `status`, creating the state directory if needed.
///
/// Best-effort: a failure to record is warned about, never propagated. Losing
/// the record must not fail a pass that actually uploaded.
/// Test: `tests::status_round_trips_through_the_state_dir`.
pub fn save_status(state_dir: &Path, status: &LogDrainStatus) {
    if let Err(e) = std::fs::create_dir_all(state_dir) {
        warn!("log_drain: cannot create {}: {e}", state_dir.display());
        return;
    }
    match serde_json::to_string_pretty(status) {
        Ok(json) => {
            if let Err(e) = std::fs::write(status_path(state_dir), json) {
                warn!("log_drain: cannot record status: {e}");
            }
        }
        Err(e) => warn!("log_drain: cannot serialise status: {e}"),
    }
}

/// Resolve the session segment of the key layout.
///
/// Why: the epic's layout is `<github-id>/<session>/logs/…`, but the trusty-mpm
/// DAEMON has no single "current session" — it supervises many, and the files
/// it drains (`~/.trusty-mpm/logs/trusty-mpm.log.*`) belong to the daemon
/// itself rather than to any one of them. A per-BOOT id would be worse than
/// useless: the manifest is keyed by target, so every restart would re-upload
/// every log file under a fresh prefix. So the daemon's session is
/// per-INSTALL — one id, minted on first run and persisted beside the manifest
/// cache. Phase 5 consumers (#6537) pass their own real session ids.
/// What: the configured `session_id` when set, else the persisted file, else a
/// freshly-minted UUID written to that file.
/// Test: `tests::session_id_is_stable_across_calls`.
///
/// # Errors
/// A message naming the path when the id can neither be read nor written.
pub fn resolve_session_id(plan: &ResolvedLogDrain, state_dir: &Path) -> Result<String, String> {
    if let Some(configured) = plan.session_id.as_deref() {
        return Ok(configured.to_string());
    }
    let path = state_dir.join(SESSION_ID_FILENAME);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let minted = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(state_dir)
        .and_then(|()| std::fs::write(&path, &minted))
        .map_err(|e| {
            format!(
                "cannot persist the drain session id at {}: {e}",
                path.display()
            )
        })?;
    Ok(minted)
}

/// Resolve the GitHub login the logs are filed under.
///
/// Why: an unattributed upload is worse than no upload — it mixes one
/// operator's logs into a shared prefix nobody can reason about. The core
/// refuses an empty id outright, so this has to produce a real one or fail.
/// What: the configured `github_id` when set, else one `gh api user --jq
/// .login` through `trusty_common::gh` — the workspace's single `gh` entry
/// point (#5475), never a fresh `Command::new("gh")`. Bounded by
/// [`GH_PROBE_TIMEOUT`]. The caller caches the answer across ticks.
/// Test: covered indirectly — the tick tests supply an explicit id, because a
/// test that shelled out to the developer's real `gh` would assert on their
/// GitHub account.
///
/// # Errors
/// A message naming what `gh` did, for the `warn!` and the status detail.
pub async fn resolve_github_id(plan: &ResolvedLogDrain) -> Result<String, String> {
    if let Some(configured) = plan.github_id.as_deref() {
        return Ok(configured.to_string());
    }
    // #6535: single `gh` entry point. `nonempty_stdout` folds a non-zero exit
    // and a blank login into the same `Err`, so no empty id can escape here.
    let command = trusty_common::gh::GhCommand::new(["api", "user", "--jq", ".login"]);
    match tokio::time::timeout(GH_PROBE_TIMEOUT, command.nonempty_stdout()).await {
        Ok(Ok(login)) => Ok(login.trim().to_string()),
        Ok(Err(e)) => Err(format!("`gh api user --jq .login` failed: {e}")),
        Err(_) => Err(format!(
            "`gh api user --jq .login` did not respond within {GH_PROBE_TIMEOUT:?}"
        )),
    }
}

/// Run one full drain pass against `plan` for `target`, and return its verdict.
///
/// Why: separated from [`drain_once`] so the tests can drive a pass with an
/// explicit identity and an explicit state directory — no config file, no `gh`,
/// no home directory.
/// What: connects the destination, calls `run_once`, and maps the outcome
/// through [`LogDrainStatus`]. Every failure arm is [`DrainOutcome::Failed`];
/// none can produce a success record.
/// Test: `tests::a_successful_tick_uploads_and_records_success`,
/// `tests::a_second_tick_dedupes`, `tests::a_failing_destination_records_failed`.
pub async fn run_tick(
    plan: &ResolvedLogDrain,
    state_dir: &Path,
    target: &DrainTarget,
) -> LogDrainStatus {
    let dest = match ObjectStoreDestination::connect(&plan.destination).await {
        Ok(dest) => dest,
        Err(e) => {
            return LogDrainStatus::failed(plan, format!("cannot reach the destination: {e}"));
        }
    };
    let cfg = DrainConfig::new(state_dir)
        .with_secrets(plan.secrets.clone())
        .with_max_file_bytes(plan.max_file_bytes)
        // #6547: the collector streams, so this is the bound that matters.
        .with_max_wire_bytes(plan.max_wire_bytes);
    match run_once(&cfg, &dest, target, &plan.sources).await {
        Ok(report) => LogDrainStatus::from_report(plan, &report),
        Err(e) => LogDrainStatus::failed(plan, format!("drain run failed: {e}")),
    }
}

/// One complete pass: read config, resolve identity, drain, record.
///
/// Why: the loop body and the boot pass are the same work, and a caller that
/// re-read the config itself would let the two drift.
/// What: resolves `config` through [`resolve_log_drain`]; the loop passes a
/// freshly loaded one every tick, so a config edit takes effect without a
/// daemon restart. A config ERROR, an unresolvable identity, and a failed
/// upload all record [`DrainOutcome::Failed`] — never a silent skip. Returns
/// the status it wrote. `config` is a parameter rather than loaded here so the
/// tests never read (or need) the developer's real config file.
/// Test: `tests::a_config_error_records_failed`,
/// `tests::a_disabled_config_records_skipped`.
pub async fn drain_once(
    config: &TrustyToolsConfig,
    framework_root: &Path,
    home: &Path,
) -> LogDrainStatus {
    let dir = state_dir(framework_root);
    let status = match resolve_log_drain(config, home) {
        // A malformed section is a hard error, so it reports FAILED rather than
        // reading like a drain nobody turned on.
        Err(e) => LogDrainStatus::config_error(&e.to_string()),
        Ok(LogDrainSetting::Disabled) => {
            LogDrainStatus::disabled("log_drain is disabled in config")
        }
        Ok(LogDrainSetting::Enabled(plan)) => match identity(&plan, &dir).await {
            Ok(target) => run_tick(&plan, &dir, &target).await,
            Err(detail) => LogDrainStatus::failed(&plan, detail),
        },
    };
    save_status(&dir, &status);
    status
}

/// Resolve both identity components, or say which one could not be resolved.
async fn identity(plan: &ResolvedLogDrain, state_dir: &Path) -> Result<DrainTarget, String> {
    let github_id = resolve_github_id(plan).await?;
    let session_id = resolve_session_id(plan, state_dir)?;
    Ok(DrainTarget {
        github_id,
        session_id,
    })
}

/// Drain on the configured interval until `cancel` fires.
///
/// Why: the epic's whole request is "periodically". This is that loop, modelled
/// on [`super::orphan_gc_loop`] — `tokio::time::interval` plus a
/// `CancellationToken` so a SIGTERM exits between passes rather than abandoning
/// one mid-upload.
/// What: runs one pass immediately (`tokio::time::interval`'s first `tick()`
/// completes at once, matching [`super::orphan_gc_loop`]'s boot sweep), then on
/// the interval. Each pass is awaited to completion before the next is armed,
/// which IS the single-flight guarantee `run_once` does not provide. A failed
/// pass is logged at `warn!` and retried on the next tick; nothing here can
/// panic the daemon. The interval is re-read after every pass, so an operator's
/// edit takes effect without a restart, at the cost of one tick at the old
/// cadence. A drain switched OFF mid-run keeps ticking rather than exiting, so
/// switching it back on needs no restart either.
/// Test: `tests::the_loop_exits_on_cancel`.
pub async fn log_drain_loop(framework_root: PathBuf, home: PathBuf, cancel: CancellationToken) {
    let mut interval_secs = current_interval_secs(&home);
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        // Checked BEFORE the select rather than only inside it: the first
        // `tick()` is ready immediately, so a `select!` between two ready
        // branches would start a pass on an already-cancelled loop about half
        // the time — a drain running after shutdown began.
        if cancel.is_cancelled() {
            info!("log_drain_loop: already cancelled; exiting without a pass");
            return;
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("log_drain_loop: cancel signal received; exiting");
                return;
            }
            _ = tick.tick() => {
                let status = drain_once(&TrustyToolsConfig::load(), &framework_root, &home).await;
                match status.outcome {
                    DrainOutcome::Success | DrainOutcome::SkippedDisabled => {
                        info!("log_drain: {}", status.detail);
                    }
                    // Retried on the next tick. Never fatal to the daemon.
                    DrainOutcome::Failed => warn!("log_drain: {}", status.detail),
                }
                let next = current_interval_secs(&home);
                if next != interval_secs {
                    interval_secs = next;
                    tick = tokio::time::interval(Duration::from_secs(interval_secs));
                    // Consume the new interval's immediate first tick, so a
                    // config edit does not trigger an extra out-of-band pass.
                    tick.tick().await;
                }
            }
        }
    }
}

/// The interval the next pass should use, falling back to the default when the
/// config no longer resolves (the pass itself reports that error).
fn current_interval_secs(home: &Path) -> u64 {
    match resolve_log_drain(&TrustyToolsConfig::load(), home) {
        Ok(LogDrainSetting::Enabled(plan)) => plan.interval.as_secs(),
        _ => crate::core::trusty_tools_config::LOG_DRAIN_DEFAULT_INTERVAL_SECS,
    }
}

/// Whether the daemon should spawn the loop at all, and why not when it should
/// not.
///
/// Why: spawning a task that immediately discovers it is disabled wastes a
/// wakeup on every host that never configures the drain — which is all of them
/// by default. Deciding at startup keeps the default-off path free.
/// What: `Ok(true)` for a validated enabled plan, `Ok(false)` for disabled, and
/// `Err` for a config error the caller logs and records.
/// Test: `tests::should_spawn_matches_the_setting`.
///
/// The record the daemon persists when startup refuses a malformed section.
///
/// Why: `should_spawn` returning `Err` means no pass will ever run, so nothing
/// else would write a status file and the doctor row would report "no run
/// recorded" — a warning, when the truth is a failure.
/// Test: `tests::a_config_error_records_failed` covers the same constructor via
/// [`drain_once`].
pub fn config_error_status(reason: &str) -> LogDrainStatus {
    LogDrainStatus::config_error(reason)
}

/// # Errors
/// The rendered [`crate::core::trusty_tools_config::LogDrainConfigError`].
pub fn should_spawn(config: &TrustyToolsConfig, home: &Path) -> Result<bool, String> {
    match resolve_log_drain(config, home) {
        Ok(LogDrainSetting::Enabled(_)) => Ok(true),
        Ok(LogDrainSetting::Disabled) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
#[path = "log_drain_tests.rs"]
mod tests;
