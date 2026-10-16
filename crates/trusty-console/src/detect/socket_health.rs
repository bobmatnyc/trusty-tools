//! One `<domain>.health` dial over a daemon's Unix socket, shared by the
//! connectors that detect a socket-only daemon (#6288 PR 1b).
//!
//! Why: the trusty-memory connector (#6286, #6356) and the trusty-mpm
//! connector ask the same question the same way — one framed request with an
//! explicit `"params": {}`, three verdicts — and a second copy of that probe is
//! where the two would drift. The #6356 lesson lives here once: an answer that
//! is not health is `Unhealthy`, not `Silent`.
//! What: [`probe_health`] and the [`ProbeOutcome`] it returns.
//! Test: `crate::detect::memory::tests::*` and `crate::detect::mpm::tests::*`
//! drive it against a socket each test binds.

use std::path::Path;
use std::time::Duration;

/// The `result` half of a health response, as far as the console reads it.
///
/// Only `version` is consumed — the card renders it. `status` is deserialised
/// too so a body that carries neither is refused as not-a-health-envelope
/// rather than silently rendering a versionless Running card.
#[derive(Debug, serde::Deserialize)]
pub(super) struct HealthEnvelope {
    /// `"ok"` or `"degraded"`. Presence is what makes this a health answer.
    #[allow(dead_code)]
    status: String,
    /// The daemon's own version, rendered on the service card.
    pub(super) version: Option<String>,
}

/// What one health dial observed.
///
/// Why (#6356): "the daemon did not answer" and "the daemon answered, but not
/// with health" are different facts about a machine. A daemon that answers at
/// all is running; only the first case is silence.
pub(super) enum ProbeOutcome {
    /// The daemon answered with a readable health envelope.
    Healthy(HealthEnvelope),
    /// The daemon answered, but not with health. Carries the operator-facing
    /// reason, which becomes the card's hint.
    Unhealthy(String),
    /// Nothing answered - no socket, no listener, or the dial timed out.
    Silent,
}

/// Dial `method` on `socket` and report what came back.
///
/// Why a dedicated thread: `ServiceConnector::detect` is synchronous — the
/// poller calls it inside `spawn_blocking` — and the UDS client is async.
/// Building a runtime and blocking on it from inside another runtime's worker
/// panics; a thread of its own with a current-thread runtime is safe from any
/// caller.
///
/// What: one `send_framed_request` bounded by `timeout`, carrying
/// `"params": {}` (#6356: a struct-bound health method refuses a null params),
/// then a JSON-RPC envelope check. `service` names the daemon in the hint.
///
/// Test: `memory_connector_sends_params_so_a_strict_health_handler_answers`,
/// `memory_connector_reports_degraded_when_the_daemon_answers_with_an_error`,
/// `mpm_connector_reports_degraded_when_the_daemon_answers_with_an_error`.
pub(super) fn probe_health(
    socket: &Path,
    service: &'static str,
    method: &'static str,
    timeout: Duration,
) -> ProbeOutcome {
    let socket = socket.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name(format!("console-{service}-probe"))
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return ProbeOutcome::Silent;
            };
            rt.block_on(async {
                let request = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": method,
                    "params": {},
                });
                let sent = trusty_common::uds::send_framed_request::<
                    _,
                    trusty_common::uds::server::RpcResponse,
                >(&socket, &request, timeout)
                .await;
                let Ok(response) = sent else {
                    return ProbeOutcome::Silent;
                };
                if let Some(error) = response.error {
                    return ProbeOutcome::Unhealthy(format!(
                        "{service} answered {method} with an error (code {}): {}",
                        error.code, error.message
                    ));
                }
                match response
                    .result
                    .map(serde_json::from_value::<HealthEnvelope>)
                {
                    Some(Ok(health)) => ProbeOutcome::Healthy(health),
                    _ => ProbeOutcome::Unhealthy(format!(
                        "{service} answered {method} with a body that is not a health envelope"
                    )),
                }
            })
        });
    let Ok(handle) = spawned else {
        return ProbeOutcome::Silent;
    };
    handle.join().unwrap_or(ProbeOutcome::Silent)
}
