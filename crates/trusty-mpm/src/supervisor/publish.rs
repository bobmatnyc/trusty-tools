//! Cross-process metrics publication: the supervisor writes a file, the daemon reads it.
//!
//! Why (#6288): the supervisor used to publish its snapshot on a second HTTP
//! listener (`127.0.0.1:7881`). Nothing in the workspace read it — the daemon's
//! `console_metrics` / `supervisor_status` rebuilt `FleetMetrics` from the
//! session store and left `run_stats` at its default, so both tools reported
//! zero sweeps and zero auto-resumes no matter how long the supervisor had been
//! running. ADR-0032 makes UDS (not a TCP port) the inter-service transport, and
//! a whole socket for one JSON blob a reader polls is more machinery than the
//! problem needs. A file under the framework root is the transport already used
//! for supervisor↔daemon state: `crate::core::auto_resume` passes the console's
//! desired flag the other way through `~/.trusty-mpm/auto_resume`, and the
//! supervisor re-reads it every sweep.
//! What: [`metrics_path`] names `<framework root>/supervisor-metrics.json`;
//! [`write_at`] publishes a [`PublishedMetrics`] there atomically (temp file +
//! rename, through the crate's shared [`atomic_write`] entry point) so a reader
//! never observes a half-written file; [`read_status_at`] classifies what a
//! reader finds as [`SupervisorMetricsStatus::Current`],
//! [`SupervisorMetricsStatus::Stale`], or
//! [`SupervisorMetricsStatus::Unavailable`] — never as a silent zero. Staleness
//! is judged against the cadence the writing supervisor actually runs at
//! ([`stale_after_secs`]), not a fixed wall-clock constant.
//! Test: `published_metrics_round_trip`, `read_status_absent_is_unavailable`,
//! `read_status_corrupt_is_unavailable`, `read_status_old_snapshot_is_stale`,
//! `supervisor_publishes_run_stats_after_sweeps`,
//! `supervisor_metrics_merge_reports_real_run_stats`,
//! `stale_threshold_tracks_the_configured_interval`,
//! `read_status_respects_a_slow_configured_interval`,
//! `snapshot_without_interval_falls_back_to_the_default_cadence` in
//! `super::tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::agent_manifest::atomic_write;
use crate::core::paths::FrameworkPaths;
use crate::supervisor::metrics::FleetMetrics;

/// Filename (under the framework root) holding the published snapshot.
///
/// Why: one named constant keeps the writing supervisor and the reading daemon
/// from drifting apart, the same way [`crate::core::auto_resume::AUTO_RESUME_FILE`]
/// does for the flag travelling the other way.
/// What: `supervisor-metrics.json`.
/// Test: `metrics_path_is_under_root`.
pub const SUPERVISOR_METRICS_FILE: &str = "supervisor-metrics.json";

/// How many missed publishes make a snapshot stale.
///
/// Why: staleness has to be measured in SWEEPS, not wall-clock seconds. The
/// cadence is operator-tunable and unbounded — `config.rs` documents slow
/// overnight cadences as a supported case — so a fixed threshold marks a
/// perfectly healthy supervisor stale for part of every cycle once the interval
/// exceeds it, and permanently once it exceeds it twice over. Three because one
/// missed publish is an ordinary slow sweep (the sweep lists the fleet and may
/// call an LLM to classify panes), two is unusual, and three consecutive misses
/// is a stopped, wedged, or crash-looping process rather than a slow one.
/// What: the multiplier applied to the writer's configured interval by
/// [`stale_after_secs`].
/// Test: `stale_threshold_tracks_the_configured_interval`,
/// `read_status_respects_a_slow_configured_interval`.
pub const STALE_AFTER_SWEEPS: i64 = 3;

/// Lower bound on the staleness threshold, whatever the interval.
///
/// Why: a fast cadence must not make the status flap. At the 30s default,
/// three sweeps is 90 seconds — close enough to ordinary scheduling jitter, a
/// `launchctl` restart, or one slow LLM-backed sweep that a reader would see
/// `stale` flicker on a healthy supervisor. The floor also keeps the default
/// install's threshold exactly where it was before the interval became part of
/// the calculation.
/// What: 300 seconds — ten sweeps at the 30s default
/// ([`crate::supervisor::config::DEFAULT_INTERVAL_SECS`]).
/// Test: `stale_threshold_tracks_the_configured_interval`,
/// `read_status_old_snapshot_is_stale`.
pub const STALE_FLOOR_SECS: i64 = 300;

