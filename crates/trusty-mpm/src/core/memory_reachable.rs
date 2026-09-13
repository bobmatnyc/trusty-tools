//! Is trusty-memory available right now? (#7685)
//!
//! Why: the owner ruling of 2026-09-12 makes Claude Code's own auto memory a
//! FALLBACK — trusty-memory is the memory, and auto memory stays on only when
//! trusty-memory is not available. Every write site that turns auto memory off
//! (the project-tier `autoMemoryEnabled: false` key and the
//! `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` spawn assignment) therefore needs one
//! yes/no answer, resolved the same way in every caller. A session that loses
//! both memories is the failure this module exists to prevent.
//! What: [`probe_memory_reachability`] issues ONE `memory.health` call against
//! the derived socket and classifies the answer as a [`MemoryReachability`].
//! Only a daemon that answers AND reports `status: "ok"` is reachable — a wedged
//! or degraded daemon, an unreadable body and a refused call all keep the
//! fallback on. [`probe_memory_reachable_blocking`] is the same probe as a
//! boolean for the synchronous launch path, run on its own short-lived thread so
//! it is safe to call from inside an async runtime. [`resolve_memory_reachable`]
//! is what launch consumers actually call: it takes whatever the caller already
//! resolved and probes only when nothing did, so one launch asks the daemon once
//! rather than once per write site.
//!
//! This is deliberately NOT [`crate::daemon::doctor`]'s `probe_health`: that one
//! retries three times and classifies four outcomes because it renders a doctor
//! row. A launch needs a fast, single, fail-safe gate — see [`PROBE_TIMEOUT`].
//! Test: `core::memory_reachable::tests`.

use std::time::Duration;

use trusty_common::memory_rpc::MemoryHealthStatus;

/// How long one launch-time reachability probe may take.
///
/// Why: this runs on the launch path, twice per managed session (preparation
/// and spawn). The doctor's 10-second budget is for a report an operator is
/// waiting on; a launch cannot pay that, and the fail-safe answer (`false` →
/// auto memory stays on) is cheap to be wrong about.
/// What: 1.5 s, the whole budget for the single `memory.health` round trip.
/// Test: `unreachable_socket_answers_false`.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The trusty-memory health method both probes call.
const HEALTH_METHOD: &str = "memory.health";

/// What one reachability probe observed about trusty-memory (#7685).
///
/// Why: "is trusty-memory the memory" has one yes and several different noes,
/// and an operator reading `tm doctor`'s `auto_memory` row needs to tell them
/// apart — a daemon that is down is restarted, a wedged one is sampled first
/// (#4001). The launch path only needs the yes/no, via [`Self::is_reachable`].
/// What: `Reachable` only when the daemon answered with a healthy status;
/// `Unhealthy` when it answered with any other status (or none it could read);
/// `Refused` when it answered with a JSON-RPC error; `Unreachable` when nothing
/// answered within [`PROBE_TIMEOUT`].
/// Test: `reachable_when_the_daemon_reports_ok`,
/// `wedged_daemon_is_not_reachable`, `degraded_daemon_is_not_reachable`,
/// `malformed_health_body_is_not_reachable`,
/// `health_body_without_a_status_is_not_reachable`,
/// `refused_health_call_is_not_reachable`,
/// `unreachable_socket_answers_false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryReachability {
    /// Answered, and reported `status: "ok"`.
    Reachable,
    /// Answered, but with a status that is not `"ok"`.
    Unhealthy(MemoryHealthStatus),
    /// Answered the health call with a JSON-RPC error, carrying its code.
    Refused(i64),
    /// Nothing answered: a dial failure, a transport error, or a timeout.
    Unreachable,
}

