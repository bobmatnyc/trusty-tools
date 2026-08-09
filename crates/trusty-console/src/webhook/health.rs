//! Oldest-pending-age as a red health state on `/api/console/metrics/*`.
//!
//! Why: ADR-0034 §2 — "A pending entry older than a threshold is a **red**
//! health state … not a `warn!` line in a log nobody reads." Two traps sit one
//! level below that, and this module is shaped around both.
//!
//! The first: a threshold evaluated only inside a background poll loop is
//! fail-quiet again, because a loop that silently stops leaves the last cached
//! value looking healthy forever. So the scan runs **on the request**, against
//! the directory, every time — no cache to go stale, and a scan that fails is
//! itself red rather than empty.
//!
//! The second: a signal that goes red and then never changes is the same log
//! line wearing a status field. Exhausted entries are by construction the
//! oldest, so computing `oldest_pending_*` across the whole spool pins those
//! diagnostics to the first poisoned delivery permanently — day 30's genuinely
//! new failure moves nothing an operator or alert rule reads. So exhausted
//! entries get their own count and ids, and `oldest_pending_*` describes the
//! oldest **live** entry, which is the one still changing.
//!
//! What: [`scan_health`] takes a filename-only census
//! ([`Spool::scan_metadata`], which opens nothing) and decodes exactly one
//! entry — the oldest live one — for its attempt count and last error.
//! [`to_report`] wraps the result in the workspace-standard
//! `ConsoleMetricsReport` so the existing dashboard renders it with no new
//! contract.
//!
//! Test: `webhook/tests.rs` — `health_*` cases cover empty, young-pending,
//! aged-pending, exhausted-alongside-live, undecodable-entry, unreadable-spool,
//! and a vanished spool directory.

use std::path::PathBuf;
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
pub const METRICS_SCHEMA_VERSION: u32 = 2;

/// Service id this report is published under.
pub const WEBHOOK_SERVICE_ID: &str = "trusty-console-webhooks";

/// Most exhausted delivery ids listed individually before the payload is
/// truncated. The count is always exact; the list is a sample.
const MAX_LISTED_EXHAUSTED: usize = 10;

/// What one on-demand scan of the spool found.
///
/// Why: an operator diagnosing a stuck webhook needs the age, the counts, and
/// the failing delivery's id and last error, in the one payload the dashboard
/// already fetches — and needs them to keep *moving* as new deliveries fail,
/// which is why the live and exhausted sets are reported separately.
/// What: serialised as the `metrics` object of a `ConsoleMetricsReport`.
/// `scan_error` being `Some` is always accompanied by `status: Error`.
/// Test: every `health_*` case in `webhook/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolHealth {
    /// Coarse classification; see [`scan_health`] for the rules.
    pub status: ServiceHealth,
    /// Live entries still eligible for relay.
    pub pending: usize,
    /// Entries console has stopped retrying. Non-zero always means red, and
    /// always means an operator has to act — nothing clears these on its own.
    pub exhausted: usize,
    /// Delivery ids of exhausted entries, up to [`MAX_LISTED_EXHAUSTED`].
    pub exhausted_delivery_ids: Vec<String>,
    /// Age of the oldest exhausted entry, in seconds.
    pub oldest_exhausted_age_secs: Option<u64>,
    /// Age of the oldest **live** entry, in seconds.
    ///
    /// Deliberately excludes exhausted entries: they are permanently the
    /// oldest, so including them freezes this field at the first poisoned
    /// delivery and it stops reporting anything new.
    pub oldest_pending_age_secs: Option<u64>,
    /// Delivery id of the oldest live entry.
    pub oldest_pending_delivery_id: Option<String>,
    /// Why the oldest live entry's last relay attempt failed.
    pub oldest_pending_last_error: Option<String>,
    /// How many attempts the oldest live entry has already burned.
    ///
    /// Replaces a spool-wide attempt total, which cost one JSON decode per
    /// entry on every metrics request. The oldest live entry's count is the
    /// actionable number and costs exactly one decode.
    pub oldest_pending_attempts: Option<u32>,
    /// Threshold in force for this scan.
    pub red_after_secs: u64,
    /// Entries present on disk that could not be read or whose name did not
    /// parse, with reasons. Never empty while `status` is `Ok`.
    pub undecodable: Vec<String>,
    /// Why the scan itself failed, if it did.
    pub scan_error: Option<String>,
    /// Deliveries a target acknowledged and has not yet processed, per source.
    ///
    /// 🔴 #5182 review: without this the signal INVERTS. An acknowledged
    /// delivery is deleted from the spool, so `pending` drops to zero and the
    /// status goes green — while the work sits in the target's inbox with
    /// nothing consuming it (the drain step is
    /// [#5192](https://github.com/bobmatnyc/trusty-tools/issues/5192)). Before
    /// the listeners existed an undrained backlog was at least VISIBLE as a
    /// pending spool entry; metering the inbox is what keeps it visible now.
    pub undrained: Vec<UndrainedTarget>,
    /// Total across [`SpoolHealth::undrained`]. Non-zero is never `Ok`.
    pub undrained_total: usize,
}

