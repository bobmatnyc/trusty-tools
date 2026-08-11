//! `POST /api/webhooks/{source}` — the console's webhook ingress (#5089 step 3).
//!
//! Why: ADR-0032 removed every sibling daemon's HTTP listener, and ADR-0034
//! rules that console terminates the GitHub request and relays inward over UDS.
//! The two handlers this path replaced both acknowledged GitHub *before* the
//! work ran and downgraded every later failure to a log line. GitHub never
//! retries an acknowledged delivery, so each of those failures was permanent,
//! silent loss with every health signal still green. #5181 deleted them —
//! `trusty-review`'s `POST /pr/github/webhook` and `trusty-analyze`'s
//! `POST /webhooks/github` now 404 — so this route is the only HTTP webhook
//! surface in the workspace, and the only holder of the shared secret.
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
//! Spawn-on-demand — console starting a target that is not resident — landed in
//! #5182 alongside the targets' listeners: [`spawn::TargetSupervisor`] runs
//! `ensure_running` before each relay. A target that will not start is still
//! [`relay::RelayOutcome::Unreachable`], which is a durable pending state, not a
//! dropped delivery.
//!
//! Test: `tests.rs`.

pub mod health;
pub mod relay;
pub mod schedule;
pub mod spawn;
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
use schedule::ClaimSet;
use spool::{Provenance, SPOOL_SCHEMA_VERSION, Spool, SpoolEntry, SpoolError};

pub use schedule::BackoffPolicy;

/// Most entries one sweep pass will relay.
///
/// Why: the sweep is serial and each relay can burn its full timeout, so an
/// unbounded pass can outlast its own tick interval. Backoff already keeps the
/// due set small; this bounds the pathological case where it is not. Entries
/// left over are simply relayed on the next tick — they stay pending and
/// durable in the meantime.
const SWEEP_BUDGET: usize = 32;