impl MemoryReachability {
    /// Whether trusty-memory can be relied on as the memory.
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }

    /// One clause naming what the probe saw, for an operator-facing row.
    pub fn describe(&self) -> String {
        match self {
            Self::Reachable => "trusty-memory is healthy".to_string(),
            Self::Unhealthy(MemoryHealthStatus::Wedged) => {
                "trusty-memory is answering but reports its worker pool WEDGED (writes are not \
                 progressing)"
                    .to_string()
            }
            Self::Unhealthy(MemoryHealthStatus::Degraded) => {
                "trusty-memory is answering but reports itself DEGRADED".to_string()
            }
            Self::Unhealthy(other) => {
                format!("trusty-memory is answering but reports no healthy status ({other:?})")
            }
            Self::Refused(code) => {
                format!("trusty-memory refused its health call (code {code})")
            }
            Self::Unreachable => "trusty-memory is not answering (unreachable)".to_string(),
        }
    }
}

/// Probe the host's trusty-memory and classify what answered.
///
/// Why: the one implementation of "is trusty-memory available" that the launch
/// path, `tm doctor`'s `auto_memory` row and `tm doctor --fix` all consult, so a
/// session and the check that grades it can never disagree about which state
/// they are in.
/// What: resolves the daemon socket via
/// `trusty_common::memory_rpc::resolve_memory_socket_or_unreachable` — which
/// always yields a path, so an absent daemon simply fails to dial — and hands it
/// to [`probe_socket_reachability`].
/// Test: through [`probe_socket_reachability`].
pub async fn probe_memory_reachability() -> MemoryReachability {
    let socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
    probe_socket_reachability(&socket).await
}

/// [`probe_memory_reachability`] against an explicit socket.
///
/// Why: the resolver reads the host's own daemon layout, which a test cannot
/// point anywhere; this seam is what lets the stub daemon answer instead.
/// What: one `memory.health` call bounded by [`PROBE_TIMEOUT`]. The answer's
/// `status` is read through the shared [`MemoryHealthStatus`]; only `Ok` is
/// [`MemoryReachability::Reachable`] (#7685: `"wedged"`, `"degraded"`, an unknown
/// value, a missing field and an unreadable body all fail closed, toward keeping
/// auto memory on). A JSON-RPC error is `Refused`; a transport failure or a
/// timeout is `Unreachable`.
/// Test: `reachable_when_the_daemon_reports_ok`,
/// `wedged_daemon_is_not_reachable`, `degraded_daemon_is_not_reachable`,
/// `malformed_health_body_is_not_reachable`,
/// `health_body_without_a_status_is_not_reachable`,
/// `refused_health_call_is_not_reachable`,
/// `unreachable_socket_answers_false`.
pub async fn probe_socket_reachability(socket: &std::path::Path) -> MemoryReachability {
    match trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        HEALTH_METHOD,
        serde_json::json!({}),
        PROBE_TIMEOUT,
    )
    .await
    {
        // #7685: an answer is not health — a wedged pool answers too.
        Ok(body) => match MemoryHealthStatus::from_health_body(&body) {
            MemoryHealthStatus::Ok => MemoryReachability::Reachable,
            other => MemoryReachability::Unhealthy(other),
        },
        Err(e) => match e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>() {
            Some(rpc) => MemoryReachability::Refused(rpc.code),
            None => MemoryReachability::Unreachable,
        },
    }
}

/// [`probe_memory_reachability`] as a boolean, for a synchronous caller.
///
/// Why: `prepare_session*` and `RuntimeAdapter::spawn` are synchronous and are
/// reached from BOTH async handlers (the daemon's spawn routes, the HTTP client)
/// and plain sync callers (`standalone::load`, `deploy_validate`). Threading an
/// awaited bool to all of them would turn a two-line behaviour change into a
/// cascade through seven unrelated call chains; the launch path already makes
/// blocking daemon-touching calls in exactly this position (`session_mcp_env`
/// touches trusty-search, `claude_supports_native_output_style` spawns
/// `claude`). See #7685.
/// What: runs the probe on its own scoped thread with a private current-thread
/// runtime, so it is correct whether or not the caller already sits inside a
/// tokio runtime (`Handle::block_on` would panic there). Any failure to build
/// the runtime, and a panicking probe, both answer `false` — the fail-safe
/// direction, since `false` leaves auto memory ON.
/// Test: `blocking_probe_answers_inside_a_runtime`.
pub fn probe_memory_reachable_blocking() -> bool {
    block_on_probe(async { probe_memory_reachability().await.is_reachable() })
}