/// The age at which a snapshot written by a supervisor on `interval` is stale.
///
/// Why: see [`STALE_AFTER_SWEEPS`] — the threshold has to scale with the
/// writer's cadence or a slow overnight supervisor reads as dead.
/// What: `max(STALE_AFTER_SWEEPS * interval, STALE_FLOOR_SECS)`, saturating so a
/// nonsensical interval cannot overflow the multiplication.
/// Test: `stale_threshold_tracks_the_configured_interval`.
pub fn stale_after_secs(interval: Duration) -> i64 {
    let secs = i64::try_from(interval.as_secs()).unwrap_or(i64::MAX);
    secs.saturating_mul(STALE_AFTER_SWEEPS)
        .max(STALE_FLOOR_SECS)
}

/// Errors publishing or reading the supervisor's metrics snapshot.
///
/// Why: the library-crate rule — a structured error, not `anyhow`, so the daemon
/// can render the failure to an operator and the supervisor can log it without
/// either side parsing a string.
/// What: an I/O failure, or a snapshot file that is not valid JSON of the
/// expected shape.
/// Test: `read_status_corrupt_is_unavailable`.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// Reading or writing the snapshot file failed.
    #[error("supervisor metrics io error: {0}")]
    Io(#[from] std::io::Error),
    /// The snapshot file exists but is not a valid [`PublishedMetrics`].
    #[error("supervisor metrics json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// The snapshot the supervisor publishes after every sweep.
///
/// Why: a reader needs both the fleet state the supervisor observed AND when it
/// observed it. Without the timestamp there is no way to tell a live supervisor
/// from a stopped one whose last file is still on disk, which is how a stale
/// counter gets presented as current.
/// What: `written_at` (the publish instant), `interval_secs` (the cadence the
/// writer is configured at, so the reader can size its staleness window), and
/// the full [`FleetMetrics`], which carries the
/// [`crate::supervisor::SupervisorRunStats`] counters — sweeps, auto-resumes,
/// resume failures, classifications — inside its `run_stats` field.
/// Test: `published_metrics_round_trip`,
/// `snapshot_without_interval_falls_back_to_the_default_cadence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedMetrics {
    /// When the supervisor wrote this snapshot.
    pub written_at: DateTime<Utc>,
    /// The sweep cadence the writing supervisor is configured at.
    ///
    /// A snapshot written by a binary that predates this field deserialises to
    /// [`crate::supervisor::config::DEFAULT_INTERVAL_SECS`], which puts the
    /// threshold on [`STALE_FLOOR_SECS`] — the behaviour that field replaced.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// Fleet counts, surfaced pending decisions, and the supervisor's own
    /// cumulative `run_stats`.
    pub fleet: FleetMetrics,
}

/// Serde fallback for a snapshot written before `interval_secs` existed.
fn default_interval_secs() -> u64 {
    crate::supervisor::config::DEFAULT_INTERVAL_SECS
}

/// What a reader found at the snapshot path.
///
/// Why (#6288 Fail-Open Check): the defect this module replaces was a SILENT
/// zero — `run_stats` defaulted and nothing said why. Every read outcome must
/// therefore be nameable on the wire: fresh counters, last-known counters that
/// are too old to trust, or no counters at all with the reason attached. There
/// is deliberately no variant that means "assume zero and say nothing".
/// What: `Current` and `Stale` both carry the snapshot (a stale counter is still
/// the last real observation, and hiding it would re-create the silent zero);
/// `Unavailable` carries the operator-facing reason.
/// Test: `read_status_absent_is_unavailable`, `read_status_corrupt_is_unavailable`,
/// `read_status_old_snapshot_is_stale`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorMetricsStatus {
    /// A snapshot no older than its own [`stale_after_secs`] window.
    Current {
        /// The published snapshot.
        snapshot: Box<PublishedMetrics>,
        /// Seconds between `written_at` and the reading instant.
        age_secs: i64,
        /// The window this snapshot was judged against, from its own cadence.
        stale_after_secs: i64,
    },
    /// A snapshot past its [`stale_after_secs`] window — the supervisor is
    /// probably not running.
    Stale {
        /// The published snapshot, still the last real observation.
        snapshot: Box<PublishedMetrics>,
        /// Seconds between `written_at` and the reading instant.
        age_secs: i64,
        /// The window this snapshot was judged against, from its own cadence.
        stale_after_secs: i64,
    },
    /// No usable snapshot: the file is absent, unreadable, or corrupt.
    Unavailable {
        /// Operator-facing explanation, surfaced verbatim by the daemon.
        reason: String,
    },
}

