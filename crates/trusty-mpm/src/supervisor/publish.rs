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
//! [`SupervisorMetricsStatus::Unavailable`] — never as a silent zero.
//! Test: `published_metrics_round_trip`, `read_status_absent_is_unavailable`,
//! `read_status_corrupt_is_unavailable`, `read_status_old_snapshot_is_stale`,
//! `supervisor_publishes_run_stats_after_sweeps`,
//! `supervisor_metrics_merge_reports_real_run_stats` in `super::tests`.

use std::path::{Path, PathBuf};

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

/// How old a published snapshot may be before a reader reports it as stale.
///
/// Why: the supervisor republishes after every sweep, and the default cadence is
/// [`crate::supervisor::config::DEFAULT_INTERVAL_SECS`] (30s). Ten missed sweeps
/// means the supervisor is stopped, wedged, or crash-looping — the reader must
/// say so rather than presenting month-old counters as current. Ten rather than
/// one or two so a slow sweep, a `launchctl` restart, or an operator-lengthened
/// interval does not flap the status.
/// What: 300 seconds.
/// Test: `read_status_old_snapshot_is_stale`.
pub const STALE_AFTER_SECS: i64 = 300;

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
/// What: `written_at` (the publish instant) plus the full [`FleetMetrics`],
/// which carries the [`crate::supervisor::SupervisorRunStats`] counters — sweeps,
/// auto-resumes, resume failures, classifications — inside its `run_stats` field.
/// Test: `published_metrics_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedMetrics {
    /// When the supervisor wrote this snapshot.
    pub written_at: DateTime<Utc>,
    /// Fleet counts, surfaced pending decisions, and the supervisor's own
    /// cumulative `run_stats`.
    pub fleet: FleetMetrics,
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
    /// A snapshot no older than [`STALE_AFTER_SECS`].
    Current {
        /// The published snapshot.
        snapshot: Box<PublishedMetrics>,
        /// Seconds between `written_at` and the reading instant.
        age_secs: i64,
    },
    /// A snapshot older than [`STALE_AFTER_SECS`] — the supervisor is probably
    /// not running.
    Stale {
        /// The published snapshot, still the last real observation.
        snapshot: Box<PublishedMetrics>,
        /// Seconds between `written_at` and the reading instant.
        age_secs: i64,
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
/// What: creates the parent directory if absent, serialises `fleet` and `now`
/// into a [`PublishedMetrics`], and swaps it into place.
/// Test: `published_metrics_round_trip`, `supervisor_publishes_run_stats_after_sweeps`.
pub fn write_at(path: &Path, fleet: &FleetMetrics, now: DateTime<Utc>) -> Result<(), PublishError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let published = PublishedMetrics {
        written_at: now,
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

/// Publish a snapshot to the default framework root.
///
/// Why: the running supervisor wants `~/.trusty-mpm/supervisor-metrics.json`
/// without resolving the home directory itself.
/// What: resolves [`FrameworkPaths::default`] and calls [`write_at`].
/// Test: covered through [`write_at`]; the default-root resolution is
/// `metrics_path_is_under_root`.
pub fn write(fleet: &FleetMetrics, now: DateTime<Utc>) -> Result<(), PublishError> {
    write_at(&metrics_path(&FrameworkPaths::default()), fleet, now)
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
/// [`STALE_AFTER_SECS`]. A snapshot dated in the future (a clock step) has a
/// negative age and counts as current — the alternative, calling it stale,
/// would hide live counters over a clock adjustment.
/// Test: `read_status_absent_is_unavailable`, `read_status_corrupt_is_unavailable`,
/// `read_status_old_snapshot_is_stale`.
pub fn read_status_at(path: &Path, now: DateTime<Utc>) -> SupervisorMetricsStatus {
    match read_at(path) {
        Ok(Some(snapshot)) => {
            let age_secs = (now - snapshot.written_at).num_seconds();
            if age_secs > STALE_AFTER_SECS {
                SupervisorMetricsStatus::Stale {
                    snapshot: Box::new(snapshot),
                    age_secs,
                }
            } else {
                SupervisorMetricsStatus::Current {
                    snapshot: Box::new(snapshot),
                    age_secs,
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
