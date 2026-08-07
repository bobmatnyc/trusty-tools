//! `POST /api/webhooks/{source}` — the console's webhook ingress (#5089 step 3).
//!
//! Why: ADR-0032 removed every sibling daemon's HTTP listener, and ADR-0034
//! rules that console terminates the GitHub request and relays inward over UDS.
//! The two handlers this path replaces both acknowledge GitHub *before* the
//! work runs and downgrade every later failure to a log line
//! (`trusty-review/src/service/webhook.rs:305-309`,
//! `trusty-analyze/src/service/handlers/review.rs:210-214`). GitHub never
//! retries an acknowledged delivery, so each of those failures is permanent,
//! silent loss with every health signal still green.
//!
//! What: one route, multiplexed by `{source}` over both targets. The order of
//! operations is the fix and is not negotiable:
//!
//! 1. unknown `{source}` → `404`, before any secret handling;
//! 2. HMAC verified once, over the exact received bytes; unset secret and bad
//!    signature both → `401` (ADR-0034 §2 unifies the policy to fail-closed);
//! 3. the delivery is written and fsync'd to the spool — **on failure `500`,
//!    and no `202` is ever sent**, so GitHub keeps the delivery redeliverable;
//! 4. the relay runs, and its outcome is recorded durably: an explicit ack
//!    deletes the entry, anything else leaves it `pending` with an incremented
//!    attempt count;
//! 5. `202`, because step 3 succeeded — not because step 4 did.
//!
//! 🔴 Explicitly absent, per ADR-0034 §2: `let _ = relay(...)`, a bare
//! `tracing::warn!` as the sole record of a failed relay, and any `202` issued
//! before the spool write returns.
//!
//! Spawn-on-demand — console starting a target that is not resident — is
//! #5089 step 4. Until then no target binds a listener and every relay lands in
//! [`relay::RelayOutcome::Unreachable`], which is a durable pending state, not
//! a dropped delivery.
//!
//! Test: `tests.rs`.

pub mod health;
pub mod relay;
pub mod spool;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::json;
use trusty_common::webhook_hmac::{HMAC_ALGORITHM, SIGNATURE_HEADER, SignatureVerdict};

use health::{DEFAULT_RED_AFTER, SpoolHealth};
use relay::{RelayOutcome, UdsRelay};
use spool::{Provenance, SPOOL_SCHEMA_VERSION, Spool, SpoolEntry};

/// Environment variable holding the shared webhook secret.
pub const SECRET_ENV: &str = "GITHUB_WEBHOOK_SECRET";

/// Headers never written to the spool.
///
/// GitHub sends none of these; storing one would put a caller-supplied
/// credential on disk for no benefit.
const HEADER_DENYLIST: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// Result of one ingress attempt, before it becomes an HTTP response.
///
/// Why: keeping the decision separate from axum lets every arm — including the
/// two that must never produce a `202` — be asserted directly, without a router.
/// What: one variant per outcome the ADR distinguishes.
/// Test: the `ingest_*` cases in `tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// `{source}` names no configured target.
    UnknownSource {
        /// The path segment that did not resolve.
        source: String,
    },
    /// No secret is configured. Fail closed.
    SecretMissing,
    /// A secret is configured and the signature did not verify.
    InvalidSignature,
    /// The durable write failed. The caller MUST return 5xx and MUST NOT ack.
    SpoolFailed {
        /// Why the write failed.
        reason: String,
    },
    /// The delivery is durably recorded. Safe to acknowledge GitHub.
    Accepted {
        /// The delivery id the entry is filed under.
        delivery_id: String,
        /// What the relay attempt established. Not a precondition of the ack.
        relay: RelayOutcome,
        /// Set when the post-relay bookkeeping write failed. The delivery is
        /// still durable — this only means the attempt count or the deletion
        /// did not land, both of which are recoverable by the retry sweep.
        bookkeeping_error: Option<String>,
    },
}

/// One relay target reachable through `/api/webhooks/{source}`.
#[derive(Debug, Clone)]
pub struct Target {
    /// Route segment that selects it.
    pub source: String,
    /// UDS client for it.
    pub relay: UdsRelay,
}

/// The webhook ingress: spool, secret, and one relay per target.
///
/// Why: assembled once at startup and shared by the route handler, the metrics
/// handler, and the background retry sweep, so all three see the same spool.
/// What: cheap to clone (everything behind `Arc`).
/// Test: constructed directly in `tests.rs` with a temp-dir spool, so no test
/// mutates a process-global env var.
#[derive(Debug, Clone)]
pub struct WebhookIngress {
    spool: Arc<Spool>,
    secret: Arc<String>,
    key_id: Arc<String>,
    targets: Arc<BTreeMap<String, UdsRelay>>,
    red_after: Duration,
}

