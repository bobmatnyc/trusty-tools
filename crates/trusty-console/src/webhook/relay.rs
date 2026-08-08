//! Sending a spooled delivery to its target, and deciding what counts as an
//! acknowledgement.
//!
//! Why: ADR-0034 §2 permits deleting a spool entry only when "the target has
//! acknowledged the frame on the UDS response". The easy wrong answer is to
//! treat a successful `connect` — or any response at all — as success, which
//! moves the silent loss down one layer instead of removing it.
//!
//! What: [`UdsRelay::deliver`] builds a `trusty_common::webhook_relay::RelayFrame`
//! carrying the raw body byte-exact plus the provenance record, sends it through
//! `trusty_common::uds::send_framed_request` (which verifies the socket's `0700`
//! directory and `0600` mode before writing), and classifies the answer into
//! [`RelayOutcome`]. The wire types themselves live in `trusty-common` because
//! step 4's receivers are `trusty-review` and `trusty-analyze`, which cannot
//! depend on the console.
//!
//! #5182 bound those listeners and added the spawn that precedes the dial: a
//! relay first asks [`super::spawn::TargetSupervisor`] to make sure something is
//! serving the socket, because ADR-0034 §1 requires the target to exist without
//! running resident. A supervisor failure is still [`RelayOutcome::Unreachable`]
//! — a first-class durable state, the entry stays pending and its attempt count
//! grows, not an error to swallow.
//!
//! Test: `webhook/tests.rs` — `relay_*` cases run against a test-double
//! `UnixListener` bound through `bind_hardened`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use trusty_common::uds::{UdsRpcError, send_framed_request};
use trusty_common::webhook_relay::{RelayFrame, RelayResponse};

use super::spawn::SharedSupervisor;
use super::spool::SpoolEntry;

/// Default budget for one relay round trip.
///
/// GitHub's own delivery timeout is 10 s and the relay runs inside the request
/// that must beat it, so this leaves headroom for the spool write and the
/// response. It is also the grace period the retry sweep gives a freshly
/// spooled entry before considering it its own to relay — see
/// [`super::BackoffPolicy`].
pub const DEFAULT_RELAY_TIMEOUT: Duration = Duration::from_secs(5);

/// What one relay attempt established.
///
/// Why: three states, not two. "Reached the target and it refused" and "never
/// reached the target" call for different operator action, and neither is an
/// acknowledgement. Only [`RelayOutcome::Acked`] permits deleting the entry.
/// What: `Acked` carries nothing; the other two carry a human-readable reason
/// written into the entry's `last_error`.
/// Test: `relay_*` cases in `webhook/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayOutcome {
    /// The target answered with an explicit `"ack": true`.
    Acked,
    /// The target answered, but not with an acknowledgement — a JSON-RPC
    /// error, `"ack": false`, or a result with no `ack` field at all.
    Refused {
        /// Why the target's answer was not an ack.
        reason: String,
    },
    /// The target could not be reached, did not answer, or answered
    /// unintelligibly.
    Unreachable {
        /// Transport-level reason.
        reason: String,
    },
}

impl RelayOutcome {
    /// True only for [`RelayOutcome::Acked`].
    ///
    /// The single predicate the deletion path is allowed to consult, so no call
    /// site can spell "not unreachable" and delete a refused entry.
    pub fn is_acked(&self) -> bool {
        matches!(self, RelayOutcome::Acked)
    }

    /// The reason string, or `"acknowledged"`.
    pub fn reason(&self) -> &str {
        match self {
            RelayOutcome::Acked => "acknowledged",
            RelayOutcome::Refused { reason } | RelayOutcome::Unreachable { reason } => reason,
        }
    }
}

/// Console's UDS client for one target.
///
/// Why: the dial half of ADR-0034's relay. Since #5182 it can also START the
/// target: with a supervisor attached it calls `ensure_running` before writing
/// the frame, which is what lets the target stay non-resident. Without one it
/// dials whatever is already at the path, which is what the tests and any
/// externally-managed deployment want.
/// What: a socket path, a timeout, and an optional supervisor. Cheap to clone;
/// opens a fresh connection per delivery, matching every other UDS client in
/// this workspace.
/// Test: `relay_*` cases in `webhook/tests.rs`.
#[derive(Debug, Clone)]
pub struct UdsRelay {
    socket: PathBuf,
    timeout: Duration,
    source: String,
    supervisor: Option<SharedSupervisor>,
}