/// How much acknowledged-but-unprocessed work one target is holding.
///
/// Test: `health_is_degraded_while_a_delivery_sits_undrained`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndrainedTarget {
    /// Route segment the target serves (`review` / `analyze`).
    pub source: String,
    /// Deliveries held in its inbox.
    pub held: usize,
    /// Why the count could not be taken, if it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Scan the spool and classify its health, right now.
///
/// Why: called per request rather than per poll tick. The whole point of the
/// signal is to catch a path that has quietly stopped working, and a signal
/// that itself depends on a background task still running does not do that.
///
/// What: classification, in order —
/// 1. the census failed → `Error`, with `scan_error` set;
/// 2. a file on disk could not be read or named → `Error`;
/// 3. any entry is exhausted → `Error` (nothing clears those without an
///    operator);
/// 4. the oldest **live** entry is at least `red_after` old → `Error`;
/// 5. anything is live-pending → `Degraded`;
/// 6. any target holds an acknowledged-but-undrained delivery → `Degraded`;
/// 7. otherwise → `Ok`.
///
/// Rule 6 is the one that stops the signal inverting — see
/// [`SpoolHealth::undrained`]. `inbox_roots` maps each configured source to the
/// directory its receiver writes to; an empty slice means nothing is metered,
/// which is only correct for an ingress with no targets.
///
/// Cost: one `read_dir` per directory and at most one JSON decode — of the
/// oldest live entry, for its attempt count and last error. If that decode
/// fails the entry is reported through `undecodable` rather than the scan being
/// abandoned.
///
/// `now_unix_ms` is a parameter so the age rules are testable without sleeping.
/// An entry whose timestamp is in the future yields age `0` rather than
/// underflowing.
///
/// Test: `health_reports_ok_on_an_empty_spool`,
/// `health_reports_degraded_for_a_young_pending_entry`,
/// `health_reports_error_once_the_oldest_entry_passes_the_threshold`,
/// `health_diagnostics_track_the_live_entry_not_the_exhausted_one`,
/// `health_reports_error_for_an_undecodable_entry`,
/// `health_reports_error_when_the_spool_cannot_be_read`,
/// `health_reports_error_when_the_spool_directory_is_gone`.
pub fn scan_health(
    spool: &Spool,
    now_unix_ms: u64,
    red_after: Duration,
    inbox_roots: &[(String, PathBuf)],
) -> SpoolHealth {
    let red_after_secs = red_after.as_secs();
    let undrained = scan_undrained(inbox_roots);
    let undrained_total = undrained.iter().map(|u| u.held).sum::<usize>();
    let age_of = |at: u64| now_unix_ms.saturating_sub(at).div_euclid(1000);

    let census = match spool.scan_metadata() {
        Ok(census) => census,
        Err(e) => {
            return SpoolHealth {
                status: ServiceHealth::Error,
                red_after_secs,
                // A spool that cannot be read is NOT an empty spool. Reporting
                // it healthy is the fail-quiet shape this module exists to
                // remove.
                scan_error: Some(format!("{e}")),
                undrained,
                undrained_total,
                ..SpoolHealth::empty(red_after_secs)
            };
        }
    };

    let mut undecodable: Vec<String> = census
        .unparsable
        .iter()
        .map(|(path, reason)| format!("{}: {reason}", path.display()))
        .collect();

    // Exactly one decode: the oldest live entry, for the two fields its
    // filename cannot carry.
    let oldest_live = census.live.first();
    let (oldest_pending_last_error, oldest_pending_attempts) = match oldest_live {
        Some(meta) => match spool.load(&meta.path) {
            Ok(entry) => (entry.last_error, Some(entry.attempts)),
            Err(e) => {
                undecodable.push(format!("{}: {e}", meta.path.display()));
                (None, None)
            }
        },
        None => (None, None),
    };

    let oldest_pending_age_secs = oldest_live.map(|m| age_of(m.received_at_unix_ms));
    let oldest_exhausted_age_secs = census
        .exhausted
        .first()
        .map(|m| age_of(m.received_at_unix_ms));
    let aged_out = oldest_pending_age_secs.is_some_and(|age| age >= red_after_secs);

    let status = if !undecodable.is_empty()
        || !census.exhausted.is_empty()
        || aged_out
        || undrained.iter().any(|u| u.error.is_some())
    {
        ServiceHealth::Error
    } else if census.live.is_empty() && undrained_total == 0 {
        ServiceHealth::Ok
    } else {
        // An empty spool with a full inbox is work that arrived and is not
        // being done. Reporting that as Ok is the failure this rule removes.
        ServiceHealth::Degraded
    };

    SpoolHealth {
        status,
        pending: census.live.len(),
        exhausted: census.exhausted.len(),
        exhausted_delivery_ids: census
            .exhausted
            .iter()
            .take(MAX_LISTED_EXHAUSTED)
            .map(|m| m.delivery_id.clone())
            .collect(),
        oldest_exhausted_age_secs,
        oldest_pending_age_secs,
        oldest_pending_delivery_id: oldest_live.map(|m| m.delivery_id.clone()),
        oldest_pending_last_error,
        oldest_pending_attempts,
        red_after_secs,
        undecodable,
        scan_error: None,
        undrained,
        undrained_total,
    }
}