/// Resolve the path of the published snapshot file.
///
/// Why: centralising the path keeps the supervisor and the daemon consistent and
/// lets tests point at a temp root instead of the developer's real `~/.trusty-mpm`.
/// What: `<root>/supervisor-metrics.json` derived from the given [`FrameworkPaths`].
/// Test: `metrics_path_is_under_root`.
pub fn metrics_path(paths: &FrameworkPaths) -> PathBuf {
    paths.root.join(SUPERVISOR_METRICS_FILE)
}

/// Publish a snapshot to an explicit path, atomically.
///
/// Why: the daemon may read at any instant, including mid-write. A plain
/// `fs::write` truncates first, so a reader can observe an empty or truncated
/// file and report `Unavailable` for a supervisor that is working fine. Routing
/// through the crate's shared [`atomic_write`] (temp file + rename) makes the
/// publish a single rename the reader either sees or does not.
/// What: creates the parent directory if absent, serialises `fleet`, `now`, and
/// the writer's `interval` into a [`PublishedMetrics`], and swaps it into place.
/// The interval travels with the snapshot because the reader cannot otherwise
/// know how long a gap between publishes is normal for this supervisor.
/// Test: `published_metrics_round_trip`, `supervisor_publishes_run_stats_after_sweeps`.
pub fn write_at(
    path: &Path,
    fleet: &FleetMetrics,
    interval: Duration,
    now: DateTime<Utc>,
) -> Result<(), PublishError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let published = PublishedMetrics {
        written_at: now,
        interval_secs: interval.as_secs(),
        fleet: fleet.clone(),
    };
    let text = serde_json::to_string_pretty(&published)?;
    // #6288: the crate's one temp-then-rename entry point, so a fix to the swap
    // lands once (CLAUDE.md "Common entry point").
    atomic_write(path, &text).map_err(|e| match e {
        crate::core::agent_manifest::ManifestError::Io(io) => PublishError::Io(io),
        crate::core::agent_manifest::ManifestError::Json(j) => PublishError::Json(j),
    })
}

/// Read a published snapshot from an explicit path.
///
/// Why: separating the path-taking core from the home-resolving wrapper keeps
/// the file logic hermetically testable.
/// What: `Ok(None)` when the file does not exist; `Ok(Some(_))` when it parses;
/// `Err` for every other I/O failure and for a file that is not a valid
/// [`PublishedMetrics`] — a corrupt file is never flattened into "absent".
/// Test: `published_metrics_round_trip`, `read_status_corrupt_is_unavailable`.
pub fn read_at(path: &Path) -> Result<Option<PublishedMetrics>, PublishError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PublishError::Io(e)),
    }
}

/// Classify what a reader finds at `path` as of `now`.
///
/// Why: this is the whole fail-open contract in one function. Every caller gets
/// a variant it must name on the wire, so there is no code path where a missing
/// or ancient snapshot silently becomes zeroed counters.
/// What: absent / unreadable / unparseable → `Unavailable` with the reason;
/// otherwise `Current` or `Stale` by comparing the snapshot age against
/// [`stale_after_secs`] computed from the snapshot's OWN `interval_secs`, so a
/// supervisor on a 15-minute overnight cadence is not called dead 5 minutes into
/// every cycle. A snapshot dated in the future (a clock step) has a negative age
/// and counts as current — the alternative, calling it stale, would hide live
/// counters over a clock adjustment.
/// Test: `read_status_absent_is_unavailable`, `read_status_corrupt_is_unavailable`,
/// `read_status_old_snapshot_is_stale`,
/// `read_status_respects_a_slow_configured_interval`.
pub fn read_status_at(path: &Path, now: DateTime<Utc>) -> SupervisorMetricsStatus {
    match read_at(path) {
        Ok(Some(snapshot)) => {
            let age_secs = (now - snapshot.written_at).num_seconds();
            let stale_after_secs = stale_after_secs(Duration::from_secs(snapshot.interval_secs));
            if age_secs > stale_after_secs {
                SupervisorMetricsStatus::Stale {
                    snapshot: Box::new(snapshot),
                    age_secs,
                    stale_after_secs,
                }
            } else {
                SupervisorMetricsStatus::Current {
                    snapshot: Box::new(snapshot),
                    age_secs,
                    stale_after_secs,
                }
            }
        }
        Ok(None) => SupervisorMetricsStatus::Unavailable {
            reason: format!(
                "no snapshot at {}; the supervisor has not published one \
                 (is `tm supervisor` running?)",
                path.display()
            ),
        },
        Err(e) => SupervisorMetricsStatus::Unavailable {
            reason: format!("cannot read {}: {e}", path.display()),
        },
    }
}