impl WebhookIngress {
    /// Assemble an ingress from explicit parts.
    ///
    /// Test: used by every `tests.rs` case.
    pub fn new(spool: Spool, secret: String, key_id: String, targets: Vec<Target>) -> Self {
        Self {
            spool: Arc::new(spool),
            secret: Arc::new(secret),
            key_id: Arc::new(key_id),
            targets: Arc::new(
                targets
                    .into_iter()
                    .map(|t| (t.source, t.relay))
                    .collect::<BTreeMap<_, _>>(),
            ),
            red_after: DEFAULT_RED_AFTER,
        }
    }

    /// Override the red-health threshold.
    pub fn with_red_after(mut self, red_after: Duration) -> Self {
        self.red_after = red_after;
        self
    }

    /// Production wiring: spool under the console data dir, secret from
    /// [`SECRET_ENV`], and one target per relay-capable service.
    ///
    /// Why: the socket paths follow #5099's hardened per-uid scratch directory
    /// rather than the bare `$TMPDIR` convention ADR-0034 §3 rejects. Nothing
    /// binds them until step 4; dialling an absent socket is a clean
    /// `Unreachable`.
    /// What: creates the spool directory eagerly so a misconfigured data dir
    /// fails at startup rather than on the first delivery.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or the spool directory cannot
    /// be created.
    ///
    /// Test: `default_spool_root_lives_under_the_console_data_dir`, plus the
    /// `#[ignore]`d `integration_from_env_*` cases, which point
    /// `TRUSTY_DATA_DIR_OVERRIDE` at a temp dir under a lock.
    pub fn from_env() -> anyhow::Result<Self> {
        let spool = Spool::open(Spool::default_root()?)?;
        let secret = std::env::var(SECRET_ENV).unwrap_or_default();
        let sockets = trusty_common::uds::scratch_socket_dir();
        let targets = vec![
            Target {
                source: "review".to_string(),
                relay: UdsRelay::new(sockets.join("trusty-review-webhook.sock")),
            },
            Target {
                source: "analyze".to_string(),
                relay: UdsRelay::new(sockets.join("trusty-analyze-webhook.sock")),
            },
        ];
        Ok(Self::new(spool, secret, SECRET_ENV.to_string(), targets))
    }

    /// The spool this ingress writes to.
    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    /// Scan the spool and classify its health, now.
    ///
    /// Deliberately not cached — see [`health`]'s module docs.
    pub fn health(&self) -> SpoolHealth {
        health::scan_health(&self.spool, now_unix_ms(), self.red_after)
    }

    /// Verify, spool, relay — in that order.
    ///
    /// Why: the ordering IS the fix; see the module docs. In particular the
    /// spool write happens before this function can return anything a caller
    /// would turn into a `202`, and a relay failure never propagates as a
    /// reason to drop the delivery.
    ///
    /// What: returns an [`IngestOutcome`]; performs no HTTP.
    ///
    /// Test: `ingest_rejects_an_unknown_source`,
    /// `ingest_fails_closed_when_no_secret_is_configured`,
    /// `ingest_rejects_a_forged_signature`,
    /// `ingest_returns_spool_failed_and_never_accepts_when_the_write_fails`,
    /// `ingest_accepts_and_deletes_on_an_explicit_ack`,
    /// `relay_failure_leaves_a_pending_entry_with_an_incremented_attempt_count`.
    pub async fn ingest(&self, source: &str, headers: &HeaderMap, body: &[u8]) -> IngestOutcome {
        let Some(relay) = self.targets.get(source) else {
            return IngestOutcome::UnknownSource {
                source: source.to_string(),
            };
        };

        // Step 2 — one verification, over the exact received bytes. Anything
        // that re-frames the body first destroys the ability to check it.
        let signature = header_str(headers, SIGNATURE_HEADER).unwrap_or_default();
        match trusty_common::webhook_hmac::verify_github_signature(&self.secret, body, &signature) {
            SignatureVerdict::Valid => {}
            SignatureVerdict::SecretMissing => {
                tracing::warn!(
                    source,
                    "{SECRET_ENV} is not set — refusing the delivery (fail-closed, ADR-0034 §2)"
                );
                return IngestOutcome::SecretMissing;
            }
            SignatureVerdict::Invalid => {
                tracing::warn!(source, "webhook HMAC verification failed");
                return IngestOutcome::InvalidSignature;
            }
        }

        let received_at_unix_ms = now_unix_ms();
        let delivery_id = header_str(headers, "x-github-delivery")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| format!("no-delivery-header-{received_at_unix_ms}"));

