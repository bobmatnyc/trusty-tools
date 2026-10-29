//! Is trusty-memory available right now? (#7685)
//!
//! Why: `tm doctor`'s `auto_memory` row reports whether trusty-memory is the
//! memory a session can use. Claude Code's own auto memory is off for every tm
//! session with no fallback (#8436, Bob Db0 Q5a 2026-10-14), so a trusty-memory
//! that does not answer leaves a session with NO memory until it does — the
//! fact the row has to name. No launch path consults this module any more: the
//! 2026-09-12 fallback that made launches probe it is retired.
//! What: [`probe_memory_reachability`] issues ONE `memory.health` call against
//! the derived socket and classifies the answer as a [`MemoryReachability`].
//! Only a daemon that answers AND reports `status: "ok"` is reachable — a wedged
//! or degraded daemon, an unreadable body and a refused call are all named
//! apart for the operator.
//!
//! This is deliberately NOT [`crate::daemon::doctor`]'s `probe_health`: that one
//! retries three times for the `memory` row. This row needs one fast answer —
//! see [`PROBE_TIMEOUT`].
//! Test: `core::memory_reachable::tests`.

use std::time::Duration;

use crate::core::memory_checked::protocol_mismatch;
use trusty_common::memory_rpc::MemoryHealthStatus;

/// How long one reachability probe may take.
///
/// Why: the `auto_memory` row runs beside the doctor's own `memory` row, which
/// already retries; this second look only has to classify the daemon, not wait
/// it out.
/// What: 1.5 s, the whole budget for the single `memory.health` round trip.
/// Test: `unreachable_socket_answers_false`.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The trusty-memory health method the probe calls.
const HEALTH_METHOD: &str = "memory.health";

/// What one reachability probe observed about trusty-memory (#7685).
///
/// Why: "is trusty-memory the memory" has one yes and several different noes,
/// and an operator reading `tm doctor`'s `auto_memory` row needs to tell them
/// apart — a daemon that is down is restarted, a wedged one is sampled first
/// (#4001). [`Self::is_reachable`] is the yes/no.
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
    /// A daemon answered but speaks a protocol this client refuses (#9538); the
    /// text is the named `MemoryProtocolError`.
    ProtocolMismatch(String),
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
                "trusty-memory is answering but reports its worker pool WEDGED (its oldest \
                 in-flight operation has outlived its bound)"
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
            Self::ProtocolMismatch(reason) => format!("trusty-memory is DEGRADED: {reason}"),
        }
    }
}

/// Probe the host's trusty-memory and classify what answered.
///
/// Why: the one implementation of "is trusty-memory available" that `tm
/// doctor`'s `auto_memory` row consults.
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
/// value, a missing field and an unreadable body all fail closed — none of them
/// reads as healthy). A JSON-RPC error is `Refused`; a transport failure or a
/// timeout is `Unreachable`.
/// Test: `reachable_when_the_daemon_reports_ok`,
/// `wedged_daemon_is_not_reachable`, `degraded_daemon_is_not_reachable`,
/// `malformed_health_body_is_not_reachable`,
/// `health_body_without_a_status_is_not_reachable`,
/// `refused_health_call_is_not_reachable`,
/// `unreachable_socket_answers_false`.
pub async fn probe_socket_reachability(socket: &std::path::Path) -> MemoryReachability {
    match crate::core::memory_checked::call_with_timeout(
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
        Err(e) if protocol_mismatch(&e).is_some() => {
            MemoryReachability::ProtocolMismatch(format!("{e:#}"))
        }
        Err(e) => match e.downcast_ref::<trusty_common::memory_rpc::MemoryRpcError>() {
            Some(rpc) => MemoryReachability::Refused(rpc.code),
            None => MemoryReachability::Unreachable,
        },
    }
}

#[cfg(test)]
#[path = "memory_reachable_tests.rs"]
mod tests;
