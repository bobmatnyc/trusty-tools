//! Is trusty-memory available right now? (#7685)
//!
//! Why: the owner ruling of 2026-09-12 makes Claude Code's own auto memory a
//! FALLBACK — trusty-memory is the memory, and auto memory stays on only when
//! trusty-memory is not available. Every write site that turns auto memory off
//! (the project-tier `autoMemoryEnabled: false` key and the
//! `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` spawn assignment) therefore needs one
//! yes/no answer, resolved the same way in every caller. A session that loses
//! both memories is the failure this module exists to prevent.
//! What: [`probe_memory_reachable`] issues ONE `memory.health` call against the
//! derived socket and answers `true` when the daemon answered at all — a
//! JSON-RPC error is the daemon refusing a call, which still proves it is there.
//! [`probe_memory_reachable_blocking`] is the same probe for the synchronous
//! launch path, run on its own short-lived thread so it is safe to call from
//! inside an async runtime. [`resolve_memory_reachable`] is what consumers
//! actually call: it takes whatever the caller already resolved and probes only
//! when nothing did, so one launch asks the daemon once rather than once per
//! write site.
//!
//! This is deliberately NOT [`crate::daemon::doctor`]'s `probe_health`: that one
//! retries three times and classifies four outcomes because it renders a doctor
//! row. A launch needs a fast, single, fail-safe gate — see [`PROBE_TIMEOUT`].
//! Test: `core::memory_reachable::tests`.

use std::time::Duration;

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

/// Whether trusty-memory answered a health call within [`PROBE_TIMEOUT`].
///
/// Why: the one implementation of "is trusty-memory available" that the launch
/// path and `tm doctor`'s `auto_memory` row both consult, so a session and the
/// check that grades it can never disagree about which state they are in.
/// What: resolves the daemon socket via
/// `trusty_common::memory_rpc::resolve_memory_socket_or_unreachable` — which
/// always yields a path, so an absent daemon simply fails to dial — and issues
/// one `memory.health` call. `Ok` is reachable; so is a JSON-RPC error, because
/// only a running daemon produces one. A transport failure or a timeout is
/// unreachable.
/// Test: `reachable_when_the_daemon_answers`,
/// `reachable_when_the_daemon_refuses_the_call`,
/// `unreachable_socket_answers_false`.
pub async fn probe_memory_reachable() -> bool {
    let socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
    probe_socket_reachable(&socket).await
}

/// [`probe_memory_reachable`] against an explicit socket.
///
/// Why: the resolver reads the host's own daemon layout, which a test cannot
/// point anywhere; this seam is what lets the stub daemon answer instead.
/// Production reaches it only through [`probe_memory_reachable`].
/// What: one `memory.health` call bounded by [`PROBE_TIMEOUT`]. `Ok` is
/// reachable; so is a JSON-RPC error, because only a running daemon produces
/// one. A transport failure or a timeout is unreachable.
/// Test: `reachable_when_the_daemon_answers`,
/// `reachable_when_the_daemon_refuses_the_call`,
/// `unreachable_socket_answers_false`.
pub async fn probe_socket_reachable(socket: &std::path::Path) -> bool {
    match trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        HEALTH_METHOD,
        serde_json::json!({}),
        PROBE_TIMEOUT,
    )
    .await
    {
        Ok(_) => true,
        // A `MemoryRpcError` is the daemon ANSWERING and refusing — it is up.
        // Anything else (dial failure, timeout) means nothing is there.
        Err(e) => e
            .downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>()
            .is_some(),
    }
}

/// [`probe_memory_reachable`] for a synchronous caller.
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
    block_on_probe(probe_memory_reachable())
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
