//! Oldest-pending-age as a red health state on `/api/console/metrics/*`.
//!
//! Why: ADR-0034 §2 — "A pending entry older than a threshold is a **red**
//! health state … not a `warn!` line in a log nobody reads." The trap this
//! module is shaped to avoid is one level down from that: a threshold evaluated
//! only inside a background poll loop is fail-quiet again, because a loop that
//! silently stops leaves the last cached value looking healthy forever. So the
//! scan runs **on the request**, against the directory, every time — there is
//! no cache to go stale, and a scan that fails is itself red rather than empty.
//!
//! What: [`scan_health`] reads the spool, computes pending count, oldest age,
//! and total failed attempts, and classifies. [`to_report`] wraps the result in
//! the workspace-standard `ConsoleMetricsReport` so the existing console
//! dashboard renders it with no new contract.
//!
//! Test: `webhook/tests.rs` — `health_*` cases cover empty, young-pending,
//! aged-pending, undecodable-entry, and unreadable-spool.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth, make_report};

use super::spool::Spool;

/// Age at which a still-pending delivery turns the health state red.
///
/// Ten minutes is longer than any legitimate relay path — a cold
/// `trusty-review` start is 191 ms (#5028) — and short enough that an operator
/// sees a stuck delivery inside one working session.
pub const DEFAULT_RED_AFTER: Duration = Duration::from_secs(600);

/// Bumped when [`SpoolHealth`]'s shape changes, per the `console_metrics`
/// contract.
pub const METRICS_SCHEMA_VERSION: u32 = 1;

/// Service id this report is published under.
pub const WEBHOOK_SERVICE_ID: &str = "trusty-console-webhooks";

/// What one on-demand scan of the spool found.
///
/// Why: an operator diagnosing a stuck webhook needs the age, the count, and
/// the failing delivery's id and last error, all in the one payload the
/// dashboard already fetches.
/// What: serialised as the `metrics` object of a `ConsoleMetricsReport`.
/// `scan_error` being `Some` is always accompanied by `status: Error`.
/// Test: every `health_*` case in `webhook/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolHealth {
    /// Coarse classification; see [`scan_health`] for the rules.
    pub status: ServiceHealth,
    /// How many decodable entries are still pending.
    pub pending: usize,
    /// Age of the oldest pending entry, in seconds.
    pub oldest_pending_age_secs: Option<u64>,
    /// Delivery id of the oldest pending entry.
    pub oldest_pending_delivery_id: Option<String>,
    /// Why the oldest pending entry's last relay attempt failed.
    pub oldest_pending_last_error: Option<String>,
    /// Sum of failed attempts across every pending entry.
    pub total_failed_attempts: u64,
    /// Threshold in force for this scan.
    pub red_after_secs: u64,
    /// Entries present on disk that could not be read or decoded, with reasons.
    /// Never empty while `status` is `Ok`.
    pub undecodable: Vec<String>,
    /// Why the scan itself failed, if it did.
    pub scan_error: Option<String>,
}

/// Scan the spool and classify its health, right now.
///
/// Why: called per request rather than per poll tick. The whole point of the
/// signal is to catch a path that has quietly stopped working, and a signal
/// that itself depends on a background task still running does not do that.
///
/// What: classification, in order —
/// 1. the scan failed → `Error`, with `scan_error` set;
/// 2. any entry on disk could not be decoded → `Error` (an unreadable pending
///    delivery is not a healthy one);
/// 3. the oldest pending entry is at least `red_after` old → `Error`;
/// 4. anything is pending → `Degraded`;
/// 5. otherwise → `Ok`.
///
/// `now_unix_ms` is a parameter so the age rules are testable without sleeping.
/// An entry whose timestamp is in the future yields age `0` rather than
/// underflowing.
///
/// Test: `health_reports_ok_on_an_empty_spool`,
/// `health_reports_degraded_for_a_young_pending_entry`,
/// `health_reports_error_once_the_oldest_entry_passes_the_threshold`,
/// `health_reports_error_for_an_undecodable_entry`,
/// `health_reports_error_when_the_spool_cannot_be_read`.
pub fn scan_health(spool: &Spool, now_unix_ms: u64, red_after: Duration) -> SpoolHealth {
    let red_after_secs = red_after.as_secs();

    let listing = match spool.list_pending() {
        Ok(listing) => listing,
        Err(e) => {
            return SpoolHealth {
                status: ServiceHealth::Error,
                pending: 0,
                oldest_pending_age_secs: None,
                oldest_pending_delivery_id: None,
                oldest_pending_last_error: None,
                total_failed_attempts: 0,
                red_after_secs,
                undecodable: Vec::new(),
                // A spool that cannot be read is NOT an empty spool. Reporting
                // it healthy is the fail-quiet shape this module exists to
                // remove.
                scan_error: Some(format!("{e}")),
            };
        }
    };

    let oldest = listing.pending.first();
    let oldest_age_secs = oldest.map(|p| {
        now_unix_ms
            .saturating_sub(p.entry.received_at_unix_ms)
            .div_euclid(1000)
    });
    let total_failed_attempts: u64 = listing
        .pending
        .iter()
        .map(|p| u64::from(p.entry.attempts))
        .sum();
    let undecodable: Vec<String> = listing
        .undecodable
        .iter()
        .map(|(path, reason)| format!("{}: {reason}", path.display()))
        .collect();

    let aged_out = oldest_age_secs.is_some_and(|age| age >= red_after_secs);
    let status = if !undecodable.is_empty() || aged_out {
        ServiceHealth::Error
    } else if listing.pending.is_empty() {
        ServiceHealth::Ok
    } else {
        ServiceHealth::Degraded
    };

    SpoolHealth {
        status,
        pending: listing.pending.len(),
        oldest_pending_age_secs: oldest_age_secs,
        oldest_pending_delivery_id: oldest.map(|p| p.entry.delivery_id.clone()),
        oldest_pending_last_error: oldest.and_then(|p| p.entry.last_error.clone()),
        total_failed_attempts,
        red_after_secs,
        undecodable,
        scan_error: None,
    }
}

/// Wrap a [`SpoolHealth`] in the standard console metrics envelope.
///
/// Why: the console dashboard already knows how to render a
/// `ConsoleMetricsReport`, so the webhook signal needs no new client contract.
/// What: `service_id` [`WEBHOOK_SERVICE_ID`], `status` mirrored from the scan,
/// `metrics` the serialised [`SpoolHealth`]. Serialisation cannot realistically
/// fail for this type; if it somehow did, the fallback payload keeps the red
/// status rather than dropping the report.
/// Test: `metrics_route_reports_red_for_an_aged_pending_entry`.
pub fn to_report(health: &SpoolHealth) -> ConsoleMetricsReport {
    let metrics = serde_json::to_value(health).unwrap_or_else(|e| {
        serde_json::json!({ "status": "error", "scan_error": format!("serialize health: {e}") })
    });
    make_report(
        WEBHOOK_SERVICE_ID,
        "Webhooks",
        env!("CARGO_PKG_VERSION"),
        health.status.clone(),
        metrics,
        METRICS_SCHEMA_VERSION,
    )
}