        let mut entry = SpoolEntry {
            schema_version: SPOOL_SCHEMA_VERSION,
            delivery_id: delivery_id.clone(),
            source: source.to_string(),
            event: header_str(headers, "x-github-event").unwrap_or_default(),
            headers: collect_headers(headers),
            body_b64: BASE64.encode(body),
            provenance: Provenance {
                algorithm: HMAC_ALGORITHM.to_string(),
                key_id: self.key_id.as_str().to_string(),
                verified: true,
            },
            received_at_unix_ms,
            attempts: 0,
            last_error: None,
            last_attempt_at_unix_ms: None,
        };

        // Step 3 — durable BEFORE the ack. A failure here is a 5xx, never a
        // logged-and-accepted delivery.
        let path = match self.spool.persist(&entry) {
            Ok(path) => path,
            Err(e) => {
                tracing::error!(
                    source,
                    delivery_id = %delivery_id,
                    error = %e,
                    "spool write failed — refusing the delivery so GitHub keeps it redeliverable"
                );
                return IngestOutcome::SpoolFailed {
                    reason: format!("{e}"),
                };
            }
        };

        // Step 4 — relay, and record what happened durably either way.
        let outcome = relay.deliver(&entry).await;
        let bookkeeping_error = self.settle(&path, &mut entry, &outcome);

        IngestOutcome::Accepted {
            delivery_id,
            relay: outcome,
            bookkeeping_error,
        }
    }

    /// Apply a relay outcome to the spool: delete on an explicit ack, otherwise
    /// bump the attempt count.
    ///
    /// Why: the single place a spool entry can be removed, so "connection
    /// succeeded" can never be mistaken for "work acknowledged".
    /// What: returns `Some(reason)` when the bookkeeping write itself failed.
    /// The delivery stays durable in that case — a failed delete leaves an entry
    /// the sweep will retry (a duplicate delivery, which is recoverable), and a
    /// failed attempt-bump leaves the count stale (visible as a growing age).
    /// Both are the safe direction.
    /// Test: `ingest_accepts_and_deletes_on_an_explicit_ack`,
    /// `relay_failure_leaves_a_pending_entry_with_an_incremented_attempt_count`.
    fn settle(
        &self,
        path: &std::path::Path,
        entry: &mut SpoolEntry,
        outcome: &RelayOutcome,
    ) -> Option<String> {
        if outcome.is_acked() {
            return match self.spool.remove_acked(path) {
                Ok(()) => None,
                Err(e) => {
                    tracing::error!(
                        delivery_id = %entry.delivery_id,
                        error = %e,
                        "target acknowledged but the spool entry could not be removed; \
                         the retry sweep will redeliver it"
                    );
                    Some(format!("{e}"))
                }
            };
        }

        match self
            .spool
            .record_attempt(entry, outcome.reason().to_string(), now_unix_ms())
        {
            Ok(_) => {
                tracing::warn!(
                    delivery_id = %entry.delivery_id,
                    attempts = entry.attempts,
                    reason = outcome.reason(),
                    "relay did not acknowledge; entry stays pending"
                );
                None
            }
            Err(e) => {
                tracing::error!(
                    delivery_id = %entry.delivery_id,
                    error = %e,
                    "relay failed AND the attempt count could not be recorded; \
                     the entry is still on disk and still pending"
                );
                Some(format!("{e}"))
            }
        }
    }

    /// Re-attempt every pending delivery once.
    ///
    /// Why: ADR-0034 §2 — "Console retries with backoff." A background loop
    /// calls this; detection does not depend on that loop still running,
    /// because [`WebhookIngress::health`] scans on the request.
    /// What: relays each pending entry, deleting only on an explicit ack.
    /// Returns per-sweep counts so a caller can log or assert on them.
    /// Test: `retry_sweep_acks_and_clears_a_pending_entry`,
    /// `retry_sweep_leaves_an_unrelayable_entry_pending_with_more_attempts`.
    pub async fn retry_pending_once(&self) -> SweepReport {
        let listing = match self.spool.list_pending() {
            Ok(listing) => listing,
            Err(e) => {
                tracing::error!(error = %e, "webhook retry sweep could not read the spool");
                return SweepReport {
                    scan_error: Some(format!("{e}")),
                    ..SweepReport::default()
                };
            }
        };

        let mut report = SweepReport {
            undecodable: listing.undecodable.len(),
            ..SweepReport::default()
        };
        for pending in listing.pending {
            let mut entry = pending.entry;
            let Some(relay) = self.targets.get(&entry.source) else {
                // A target removed from the config leaves its deliveries on
                // disk rather than dropping them; the age turns the health
                // state red, which is the correct operator signal.
                report.orphaned += 1;
                continue;
            };
            let outcome = relay.deliver(&entry).await;
            if outcome.is_acked() {
                report.acked += 1;
            } else {
                report.still_pending += 1;
            }
            if self.settle(&pending.path, &mut entry, &outcome).is_some() {
                report.bookkeeping_failures += 1;
            }
        }
        report
    }
}