/// Largest webhook body the ingress route accepts.
///
/// Why: axum's `DefaultBodyLimit` is 2 MiB, and a rejection there happens
/// *before* this module's handler runs — no spool entry, no metric, no log,
/// just a 413 GitHub records and nobody reads. That is the invisible drop the
/// whole step exists to remove, arriving through the framework instead of the
/// code. GitHub payloads are legal to 25 MB and `push` / `pull_request` bodies
/// routinely exceed 2 MiB, so the default silently refuses real deliveries.
/// What: 25 MiB, matching GitHub's documented ceiling. Applied only to the
/// webhook sub-router (`server::build_router_with_webhooks`), so the proxy and
/// SPA routes keep the framework default.
/// Test: `route_accepts_a_body_larger_than_the_axum_default_limit`.
pub const MAX_WEBHOOK_BODY_BYTES: usize = 25 * 1024 * 1024;

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
    /// Which entries are being relayed right now, so the sweep and the request
    /// path cannot both relay one delivery. See [`schedule::ClaimSet`].
    claims: ClaimSet,
    /// When a pending entry becomes eligible for another attempt.
    backoff: BackoffPolicy,
    /// Each target's inbox, so an acknowledged-but-undrained delivery is
    /// metered rather than disappearing from the signal. See
    /// [`health::SpoolHealth::undrained`] and #5192.
    inbox_roots: Arc<Vec<(String, PathBuf)>>,
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
            claims: ClaimSet::new(),
            backoff: BackoffPolicy::default(),
            // Deliberately empty rather than derived: deriving would make every
            // unit test read the developer's real `~/…/webhook-inbox`. Production
            // wiring is `from_env`, and `from_env_meters_every_targets_inbox`
            // pins that it populates this.
            inbox_roots: Arc::new(Vec::new()),
        }
    }

    /// Meter these inboxes when reporting health.
    ///
    /// Why: an acknowledged delivery leaves the spool, so without this the
    /// status goes green while the work sits unprocessed in a target's inbox.
    /// Test: `health_is_degraded_while_a_delivery_sits_undrained`.
    pub fn with_inbox_roots(mut self, roots: Vec<(String, PathBuf)>) -> Self {
        self.inbox_roots = Arc::new(roots);
        self
    }

    /// Override the red-health threshold.
    pub fn with_red_after(mut self, red_after: Duration) -> Self {
        self.red_after = red_after;
        self
    }

    /// Override the retry schedule.
    ///
    /// Test: the `sweep_*` and `backoff_*` cases use a zeroed grace so a sweep
    /// runs without waiting out the real 5 s hold-off.
    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    /// The retry schedule in force.
    pub fn backoff(&self) -> BackoffPolicy {
        self.backoff
    }

    /// Production wiring: spool under the console data dir, secret from
    /// [`SECRET_ENV`], and one target per relay-capable service.
    ///
    /// Why: the socket paths come from `trusty_common::uds::scratch_socket_dir`,
    /// the shared entry point #5099 built. That is `$TMPDIR/trusty-<uid>` with a
    /// `/tmp` fallback — the *base* ADR-0034 §3 names, but not the exposure it
    /// objects to: the uid-keyed subdirectory is created at `0700` and owned by
    /// this process, and `connect_hardened` re-verifies owner and mode before
    /// dialling. #5099 supersedes §3's "use the service state directory instead"
    /// path rule by making the scratch path satisfy the property §3 wanted.
    /// Nothing binds these sockets until step 4; dialling an absent one is a
    /// clean `Unreachable`.
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
        // #5182: the paths come from the shared contract rather than a literal
        // here, so the sender and the two receivers cannot disagree about them.
        let supervisor: spawn::SharedSupervisor = Arc::new(spawn::TargetSupervisor::new());
        let mut targets = Vec::new();
        for source in [
            trusty_common::webhook_relay::REVIEW_SOURCE,
            trusty_common::webhook_relay::ANALYZE_SOURCE,
        ] {
            let socket = trusty_common::webhook_relay::socket_path_for(source)
                .ok_or_else(|| anyhow::anyhow!("no socket is defined for source {source}"))?;
            targets.push(Target {
                source: source.to_string(),
                relay: UdsRelay::new(socket).with_supervisor(source, Arc::clone(&supervisor)),
            });
        }
        // #5182 review: meter each target's inbox. Without it an acknowledged
        // delivery leaves the spool and the signal goes green while the work
        // sits unprocessed — see `health::SpoolHealth::undrained` and #5192.
        let mut inbox_roots = Vec::new();
        for source in [
            trusty_common::webhook_relay::REVIEW_SOURCE,
            trusty_common::webhook_relay::ANALYZE_SOURCE,
        ] {
            let root = trusty_common::webhook_relay::inbox_root_for(source)
                .ok_or_else(|| anyhow::anyhow!("no inbox is defined for source {source}"))??;
            inbox_roots.push((source.to_string(), root));
        }
        Ok(Self::new(spool, secret, SECRET_ENV.to_string(), targets).with_inbox_roots(inbox_roots))
    }

    /// The spool this ingress writes to.
    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    /// Run one blocking spool operation off the async runtime.
    ///
    /// Why: every spool call does real filesystem work — `persist_new` alone
    /// fsyncs a file and a directory, and a census `read_dir`s two of them.
    /// Doing that inline stalls a runtime worker thread, and the ingest path
    /// runs it twice per delivery while the metrics route runs it per request.
    /// What: `spawn_blocking` over a cloned [`Spool`] (a `PathBuf` and a flag,
    /// so cloning is free). A join failure — the blocking pool shutting down
    /// mid-operation — is surfaced as an error rather than silently swallowed.
    /// Test: exercised by every async case; the correctness of each operation
    /// is covered by its own `spool_*` case.
    async fn blocking<T, F>(&self, op: F) -> Result<T, SpoolError>
    where
        F: FnOnce(Spool) -> Result<T, SpoolError> + Send + 'static,
        T: Send + 'static,
    {
        let spool = (*self.spool).clone();
        match tokio::task::spawn_blocking(move || op(spool)).await {
            Ok(result) => result,
            Err(join) => Err(SpoolError::PrepareDir {
                path: self.spool.root().to_path_buf(),
                source: std::io::Error::other(format!("spool task did not complete: {join}")),
            }),
        }
    }

    /// Scan the spool and classify its health, now.
    ///
    /// Deliberately not cached — see [`health`]'s module docs. The scan is
    /// filesystem work, so it runs off the async runtime.
    pub async fn health(&self) -> SpoolHealth {
        let red_after = self.red_after;
        let now = now_unix_ms();
        let spool = (*self.spool).clone();
        let roots = Arc::clone(&self.inbox_roots);
        match tokio::task::spawn_blocking(move || {
            health::scan_health(&spool, now, red_after, &roots)
        })
        .await
        {
            Ok(health) => health,
            // A scan that could not run is not a healthy spool.
            Err(join) => health::scan_failed(
                red_after,
                format!("health scan task did not complete: {join}"),
            ),
        }
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

        // 🔴 Claim BEFORE the write, not after it. `entry_path` is a pure
        // function of the receipt time and delivery id, both already fixed, so
        // the path is known before the entry exists. Claiming afterwards left a
        // window in which the entry was on disk and unclaimed — and the write
        // now runs on the blocking pool, which widens that window to however
        // long a worker thread takes to be scheduled. A sweep landing there
        // relayed a delivery this request was about to relay itself. Measured,
        // not theorised: with the claim taken after the write, a sweep 20 ms
        // into `ingest` reports `acked: 1` for an entry the request path then
        // relays again.
        //
        // Backoff's first-attempt grace also covers this window in production,
        // but two guards that each fully close it is the point — a deployment
        // that tunes the grace to zero must not reopen a double-relay.
        //
        // 🔴 No regression test pins this ordering. The window is one scheduler
        // poll wide, and every deterministic probe tried against it — a
        // rendezvous, a select! loop, a parallel hammer on a 4-worker runtime —
        // passed against a claim-after-write build as readily as against this
        // one. A test that cannot tell the two apart is worse than none, so
        // none was kept. Do not reorder these two statements on the strength of
        // a green suite.
        let claim = self.claims.claim(&self.spool.entry_path(&entry));

        // Step 3 — durable BEFORE the ack. A failure here is a 5xx, never a
        // logged-and-accepted delivery. `persist_new` refuses to overwrite: the
        // entry already at that path may be one console has acknowledged, and
        // GitHub will never re-send it.
        let to_write = entry.clone();
        let path = match self
            .blocking(move |spool| spool.persist_new(&to_write))
            .await
        {
            Ok(path) => path,
            Err(e) => {
                let already = matches!(e, SpoolError::AlreadyExists { .. });
                tracing::error!(
                    source,
                    delivery_id = %delivery_id,
                    error = %e,
                    already_spooled = already,
                    "spool write failed — refusing the delivery so GitHub keeps it redeliverable"
                );
                return IngestOutcome::SpoolFailed {
                    reason: format!("{e}"),
                };
            }
        };

        // Step 4 — relay, and record what happened durably either way, holding
        // the claim taken before the write. It is released on drop, so a
        // panicking relay cannot wedge the entry.
        let outcome = match claim {
            Some(_claim) => {
                let outcome = relay.deliver(&entry).await;
                let bookkeeping_error = self.settle(&path, &mut entry, &outcome).await;
                return IngestOutcome::Accepted {
                    delivery_id,
                    relay: outcome,
                    bookkeeping_error,
                };
            }
            // Someone else is already relaying this exact path. The delivery is
            // durable, so acknowledging is still correct; the in-flight relay
            // (or the next sweep) settles it.
            None => RelayOutcome::Unreachable {
                reason: "another relay for this entry is already in flight".to_string(),
            },
        };

        IngestOutcome::Accepted {
            delivery_id,
            relay: outcome,
            bookkeeping_error: None,
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
    async fn settle(
        &self,
        path: &std::path::Path,
        entry: &mut SpoolEntry,
        outcome: &RelayOutcome,
    ) -> Option<String> {
        if outcome.is_acked() {
            let acked_path = path.to_path_buf();
            return match self
                .blocking(move |spool| spool.remove_acked(&acked_path))
                .await
            {
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

        let mut updated = entry.clone();
        let reason = outcome.reason().to_string();
        let now = now_unix_ms();
        let written = self
            .blocking(move |spool| {
                spool
                    .record_attempt(&mut updated, reason, now)
                    .map(|_| updated)
            })
            .await;
        match written {
            Ok(after) => {
                *entry = after;
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

    /// Re-attempt every pending delivery that is due, once.
    ///
    /// Why: ADR-0034 §2 — "Console retries with backoff." Three guards, each
    /// closing a different failure:
    ///
    /// - **Backoff** ([`BackoffPolicy::is_due`]) — without it every pending
    ///   entry is re-relayed on every tick and each non-ack rewrites the whole
    ///   base64 body plus two `fsync`s. Until step 4 binds a listener that is
    ///   every delivery, forever.
    /// - **Claims** ([`schedule::ClaimSet`]) — without them a tick landing
    ///   inside the ≤5 s relay window sends a delivery the request path is
    ///   still sending. One delivery, two relays.
    /// - **[`SWEEP_BUDGET`]** — the pass is serial and each relay can burn its
    ///   full timeout, so an unbounded pass can outlast its own tick interval.
    ///
    /// Nothing any guard skips is dropped: it stays pending, durable, and
    /// visible to [`WebhookIngress::health`], which scans on the request rather
    /// than trusting this loop to still be alive.
    ///
    /// What: relays each due entry, deleting only on an explicit ack. Returns
    /// per-sweep counts.
    ///
    /// Test: `retry_sweep_acks_and_clears_a_pending_entry`,
    /// `retry_sweep_leaves_an_unrelayable_entry_pending_with_more_attempts`,
    /// `sweep_does_not_relay_an_entry_the_request_path_is_still_relaying`,
    /// `sweep_honours_backoff_between_ticks`,
    /// `sweep_stops_relaying_an_exhausted_entry`.
    pub async fn retry_pending_once(&self) -> SweepReport {
        let listing = match self.blocking(|spool| spool.list_pending()).await {
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
        let now = now_unix_ms();
        for pending in listing.pending {
            if report.acked + report.still_pending >= SWEEP_BUDGET {
                report.deferred += 1;
                continue;
            }
            let mut entry = pending.entry;
            let Some(relay) = self.targets.get(&entry.source) else {
                // A target removed from the config leaves its deliveries on
                // disk rather than dropping them; the age turns the health
                // state red, which is the correct operator signal.
                report.orphaned += 1;
                continue;
            };
            if self.backoff.is_exhausted(&entry) {
                // Move it out of the live set. Leaving it here would make every
                // later sweep and every metrics request read and decode it
                // forever, and would pin the oldest-pending diagnostics to it so
                // a genuinely new failure changed nothing an operator reads.
                // It is kept, not deleted — it is still an unacknowledged
                // webhook, and it keeps the health signal red.
                report.exhausted += 1;
                let quarantine_path = pending.path.clone();
                if let Err(e) = self
                    .blocking(move |spool| spool.quarantine(&quarantine_path))
                    .await
                {
                    tracing::error!(
                        delivery_id = %entry.delivery_id,
                        error = %e,
                        "could not move an exhausted entry aside; it stays in the live set"
                    );
                    report.bookkeeping_failures += 1;
                }
                continue;
            }
            if !self.backoff.is_due(&entry, now) {
                report.not_due += 1;
                continue;
            }
            let Some(_claim) = self.claims.claim(&pending.path) else {
                report.in_flight += 1;
                continue;
            };

            let outcome = relay.deliver(&entry).await;
            if outcome.is_acked() {
                report.acked += 1;
            } else {
                report.still_pending += 1;
            }
            if self
                .settle(&pending.path, &mut entry, &outcome)
                .await
                .is_some()
            {
                report.bookkeeping_failures += 1;
            }
        }
        report
    }
}

/// Counts from one [`WebhookIngress::retry_pending_once`] pass.
///
/// Every pending entry lands in exactly one bucket, so the counts account for
/// the whole spool — a delivery that vanishes from all of them is a bug the
/// tests can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Entries a target acknowledged and that were removed.
    pub acked: usize,
    /// Entries relayed this pass that stayed pending.
    pub still_pending: usize,
    /// Entries not yet eligible under the backoff schedule.
    pub not_due: usize,
    /// Entries past `max_attempts` — never retried again, never deleted, and
    /// holding the health signal red until an operator intervenes.
    pub exhausted: usize,
    /// Entries another relay was already handling.
    pub in_flight: usize,
    /// Entries left for the next tick by [`SWEEP_BUDGET`].
    pub deferred: usize,
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
    axum::Json(health::to_report(&ingress.health().await)).into_response()
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
            if report.acked > 0
                || report.still_pending > 0
                || report.exhausted > 0
                || report.scan_error.is_some()
            {
                tracing::info!(
                    acked = report.acked,
                    still_pending = report.still_pending,
                    not_due = report.not_due,
                    exhausted = report.exhausted,
                    in_flight = report.in_flight,
                    deferred = report.deferred,
                    orphaned = report.orphaned,
                    undecodable = report.undecodable,
                    "webhook retry sweep completed"
                );
            }
        }
    });
}