/// Count what each target is holding, without creating or touching its inbox.
///
/// Why: console meters a directory another service owns, so asking must have no
/// side effect. An unreadable inbox is reported as an error rather than a zero —
/// "I could not count" and "there is nothing" are the two answers this whole
/// module exists to keep apart.
/// Test: `health_is_degraded_while_a_delivery_sits_undrained`,
/// `health_is_error_when_a_targets_inbox_cannot_be_counted`.
fn scan_undrained(inbox_roots: &[(String, PathBuf)]) -> Vec<UndrainedTarget> {
    inbox_roots
        .iter()
        .map(
            |(source, root)| match trusty_common::webhook_relay::held_count(root) {
                Ok(held) => UndrainedTarget {
                    source: source.clone(),
                    held,
                    error: None,
                },
                Err(e) => UndrainedTarget {
                    source: source.clone(),
                    held: 0,
                    error: Some(format!("{e}")),
                },
            },
        )
        .collect()
}

/// A red report for a scan that could not run at all.
///
/// Why: the caller that needs this is `WebhookIngress::health`, when the
/// blocking task carrying the scan fails to join. "The scan did not happen" is
/// not "the spool is empty", and only one of those is safe to render green.
/// Test: covered by the same rule as `health_reports_error_when_the_spool_cannot_be_read`.
pub fn scan_failed(red_after: Duration, reason: String) -> SpoolHealth {
    SpoolHealth {
        status: ServiceHealth::Error,
        scan_error: Some(reason),
        ..SpoolHealth::empty(red_after.as_secs())
    }
}

impl SpoolHealth {
    /// A healthy, empty report — the base every classification starts from.
    fn empty(red_after_secs: u64) -> Self {
        Self {
            status: ServiceHealth::Ok,
            pending: 0,
            exhausted: 0,
            exhausted_delivery_ids: Vec::new(),
            oldest_exhausted_age_secs: None,
            oldest_pending_age_secs: None,
            oldest_pending_delivery_id: None,
            oldest_pending_last_error: None,
            oldest_pending_attempts: None,
            red_after_secs,
            undecodable: Vec::new(),
            scan_error: None,
            undrained: Vec::new(),
            undrained_total: 0,
        }
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