/// Counts from one [`WebhookIngress::retry_pending_once`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Entries a target acknowledged and that were removed.
    pub acked: usize,
    /// Entries that stayed pending.
    pub still_pending: usize,
    /// Entries whose `source` no longer maps to a configured target.
    pub orphaned: usize,
    /// Entries on disk that could not be decoded.
    pub undecodable: usize,
    /// Entries whose post-relay spool write failed.
    pub bookkeeping_failures: usize,
    /// Set when the spool itself could not be listed.
    pub scan_error: Option<String>,
}

/// `POST /api/webhooks/{source}` — the axum front door.
///
/// Why: a thin mapping from [`IngestOutcome`] to a status code, so the ordering
/// guarantee lives in [`WebhookIngress::ingest`] and cannot be broken by an
/// edit to the HTTP layer.
/// What: `404` unknown source, `401` both refusal arms, `500` spool failure,
/// `202` once the delivery is durable. The body reports the relay state so an
/// operator can see a pending delivery without opening the dashboard.
/// Test: `route_returns_500_and_no_ack_when_the_spool_write_fails`,
/// `route_returns_401_for_an_unset_secret`, `route_returns_202_after_a_durable_write`.
pub async fn webhook_handler(
    State(ingress): State<WebhookIngress>,
    Path(source): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    match ingress.ingest(&source, &headers, &body).await {
        IngestOutcome::UnknownSource { source } => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "unknown webhook source", "source": source})),
        )
            .into_response(),
        // One uniform 401 for both refusals: the response must not tell an
        // unauthenticated caller whether a secret is configured.
        IngestOutcome::SecretMissing | IngestOutcome::InvalidSignature => (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "signature verification failed"})),
        )
            .into_response(),
        IngestOutcome::SpoolFailed { reason } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "error": "could not durably record the delivery; not acknowledged",
                "detail": reason,
            })),
        )
            .into_response(),
        IngestOutcome::Accepted {
            delivery_id,
            relay,
            bookkeeping_error,
        } => (
            StatusCode::ACCEPTED,
            axum::Json(json!({
                "status": "accepted",
                "delivery_id": delivery_id,
                "relay": if relay.is_acked() { "acknowledged" } else { "pending" },
                "detail": relay.reason(),
                "bookkeeping_error": bookkeeping_error,
            })),
        )
            .into_response(),
    }
}

/// `GET /api/console/metrics/webhooks` — oldest-pending-age as a health state.
///
/// Why: ADR-0034 §2 requires the signal on console's existing metrics surface,
/// red once a delivery has been pending too long.
/// What: scans the spool on this request and returns a `ConsoleMetricsReport`.
/// Always `200`, never `503`: a red report is information, and a `503` would be
/// indistinguishable from "no data yet" — which is the fail-quiet reading this
/// signal exists to prevent.
/// Test: `metrics_route_reports_red_for_an_aged_pending_entry`,
/// `metrics_route_reports_ok_on_an_empty_spool`.
pub async fn metrics_webhooks_handler(
    State(ingress): State<WebhookIngress>,
) -> axum::response::Response {
    axum::Json(health::to_report(&ingress.health())).into_response()
}

/// Milliseconds since the Unix epoch, saturating at 0 before it.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// One header value as a `String`, lowercased name, `None` when absent or not
/// valid UTF-8.
fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Every header worth spooling, lowercased, minus [`HEADER_DENYLIST`].
fn collect_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            if HEADER_DENYLIST.contains(&name.as_str()) {
                return None;
            }
            value.to_str().ok().map(|v| (name, v.to_string()))
        })
        .collect()
}

/// The spool root, for callers that need it before an ingress exists.
pub fn default_spool_root() -> anyhow::Result<PathBuf> {
    Spool::default_root()
}

/// Run [`WebhookIngress::retry_pending_once`] on an interval, forever.
///
/// Why: recovery for entries whose first relay failed — expected for every
/// delivery until #5089 step 4 binds the targets' listeners.
/// What: spawns a detached task. Deliberately NOT the detection path: if this
/// task dies, `GET /api/console/metrics/webhooks` still turns red, because it
/// scans the spool on the request rather than reading anything this loop wrote.
/// Test: the sweep body is tested directly via `retry_sweep_*`; the timer
/// wrapper carries no logic to test.
pub fn start_retry_sweep(ingress: WebhookIngress, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let report = ingress.retry_pending_once().await;
            if report.acked > 0 || report.still_pending > 0 || report.scan_error.is_some() {
                tracing::info!(
                    acked = report.acked,
                    still_pending = report.still_pending,
                    orphaned = report.orphaned,
                    undecodable = report.undecodable,
                    "webhook retry sweep completed"
                );
            }
        }
    });
}
