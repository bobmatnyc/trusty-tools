//! The daemon's log-drain scheduler (#6535, Phase 3 of #6533).
//!
//! Why: `trusty_common::log_drain::run_once` is one pass with no locking and no
//! memory of what it last did. A daemon that only ever called it would upload
//! once and never again. This module is the half that makes it a running
//! service: an interval loop beside `orphan_gc_loop` (private, in
//! `daemon/mod.rs`), one pass per configured project, and a persisted last-run
//! verdict the `log_drain` doctor row reads. Each pass's `<owner>/<project>`
//! comes from the resolved plan (#6657) — nothing is resolved here.
//!
//! What: [`log_drain_loop`](crate::daemon::log_drain::log_drain_loop) ticks on
//! the configured interval until its
//! [`CancellationToken`](tokio_util::sync::CancellationToken) fires;
//! [`drain_once`](crate::daemon::log_drain::drain_once) is one full pass
//! (resolve config, then connect, `run_once`, and record per project);
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
//!
//! # One pass per destination, and no fall-back between them (#6657)
//!
//! A tick runs `run_once` once per entry in
//! [`ResolvedLogDrain::destinations`](crate::core::trusty_tools_config::ResolvedLogDrain),
//! sequentially, each with its own connection and its own manifest — the cache
//! is already keyed by destination (#6548), so nothing here forks that.
//!
//! A destination that cannot be reached fails ALONE. Its sources are skipped
//! for that tick and the remaining destinations still run. There is deliberately
//! no path that retries a source against the section default: a per-source
//! destination exists precisely because those bytes belong in one specific
//! account, so shipping them to the fallback would be worse than not shipping
//! them at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use trusty_common::log_drain::{DrainConfig, DrainReport, ObjectStoreDestination, run_once};

use crate::core::trusty_tools_config::{
    LOG_DRAIN_STATE_SUBDIR, LogDrainSetting, ResolvedDrainDestination, ResolvedLogDrain,
    TrustyToolsConfig, resolve_log_drain,
};

/// Filename of the last-run record inside the drain state directory.
const STATUS_FILENAME: &str = "status.json";

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

/// How one destination's pass ended, within a tick (#6657).
///
/// Why: a tick now covers several object stores, and "the drain failed" is not
/// actionable when only one of three destinations is unreachable. The doctor
/// row lists these, so an operator sees which account stopped accepting logs.
/// What: one record per entry in `ResolvedLogDrain::destinations`, in the same
/// order.
/// Test: `tests::one_failing_destination_does_not_stop_the_others`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainDestinationStatus {
    /// The destination as the operator wrote it.
    pub destination: String,
    /// The `<owner>/<project>` this pass uploaded under (#6657).
    ///
    /// `#[serde(default)]`, so a `status.json` written before the key layout
    /// changed still decodes — it simply names no project.
    #[serde(default)]
    pub project: String,
    /// Destination scheme (`s3`, `file`).
    pub scheme: String,
    /// How this destination's pass ended.
    pub outcome: DrainOutcome,
    /// Files uploaded to this destination.
    pub uploaded: usize,
    /// Files the manifest proved this destination already had.
    pub skipped_unchanged: usize,
    /// One line an operator can act on, for this destination alone.
    pub detail: String,
}

