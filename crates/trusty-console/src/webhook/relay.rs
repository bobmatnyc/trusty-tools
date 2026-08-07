//! Relaying a spooled delivery to its target over UDS, and deciding what
//! counts as an acknowledgement.
//!
//! Why: ADR-0034 §2 permits deleting a spool entry only when "the target has
//! acknowledged the frame on the UDS response". The easy wrong answer is to
//! treat a successful `connect` — or any response at all — as success, which
//! moves the silent loss down one layer instead of removing it. This module
//! makes the ack an explicit, positively-asserted field and defaults every
//! other shape to "not acknowledged".
//!
//! What: [`UdsRelay::deliver`] builds a JSON-RPC 2.0 frame carrying the raw
//! body byte-exact plus the provenance record, sends it through
//! `trusty_common::uds::send_framed_request` (which verifies the socket's
//! `0700` directory and `0600` mode before writing), and classifies the
//! response into [`RelayOutcome`].
//!
//! Until #5089 step 4 binds the targets' listeners, the expected outcome here
//! is [`RelayOutcome::Unreachable`]. That is a first-class, durable state — the
//! entry stays pending and its attempt count grows — not an error to swallow.
//!
//! Test: `webhook/tests.rs` — `relay_*` cases run against a test-double
//! `UnixListener` bound through `bind_hardened`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use trusty_common::uds::{UdsRpcError, send_framed_request};

use super::spool::{Provenance, SpoolEntry};

/// JSON-RPC method the target implements to accept a relayed delivery.
///
/// Fixed here and consumed by #5089 step 4's listener; a mismatch is a
/// compile-time-invisible wire break, so both halves read this constant.
pub const RELAY_METHOD: &str = "webhook.deliver";

/// Default budget for one relay round trip.
///
/// GitHub's own delivery timeout is 10 s and the relay runs inside the request
/// that must beat it, so this leaves headroom for the spool write and the
/// response.
pub const DEFAULT_RELAY_TIMEOUT: Duration = Duration::from_secs(5);

/// What one relay attempt established.
///
/// Why: three states, not two. "Reached the target and it refused" and "never
/// reached the target" call for different operator action, and neither is an
/// acknowledgement. Only [`RelayOutcome::Acked`] permits deleting the entry.
/// What: `Acked` carries nothing; the other two carry a human-readable reason
/// that is written into the entry's `last_error`.
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

/// The frame console writes to the target.
///
/// Why: ADR-0034 §3 — "the relayed frame carries the raw body verbatim, the
/// original headers, the delivery GUID, and an explicit provenance record".
/// Byte-exactness is what lets a target re-verify the HMAC itself instead of
/// trusting console unconditionally.
/// What: JSON-RPC 2.0 request whose params are the spooled entry's
/// delivery-relevant fields. `body_b64` is copied, never re-encoded from a
/// parsed value.
/// Test: `relay_frame_carries_provenance_and_byte_exact_body`.
#[derive(Debug, Clone, Serialize)]
pub struct RelayFrame<'a> {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// Always [`RELAY_METHOD`].
    pub method: &'static str,
    /// Request id — the delivery GUID, so a target's logs correlate with
    /// GitHub's delivery list without a second identifier.
    pub id: &'a str,
    /// The delivery itself.
    pub params: RelayParams<'a>,
}

/// Params of a [`RelayFrame`].
#[derive(Debug, Clone, Serialize)]
pub struct RelayParams<'a> {
    /// GitHub's `X-GitHub-Delivery` GUID.
    pub delivery_id: &'a str,
    /// Which console route the delivery arrived on.
    pub source: &'a str,
    /// The `X-GitHub-Event` value.
    pub event: &'a str,
    /// Original request headers, lowercased.
    pub headers: &'a std::collections::BTreeMap<String, String>,
    /// The raw body, base64, byte-exact as received.
    pub body_b64: &'a str,
    /// What console verified, and with which key.
    pub provenance: &'a Provenance,
    /// When console accepted the delivery.
    pub received_at_unix_ms: u64,
    /// How many attempts have already failed, so a target can distinguish a
    /// first delivery from a retry.
    pub attempts: u32,
}