#[cfg(test)]
thread_local! {
    /// How many adapter spawns on this thread had no resolved reachability (#7685).
    ///
    /// Why: a launch path that throws away the reachability its preparation
    /// resolved makes the runtime adapter probe a second time, and that is
    /// invisible in the session it produces. Counting the adapter's own
    /// unresolved reads lets a test drive a real launch path and assert the
    /// adapter reused the prepared answer. Thread local, so parallel tests cannot
    /// disturb the count.
    /// What: incremented by `ClaudeCodeAdapter::spawn` when it was built with
    /// `None` and is about to probe.
    /// Test: `spawn_managed_on_main_hands_the_adapter_the_prepared_reachability`.
    pub(crate) static ADAPTER_REPROBES_ON_THIS_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The reachability a launch should act on: a value already resolved, or a probe.
///
/// Why (#7685): one launch used to answer this question twice — once in
/// `prepare_session_inner` and once again in the runtime adapter's spawn — so a
/// hung trusty-memory cost the launch two [`PROBE_TIMEOUT`] budgets instead of
/// one. Every consumer now asks through here with whatever the caller already
/// resolved, so threading a value UP the call chain is the only thing needed to
/// collapse the second probe.
/// What: `Some(value)` is taken verbatim and no socket is dialled; `None` runs
/// [`probe_memory_reachable_blocking`].
/// Test: `resolved_reachability_skips_the_probe`,
/// `unresolved_reachability_runs_the_probe`.
pub fn resolve_memory_reachable(resolved: Option<bool>) -> bool {
    resolve_memory_reachable_with(resolved, probe_memory_reachable_blocking)
}

/// [`resolve_memory_reachable`] with the probe supplied.
///
/// Why: the seam a test counts. Asserting "the adapter did not probe again" needs
/// the probe itself to be observable, and the real one dials a host socket no
/// test can point anywhere.
/// What: `resolved.unwrap_or_else(probe)` — nothing else, so the production
/// entry point above cannot drift from what the test drives.
/// Test: `resolved_reachability_skips_the_probe`,
/// `unresolved_reachability_runs_the_probe`.
pub(crate) fn resolve_memory_reachable_with(
    resolved: Option<bool>,
    probe: impl FnOnce() -> bool,
) -> bool {
    resolved.unwrap_or_else(probe)
}

/// Drive one reachability future to completion from synchronous code.
///
/// Why: see [`probe_memory_reachable_blocking`]. Isolating the bridge keeps the
/// unsafe-to-get-wrong part (not touching the caller's runtime) in one place,
/// and lets a test drive a socket-pinned probe through the identical mechanism.
/// What: a scoped thread with its own current-thread runtime. A runtime that
/// cannot be built, and a probe that panics, both answer `false` — the fail-safe
/// direction, since `false` leaves auto memory ON. The panic is contained by the
/// explicit `join`: `std::thread::scope` re-raises only the panics of threads it
/// had to join itself.
/// Test: `blocking_probe_answers_inside_a_runtime`,
/// `a_panicking_probe_answers_false_without_propagating`.
fn block_on_probe<F>(probe: F) -> bool
where
    F: std::future::Future<Output = bool> + Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::warn!(
                            "could not build a probe runtime for the trusty-memory \
                             reachability check; assuming unreachable: {err}"
                        );
                        return false;
                    }
                };
                runtime.block_on(probe)
            })
            .join()
            .unwrap_or(false)
    })
}

#[cfg(test)]
#[path = "memory_reachable_tests.rs"]
mod tests;