/// The persisted result of the most recent drain pass.
///
/// Why: `tm doctor` runs daemonless (see [`super::doctor::run_doctor`]), so the
/// doctor row cannot read the scheduler's memory. A small JSON file is the only
/// channel between the two.
/// What: the aggregate outcome, when it happened, one
/// [`LogDrainDestinationStatus`] per destination the tick covered, the summed
/// counts, and a human-readable detail line. `destinations` is
/// `#[serde(default)]`, so a `status.json` written before #6657 still decodes —
/// it simply carries no per-destination breakdown and the doctor row falls back
/// to `detail`.
/// Test: `tests::status_round_trips_through_the_state_dir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LogDrainStatus {
    /// The tick's verdict: `Failed` when ANY destination failed.
    pub outcome: DrainOutcome,
    /// RFC 3339 timestamp of the pass.
    pub at: String,
    /// Per-destination outcomes, in plan order. Empty when disabled (#6657).
    #[serde(default)]
    pub destinations: Vec<LogDrainDestinationStatus>,
    /// Files uploaded this pass, across every destination.
    pub uploaded: usize,
    /// Files the manifests proved were already uploaded, across every destination.
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
            destinations: Vec::new(),
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
            destinations: Vec::new(),
            uploaded: 0,
            skipped_unchanged: 0,
            detail: format!("config error: {reason}"),
        }
    }

    /// Fold every destination's record into the tick's verdict.
    ///
    /// The tick FAILS when any destination failed — the fail-open guard is
    /// per-destination, so a partial success is still a failure to report.
    fn from_destinations(destinations: Vec<LogDrainDestinationStatus>) -> Self {
        let failed = destinations
            .iter()
            .any(|d| d.outcome == DrainOutcome::Failed);
        let uploaded = destinations.iter().map(|d| d.uploaded).sum();
        let skipped_unchanged = destinations.iter().map(|d| d.skipped_unchanged).sum();
        // With one destination the tick's detail IS that destination's, so a
        // single-destination host reads exactly as it did before #6657.
        let detail = match destinations.as_slice() {
            [] => "no destination had any source configured".to_string(),
            [only] => only.detail.clone(),
            many => many
                .iter()
                .map(|d| {
                    format!(
                        "{} → {} [{}]: {}",
                        d.scheme, d.destination, d.project, d.detail
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        };
        Self {
            outcome: if failed {
                DrainOutcome::Failed
            } else {
                DrainOutcome::Success
            },
            at: chrono::Utc::now().to_rfc3339(),
            destinations,
            uploaded,
            skipped_unchanged,
            detail,
        }
    }
}

impl LogDrainDestinationStatus {
    /// Build a failure record for one destination.
    fn failed(group: &ResolvedDrainDestination, detail: impl Into<String>) -> Self {
        Self {
            destination: group.destination_display.clone(),
            project: group.target.key_prefix(),
            scheme: group.scheme().to_string(),
            outcome: DrainOutcome::Failed,
            uploaded: 0,
            skipped_unchanged: 0,
            detail: detail.into(),
        }
    }

    /// Build the record for one destination's completed `run_once`.
    ///
    /// A report carrying per-file errors is [`DrainOutcome::Failed`] even
    /// though `run_once` returned `Ok` — see the module docs.
    fn from_report(group: &ResolvedDrainDestination, report: &DrainReport) -> Self {
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
            destination: group.destination_display.clone(),
            project: group.target.key_prefix(),
            scheme: group.scheme().to_string(),
            outcome: if failed {
                DrainOutcome::Failed
            } else {
                DrainOutcome::Success
            },
            uploaded: report.uploaded,
            skipped_unchanged: report.skipped_unchanged,
            detail,
        }
    }
}

/// The drain's state directory: `<framework root>/log-drain`.
///
/// Holds the per-destination manifest cache and `status.json`.
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

/// Run every pass in `plan`, and return the tick's verdict.
///
/// Why: separated from [`drain_once`] so the tests can drive a tick with an
/// explicit state directory — no config file and no home directory.
/// What: one [`run_destination_pass`] per entry in `plan.destinations`, in
/// order, each folded into the tick's verdict by
/// [`LogDrainStatus::from_destinations`]. Passes are sequential rather than
/// concurrent: the single-flight guarantee this module provides is "one pass at
/// a time", and running two destinations at once would double the drain's peak
/// memory for no operator-visible gain at a 15-minute cadence.
/// Test: `tests::a_successful_tick_uploads_and_records_success`,
/// `tests::a_second_tick_dedupes`, `tests::a_failing_destination_records_failed`,
/// `tests::two_destinations_each_get_their_own_pass`.
pub async fn run_tick(plan: &ResolvedLogDrain, state_dir: &Path) -> LogDrainStatus {
    let mut per_destination = Vec::with_capacity(plan.destinations.len());
    for group in &plan.destinations {
        per_destination.push(run_destination_pass(plan, group, state_dir).await);
    }
    LogDrainStatus::from_destinations(per_destination)
}

/// Drain one destination's own sources, and report only on that destination.
///
/// #6657: every failure arm ends here. Nothing retries these sources against
/// another destination — a per-source destination is a statement about which
/// account the bytes belong in, so a fallback would violate the very
/// requirement the override exists to satisfy.
async fn run_destination_pass(
    plan: &ResolvedLogDrain,
    group: &ResolvedDrainDestination,
    state_dir: &Path,
) -> LogDrainDestinationStatus {
    let dest = match ObjectStoreDestination::connect(&group.destination).await {
        Ok(dest) => dest,
        Err(e) => {
            return LogDrainDestinationStatus::failed(
                group,
                format!("cannot reach the destination: {e}"),
            );
        }
    };
    let cfg = DrainConfig::new(state_dir)
        .with_secrets(plan.secrets.clone())
        .with_max_file_bytes(plan.max_file_bytes)
        // #6547: the collector streams, so this is the bound that matters.
        .with_max_wire_bytes(plan.max_wire_bytes);
    // The manifest cache under `state_dir` is namespaced by destination
    // (#6548), so each group reads and writes its own record from one shared
    // directory.
    match run_once(&cfg, &dest, &group.target, &group.sources).await {
        Ok(report) => LogDrainDestinationStatus::from_report(group, &report),
        Err(e) => LogDrainDestinationStatus::failed(group, format!("drain run failed: {e}")),
    }
}

/// One complete tick: read config, drain every project, record.
///
/// Why: the loop body and the boot pass are the same work, and a caller that
/// re-read the config itself would let the two drift.
/// What: resolves `config` through [`resolve_log_drain`]; the loop passes a
/// freshly loaded one every tick, so a config edit takes effect without a
/// daemon restart. A config ERROR — an unresolvable project among them — and a
/// failed upload both record [`DrainOutcome::Failed`], never a silent skip. Returns
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
        Ok(LogDrainSetting::Enabled(plan)) => run_tick(&plan, &dir).await,
    };
    save_status(&dir, &status);
    status
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