impl<'a> RelayFrame<'a> {
    /// Build a frame from a spooled entry, borrowing rather than copying the
    /// body so it cannot be transformed on the way through.
    pub fn from_entry(entry: &'a SpoolEntry) -> Self {
        Self {
            jsonrpc: "2.0",
            method: RELAY_METHOD,
            id: &entry.delivery_id,
            params: RelayParams {
                delivery_id: &entry.delivery_id,
                source: &entry.source,
                event: &entry.event,
                headers: &entry.headers,
                body_b64: &entry.body_b64,
                provenance: &entry.provenance,
                received_at_unix_ms: entry.received_at_unix_ms,
                attempts: entry.attempts,
            },
        }
    }
}

/// The target's answer.
///
/// `ack` is `#[serde(default)]` on purpose: a target that answers with a result
/// object lacking the field has NOT acknowledged, and the default must be the
/// safe direction.
#[derive(Debug, Clone, Deserialize)]
struct RelayResponse {
    #[serde(default)]
    result: Option<RelayResult>,
    #[serde(default)]
    error: Option<RelayRpcError>,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayResult {
    #[serde(default)]
    ack: bool,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayRpcError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

/// Console's UDS client for one target.
///
/// Why: spawn-on-demand is #5089 step 4's job, so this dials an existing socket
/// and reports [`RelayOutcome::Unreachable`] when nothing is listening — the
/// state every delivery is in until that step lands.
/// What: a socket path plus a timeout. Cheap to clone; opens a fresh connection
/// per delivery, matching every other UDS client in this workspace.
/// Test: `relay_*` cases in `webhook/tests.rs`.
#[derive(Debug, Clone)]
pub struct UdsRelay {
    socket: PathBuf,
    timeout: Duration,
}

impl UdsRelay {
    /// Target `socket` with [`DEFAULT_RELAY_TIMEOUT`].
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: DEFAULT_RELAY_TIMEOUT,
        }
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

    /// Send one delivery and classify the answer.
    ///
    /// Why: the classification is the whole safety property. Anything other
    /// than an explicit `"ack": true` leaves the entry pending, so a target that
    /// accepts a connection and then crashes, or answers with an empty object,
    /// does not cause the delivery to be dropped.
    ///
    /// What: dials through the hardened UDS entry point, writes one frame,
    /// reads one frame, and maps: JSON-RPC `error` → [`RelayOutcome::Refused`];
    /// `result.ack == true` → [`RelayOutcome::Acked`]; any other result shape →
    /// `Refused`; any transport failure → [`RelayOutcome::Unreachable`].
    ///
    /// Never returns `Err`: every failure is a first-class outcome the caller
    /// must record durably, and an error return invites the `let _ =` this ADR
    /// forbids.
    ///
    /// Test: `relay_acked_response_is_the_only_ack`,
    /// `relay_treats_a_result_without_ack_as_refused`,
    /// `relay_treats_a_jsonrpc_error_as_refused`,
    /// `relay_reports_unreachable_when_no_listener_is_bound`,
    /// `relay_reports_unreachable_when_the_target_hangs_up_without_answering`.
    pub async fn deliver(&self, entry: &SpoolEntry) -> RelayOutcome {
        let frame = RelayFrame::from_entry(entry);
        let response: Result<RelayResponse, UdsRpcError> =
            send_framed_request(&self.socket, &frame, self.timeout).await;

        match response {
            Ok(RelayResponse {
                error: Some(err), ..
            }) => RelayOutcome::Refused {
                reason: format!(
                    "target rejected the frame: code {} — {}",
                    err.code, err.message
                ),
            },
            Ok(RelayResponse {
                result: Some(result),
                ..
            }) => {
                if result.ack {
                    RelayOutcome::Acked
                } else {
                    RelayOutcome::Refused {
                        reason: result.detail.unwrap_or_else(|| {
                            "target answered without an explicit \"ack\": true".to_string()
                        }),
                    }
                }
            }
            Ok(_) => RelayOutcome::Refused {
                reason: "target answered with neither a result nor an error".to_string(),
            },
            Err(e) => RelayOutcome::Unreachable {
                reason: format!("{e}"),
            },
        }
    }
}