impl UdsRelay {
    /// Target `socket` with [`DEFAULT_RELAY_TIMEOUT`] and no supervision.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: DEFAULT_RELAY_TIMEOUT,
            source: String::new(),
            supervisor: None,
        }
    }

    /// Start the target on demand, under `supervisor`, keyed by `source`.
    ///
    /// Test: `relay_with_a_supervisor_still_acks_against_a_live_target`,
    /// `spawn_adopts_a_socket_that_is_already_served`.
    pub fn with_supervisor(
        mut self,
        source: impl Into<String>,
        supervisor: SharedSupervisor,
    ) -> Self {
        self.source = source.into();
        self.supervisor = Some(supervisor);
        self
    }

    /// Override the round-trip budget.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Socket this relay dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Round-trip budget, which is also the sweep's hands-off grace period for
    /// an entry the request path may still be relaying.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Send one delivery and classify the answer.
    ///
    /// Why: the classification is the whole safety property. Anything other
    /// than an explicit `"ack": true` leaves the entry pending, so a target that
    /// accepts a connection and then crashes, or answers with an empty object,
    /// does not cause the delivery to be dropped.
    ///
    /// What: dials through the hardened UDS entry point, writes one frame,
    /// reads one frame, and defers the ack decision to
    /// `RelayResponse::is_ack` so both halves of the contract agree on it. A
    /// JSON-RPC error or a non-ack result is [`RelayOutcome::Refused`]; any
    /// transport failure is [`RelayOutcome::Unreachable`].
    ///
    /// Never returns `Err`: every failure is a first-class outcome the caller
    /// must record durably, and an error return invites the `let _ =` ADR-0034
    /// §2 forbids.
    ///
    /// Test: `relay_acked_response_is_the_only_ack`,
    /// `relay_treats_a_result_without_ack_as_refused`,
    /// `relay_treats_a_jsonrpc_error_as_refused`,
    /// `relay_reports_unreachable_when_no_listener_is_bound`,
    /// `relay_reports_unreachable_when_the_target_hangs_up_without_answering`.
    pub async fn deliver(&self, entry: &SpoolEntry) -> RelayOutcome {
        let frame = RelayFrame::new(
            &entry.delivery_id,
            &entry.source,
            &entry.event,
            &entry.headers,
            &entry.body_b64,
            &entry.provenance,
            entry.received_at_unix_ms,
            entry.attempts,
        );
        // #5182: make sure something is serving the socket before writing to
        // it. A supervisor failure is a transport failure — never an ack — so
        // the entry stays pending and the sweep retries.
        if let Some(supervisor) = &self.supervisor
            && let Err(e) = supervisor.ensure_running(&self.source, &self.socket).await
        {
            return RelayOutcome::Unreachable {
                reason: format!("could not start the {} target: {e}", self.source),
            };
        }

        let response: Result<RelayResponse, UdsRpcError> =
            send_framed_request(&self.socket, &frame, self.timeout).await;

        match response {
            Ok(resp) if resp.is_ack() => RelayOutcome::Acked,
            Ok(resp) => RelayOutcome::Refused {
                reason: refusal_reason(&resp),
            },
            Err(e) => RelayOutcome::Unreachable {
                reason: format!("{e}"),
            },
        }
    }
}

/// Turn a non-ack response into the string stored in the entry's `last_error`.
///
/// Prefers the target's own words — a JSON-RPC error message, then a result
/// `detail` — so an operator reading the durable record sees the target's
/// diagnosis rather than console's paraphrase of it.
fn refusal_reason(resp: &RelayResponse) -> String {
    if let Some(err) = &resp.error {
        return format!(
            "target rejected the frame: code {} — {}",
            err.code, err.message
        );
    }
    match resp.result.as_ref().and_then(|r| r.detail.clone()) {
        Some(detail) => detail,
        None if resp.result.is_some() => {
            "target answered without an explicit \"ack\": true".to_string()
        }
        None => "target answered with neither a result nor an error".to_string(),
    }
}
