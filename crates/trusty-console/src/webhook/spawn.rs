//! Starting a relay target that is not running (#5182, ADR-0034 §1).
//!
//! Why: milestone `tm 1.3.5` criterion (c) wants no resident `trusty-analyze`
//! or `trusty-review` daemon, and relaying over UDS wants a bound listener.
//! Both hold only if something makes the target resident at the moment of
//! delivery. Measured, that trade is cheap: #5028 recorded zero webhook
//! deliveries in 14 days, a 191 ms cold start, and a 36.7 s median review — so
//! the spawn is half a percent of the work it precedes.
//!
//! What: a thin wrapper over `trusty_common::uds::UdsServiceSupervisor`, the
//! supervisor promoted from `trusty-memory`'s `Bm25Supervisor` in #5089 step 2.
//! Writing a second supervisor here would re-earn the scars of #2845, #2846 and
//! #5085 — the serialised spawn gate, the live-child cap, the socket-decides-
//! liveness rule — so this only supplies the per-service parts: which binary,
//! which argv, and the timing budget.
//!
//! Test: `webhook/tests.rs` — `spawn_*`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use trusty_common::uds::{
    ServiceTimeouts, SpawnSpec, SupervisorConfig, SupervisorError, UdsServiceSupervisor,
};
use trusty_common::webhook_relay::LISTENER_SHUTDOWN_FLUSH;

/// Subcommand every relay target implements to serve its socket.
pub const LISTEN_SUBCOMMAND: &str = "webhook-listen";

/// Environment variable that hands target lifecycle back to the operator.
///
/// Set to exactly `"1"` when running the targets under `tctl` (ADR-0011): the
/// supervisor then dials whatever is at the socket and never spawns.
pub const ENV_EXTERNAL_TARGETS: &str = "TRUSTY_WEBHOOK_TARGET_EXTERNAL";

/// How long a freshly-spawned target has to bind and accept.
///
/// Generous rather than tight: `trusty-review` cold-starts in 191 ms (#5028)
/// but a first run after an upgrade pays page-in costs on a much larger binary,
/// and a spawn that times out looks to an operator like a broken install.
const SPAWN_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// SIGTERM-to-SIGKILL patience, strictly above the listener's own flush budget.
///
/// 🔴 This `const` item is the compile-time guard. `ServiceTimeouts::new` is a
/// `const fn` that asserts `sigterm_patience > shutdown_flush`, so lowering this
/// to or below [`LISTENER_SHUTDOWN_FLUSH`] fails the build rather than shipping
/// a SIGKILL that lands inside a delivery the child is mid-way through.
const SIGTERM_PATIENCE: Duration = Duration::from_secs(5);

/// The targets' timing budget.
///
/// `shutdown_flush` is the listener's OWN constant, imported from the contract
/// module both halves share — not a literal that happens to match it today.
/// Console cannot depend on `trusty-review` or `trusty-analyze`, so that shared
/// module is where the number has to live for the sourcing rule on
/// [`ServiceTimeouts`] to be satisfiable at all.
const TARGET_TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    SPAWN_PROBE_TIMEOUT,
    LISTENER_SHUTDOWN_FLUSH,
    SIGTERM_PATIENCE,
);

/// Supervises the two webhook relay targets on demand.
///
/// Why: one supervisor across both services rather than one each, so the
/// live-child cap is a statement about console's whole child population.
/// What: `ensure_running` keyed by source (`review` / `analyze`), with the
/// binary located lazily so an already-running or externally-managed target
/// never requires it to be installed.
/// Test: `spawn_adopts_a_socket_that_is_already_served`,
/// `spawn_maps_each_source_to_its_binary`. The external-mode opt-out itself is
/// `trusty-common`'s `external_env_only_honours_exactly_one` — console supplies
/// only the variable name, and a test here would have to mutate a
/// process-global env var that every sibling in the binary can see.
#[derive(Debug)]
pub struct TargetSupervisor {
    inner: UdsServiceSupervisor,
}

impl TargetSupervisor {
    /// Build a supervisor for the relay targets.
    ///
    /// `max_live` is 2 — there are exactly two targets, and a cap that reaped
    /// one to make room for the other would thrash under a two-source burst.
    pub fn new() -> Self {
        Self {
            inner: UdsServiceSupervisor::new(
                SupervisorConfig::new("trusty-webhook-target", 2, TARGET_TIMEOUTS)
                    .with_external_env(ENV_EXTERNAL_TARGETS),
            ),
        }
    }

    /// How many children this supervisor has launched.
    pub fn spawned_count(&self) -> u64 {
        self.inner.spawned_count()
    }

    /// Ensure something is serving `socket` for `source`.
    ///
    /// Why: called immediately before a relay, so the socket is bound by the
    /// time the frame is written. The supervisor's own fast path means a target
    /// already serving costs one probe, not a spawn.
    ///
    /// # Errors
    ///
    /// [`SupervisorError`] when the binary cannot be found, the spawn fails, or
    /// the child never binds. Every one leaves the delivery unrelayed and
    /// therefore unacked — the caller records it as `Unreachable`, which keeps
    /// the spool entry.
    ///
    /// Test: `spawn_adopts_a_socket_that_is_already_served` covers the path
    /// that resolves no binary; the spawn path itself is covered by
    /// `trusty-common`'s supervisor suite rather than by launching a real
    /// `trusty-review` from a unit test.
    pub async fn ensure_running(
        &self,
        source: &str,
        socket: &Path,
    ) -> Result<PathBuf, SupervisorError> {
        let binary = target_binary_name(source).to_string();
        self.inner
            .ensure_running(source, socket, move || {
                let program = trusty_common::bin_resolve::resolve_binary(&binary).ok_or_else(
                    || -> Box<dyn std::error::Error + Send + Sync> {
                        format!(
                            "{binary} is not installed or not on PATH; \
                             console cannot start the webhook target"
                        )
                        .into()
                    },
                )?;
                Ok(SpawnSpec::new(program).arg(LISTEN_SUBCOMMAND))
            })
            .await
    }

    /// SIGTERM every child and clean up its socket.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

impl Default for TargetSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// The binary that serves a given `{source}` route segment.
///
/// Test: `spawn_maps_each_source_to_its_binary`.
pub fn target_binary_name(source: &str) -> &'static str {
    match source {
        trusty_common::webhook_relay::ANALYZE_SOURCE => "trusty-analyze",
        // `review` is the only other configured source; anything else never
        // reaches here, because `ingest` rejects an unknown source with a 404
        // before a relay is selected.
        _ => "trusty-review",
    }
}

/// Shared handle a [`super::relay::UdsRelay`] holds.
pub type SharedSupervisor = Arc<TargetSupervisor>;
