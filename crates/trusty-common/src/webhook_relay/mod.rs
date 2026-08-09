//! The console→target webhook relay wire contract (ADR-0034 §3, #5089 step 3).
//!
//! Why: the sender is `trusty-console` and the receivers are `trusty-review`
//! and `trusty-analyze`, neither of which can depend on the console. A method
//! name or field name that lives in only one half is not a contract — it is two
//! copies waiting to drift, which is what the common-entry-point rule exists to
//! stop. Both halves read the types here.
//!
//! What: [`RELAY_METHOD`]; the borrowed [`RelayFrame`] the sender serialises
//! (borrowed so the raw body is forwarded rather than re-encoded — the HMAC
//! covers the literal bytes); the owned [`RelayRequest`] the receiver
//! deserialises; and [`RelayResponse`], whose `ack` field is the only thing that
//! licenses the sender to delete its spool entry.
//!
//! 🔴 [`RelayResult::ack`] is `#[serde(default)]`, so a receiver that answers
//! with a result object lacking the field has NOT acknowledged. The default has
//! to be the safe direction: treating any answer as success is the silent loss
//! ADR-0034 §2 forbids, one layer below the one it is fixing.
//!
//! Test: `tests` below cover the sender→receiver round trip over the real
//! types, the ack/refuse constructors, and the missing-`ack` default.
//! `trusty-console`'s `webhook/tests.rs` exercises the frame over a real socket.
//!
//! #5182 added the receive half. [`inbox`] is the durable owner a receiver
//! writes to, [`serve`] is the listener that answers the frame, and the socket
//! paths both halves use are [`review_socket_path`] / [`analyze_socket_path`]
//! here rather than a string literal per crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod claim;
pub mod drain;
pub mod inbox;
pub mod listener;
pub mod retry;
pub mod serve;

/// The receive half's tests (#5182), kept out of this file so the wire contract
/// stays readable and the 500-SLOC production cap is not spent on fixtures.
#[cfg(test)]
#[path = "tests.rs"]
mod receive_tests;

/// The drain half's tests (#5192), in their own file for the same reason.
#[cfg(test)]
#[path = "drain_tests.rs"]
mod drain_tests;

pub use claim::{Claim, ClaimError, ClaimOutcome, entry_is_still_linked};
pub use drain::{
    DeliveryProcessor, Disposition, DrainFailure, DrainPolicy, DrainReport, FailureOutcome,
    ProcessFailure, drain_once,
};
pub use inbox::{Inbox, InboxError, Ownership, held_count};
pub use listener::{
    DEFAULT_DRAIN_INTERVAL, ListenerError, WebhookListener, run_until_signal,
    run_until_signal_with_processor,
};
pub use retry::{
    AttemptRecord, DEFAULT_MAX_ATTEMPTS, PROCESSED_DIR_NAME, PROCESSED_RETENTION,
    QUARANTINE_DIR_NAME, is_processed, mark_processed, quarantine_dir, quarantined_count,
};
pub use serve::{
    DeliverySink, LISTENER_SHUTDOWN_FLUSH, ServeOptions, Served, SinkRejection, dispatch_frame,
    serve_until,
};

/// JSON-RPC method a target implements to accept a relayed delivery.
pub const RELAY_METHOD: &str = "webhook.deliver";

/// Route segment and supervisor key for `trusty-review`.
pub const REVIEW_SOURCE: &str = "review";

/// Route segment and supervisor key for `trusty-analyze`.
pub const ANALYZE_SOURCE: &str = "analyze";

/// Socket filename `trusty-review` binds and console dials.
pub const REVIEW_SOCKET_FILE: &str = "trusty-review-webhook.sock";

/// Socket filename `trusty-analyze` binds and console dials.
pub const ANALYZE_SOCKET_FILE: &str = "trusty-analyze-webhook.sock";

/// Where `trusty-review` binds its webhook listener.
///
/// Why: before #5182 the path existed only as a literal in
/// `trusty-console`'s `WebhookIngress::from_env`, and nothing bound it. A
/// receiver spelling the same literal a second time is two copies of a path
/// that must agree byte-for-byte or the relay silently never arrives — the
/// common-entry-point rule applied to a filename.
/// What: [`crate::uds::scratch_socket_dir`] (a `0700`, uid-owned directory)
/// joined with [`REVIEW_SOCKET_FILE`].
/// Test: `socket_paths_live_in_the_hardened_scratch_dir`.
#[cfg(feature = "uds")]
pub fn review_socket_path() -> std::path::PathBuf {
    crate::uds::scratch_socket_dir().join(REVIEW_SOCKET_FILE)
}

/// Where `trusty-analyze` binds its webhook listener. See
/// [`review_socket_path`].
#[cfg(feature = "uds")]
pub fn analyze_socket_path() -> std::path::PathBuf {
    crate::uds::scratch_socket_dir().join(ANALYZE_SOCKET_FILE)
}

/// Directory name each receiver's inbox occupies under its data directory.
pub const INBOX_DIR_NAME: &str = "webhook-inbox";

/// Crate whose data directory holds a `{source}`'s inbox.
///
/// Test: `inbox_app_names_map_each_source_to_its_owning_crate`.
pub fn inbox_app_name(source: &str) -> Option<&'static str> {
    match source {
        REVIEW_SOURCE => Some("trusty-review"),
        ANALYZE_SOURCE => Some("trusty-analyze"),
        _ => None,
    }
}

/// Where a `{source}`'s receiver holds deliveries it has acknowledged.
///
/// Why: the receiver writes here and `trusty-console` reads the depth here, so
/// the path has to be one function. A delivery sitting in this directory is work
/// that arrived and is not finished; console meters it so an undrained backlog
/// does not read as healthy (#5192). A second spelling of the path would make
/// console meter a directory nobody writes to and report `Ok` forever.
/// What: `<data dir for the owning crate>/webhook-inbox`. Honours
/// `TRUSTY_DATA_DIR_OVERRIDE`, which is what keeps tests off the real one.
///
/// # Errors
///
/// When the platform data directory cannot be resolved or created.
///
/// Test: `inbox_app_names_map_each_source_to_its_owning_crate` covers the
/// mapping; the data-dir resolution itself is `resolve_data_dir`'s own.
pub fn inbox_root_for(source: &str) -> Option<anyhow::Result<std::path::PathBuf>> {
    let app = inbox_app_name(source)?;
    Some(crate::resolve_data_dir(app).map(|dir| dir.join(INBOX_DIR_NAME)))
}

/// The socket for a `{source}` route segment, or `None` when it names no
/// target.
///
/// Why: console multiplexes `POST /api/webhooks/{source}` over both targets and
/// each target needs the same mapping to know which socket is its own. One
/// function keeps the route segment, the supervisor key and the socket
/// filename from drifting apart.
/// Test: `socket_path_for_resolves_both_targets_and_refuses_others`.
#[cfg(feature = "uds")]
pub fn socket_path_for(source: &str) -> Option<std::path::PathBuf> {
    match source {
        REVIEW_SOURCE => Some(review_socket_path()),
        ANALYZE_SOURCE => Some(analyze_socket_path()),
        _ => None,
    }
}

/// JSON-RPC version string both halves send.
pub const JSONRPC_VERSION: &str = "2.0";

/// What the sender proves to the receiver about a relayed body.
///
/// Why: ADR-0034 §3 — the receiver trusts this assertion because only a
/// same-uid process could have written it through a `0600` socket. Recording
/// the algorithm and key id explicitly, rather than letting the receiver infer
/// "it came over UDS so it is fine", is what keeps the trust boundary named
/// instead of assumed.
/// What: three fields, carried verbatim in the frame and stored verbatim in the
/// sender's spool entry.
/// Test: `relay_frame_round_trips_through_the_owned_request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Algorithm the sender checked, e.g. `hmac-sha256`.
    pub algorithm: String,
    /// Which secret was used, by name — never the secret itself.
    pub key_id: String,
    /// Whether verification succeeded. Always `true` on the wire: the sender
    /// refuses an unverified delivery before it reaches the spool.
    pub verified: bool,
}

/// The frame the sender writes, borrowing every payload field.
///
/// Why: ADR-0034 §3 requires the raw body verbatim so a receiver retains the
/// option of re-verifying the HMAC independently. Borrowing rather than owning
/// makes it structurally impossible for a sender to transform the body on the
/// way through.
/// What: JSON-RPC 2.0 request; `id` is the delivery GUID so a receiver's logs
/// correlate with GitHub's delivery list without a second identifier.
/// Test: `relay_frame_round_trips_through_the_owned_request`.
#[derive(Debug, Clone, Serialize)]
pub struct RelayFrame<'a> {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: &'static str,
    /// Always [`RELAY_METHOD`].
    pub method: &'static str,
    /// The delivery GUID.
    pub id: &'a str,
    /// The delivery itself.
    pub params: RelayParams<'a>,
}

/// Params of a [`RelayFrame`].
#[derive(Debug, Clone, Serialize)]
pub struct RelayParams<'a> {
    /// GitHub's `X-GitHub-Delivery` GUID.
    pub delivery_id: &'a str,
    /// Which ingress route the delivery arrived on (`review` / `analyze`).
    pub source: &'a str,
    /// The `X-GitHub-Event` value.
    pub event: &'a str,
    /// Original request headers, lowercased.
    pub headers: &'a BTreeMap<String, String>,
    /// The raw body, base64, byte-exact as received.
    pub body_b64: &'a str,
    /// What the sender verified, and with which key.
    pub provenance: &'a Provenance,
    /// When the sender accepted the delivery.
    pub received_at_unix_ms: u64,
    /// How many relay attempts have already failed, so a receiver can tell a
    /// first delivery from a retry.
    ///
    /// 🔴 Relay is at-least-once by construction: the sender keeps the entry
    /// until an explicit ack, so a receiver that acknowledges after doing the
    /// work can still be re-sent the same `delivery_id` if the ack is lost.
    /// Receivers must deduplicate on `delivery_id`; a non-zero `attempts` is a
    /// hint, never a guarantee that the first attempt did nothing.
    pub attempts: u32,
}

impl<'a> RelayFrame<'a> {
    /// Build a frame from borrowed parts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        delivery_id: &'a str,
        source: &'a str,
        event: &'a str,
        headers: &'a BTreeMap<String, String>,
        body_b64: &'a str,
        provenance: &'a Provenance,
        received_at_unix_ms: u64,
        attempts: u32,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method: RELAY_METHOD,
            id: delivery_id,
            params: RelayParams {
                delivery_id,
                source,
                event,
                headers,
                body_b64,
                provenance,
                received_at_unix_ms,
                attempts,
            },
        }
    }
}

/// The receiver's owned view of a [`RelayFrame`].
///
/// Why: a target crate cannot deserialise into borrowed `&'a str` fields from a
/// framed read, and duplicating the field names on its side is how the two
/// halves drift. This is the same contract, owned.
/// What: mirrors [`RelayFrame`] field for field.
/// Test: `relay_frame_round_trips_through_the_owned_request`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RelayRequest {
    /// JSON-RPC version.
    pub jsonrpc: String,
    /// Method name; a receiver must reject anything but [`RELAY_METHOD`].
    pub method: String,
    /// The delivery GUID, echoed as the request id.
    pub id: String,
    /// The delivery.
    pub params: RelayDelivery,
}

/// Owned params of a [`RelayRequest`]. See [`RelayParams`] for field semantics.
///
/// `Serialize` as well as `Deserialize` since #5182: a receiver writes this
/// value verbatim into its durable [`inbox`] before acknowledging, so the
/// stored copy is the frame it was handed rather than a re-derived summary of
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayDelivery {
    /// GitHub's `X-GitHub-Delivery` GUID.
    pub delivery_id: String,
    /// Which ingress route the delivery arrived on.
    pub source: String,
    /// The `X-GitHub-Event` value.
    pub event: String,
    /// Original request headers, lowercased.
    pub headers: BTreeMap<String, String>,
    /// The raw body, base64, byte-exact as received.
    pub body_b64: String,
    /// What the sender verified.
    pub provenance: Provenance,
    /// When the sender accepted the delivery.
    pub received_at_unix_ms: u64,
    /// Failed attempts so far. See [`RelayParams::attempts`] on why this is a
    /// hint and deduplication on `delivery_id` is mandatory.
    #[serde(default)]
    pub attempts: u32,
}

/// The receiver's answer.
///
/// Why: the sender deletes a durable spool entry on the strength of this, so
/// the shape has to make "did not acknowledge" the default rather than an
/// explicit denial the receiver might forget to send.
/// What: JSON-RPC-shaped. Both fields optional; a response with neither is
/// treated as a refusal.
/// Test: `relay_response_without_ack_is_not_an_ack`, `relay_response_ack`,
/// `relay_response_refuse`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayResponse {
    /// Present on success-shaped answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RelayResult>,
    /// Present on error-shaped answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RelayRpcError>,
}

/// Result half of a [`RelayResponse`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayResult {
    /// 🔴 The single field that permits deleting the sender's spool entry.
    /// `#[serde(default)]` — absent means **not** acknowledged.
    #[serde(default)]
    pub ack: bool,
    /// Optional human-readable detail, recorded in the sender's durable entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Error half of a [`RelayResponse`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayRpcError {
    /// JSON-RPC error code.
    #[serde(default)]
    pub code: i64,
    /// Human-readable reason, recorded in the sender's durable entry.
    #[serde(default)]
    pub message: String,
}

impl RelayResponse {
    /// The receiver has taken responsibility for the delivery.
    ///
    /// Only send this once the work is durable on the receiver's side —
    /// the sender deletes its own copy the moment it arrives.
    pub fn ack() -> Self {
        Self {
            result: Some(RelayResult {
                ack: true,
                detail: None,
            }),
            error: None,
        }
    }

    /// The receiver declines; the sender keeps the entry and retries.
    pub fn refuse(code: i64, message: impl Into<String>) -> Self {
        Self {
            result: None,
            error: Some(RelayRpcError {
                code,
                message: message.into(),
            }),
        }
    }

    /// True only when the receiver explicitly acknowledged.
    ///
    /// The one predicate a deletion path may consult, so no call site can spell
    /// "no error, therefore done".
    pub fn is_ack(&self) -> bool {
        self.error.is_none() && self.result.as_ref().is_some_and(|r| r.ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            algorithm: "hmac-sha256".to_string(),
            key_id: "GITHUB_WEBHOOK_SECRET".to_string(),
            verified: true,
        }
    }

    #[test]
    fn relay_frame_round_trips_through_the_owned_request() {
        // The contract's whole point: what the console writes is exactly what a
        // target crate reads, with no per-side field list to drift.
        let headers = BTreeMap::from([("x-github-event".to_string(), "pull_request".to_string())]);
        let prov = provenance();
        let frame = RelayFrame::new(
            "abc-123",
            "review",
            "pull_request",
            &headers,
            "eyJhIjoxfQ==",
            &prov,
            1_700_000_000_000,
            2,
        );

        let bytes = serde_json::to_vec(&frame).expect("serialize frame");
        let got: RelayRequest = serde_json::from_slice(&bytes).expect("deserialize frame");

        assert_eq!(got.jsonrpc, JSONRPC_VERSION);
        assert_eq!(got.method, RELAY_METHOD);
        assert_eq!(got.id, "abc-123");
        assert_eq!(got.params.delivery_id, "abc-123");
        assert_eq!(got.params.source, "review");
        assert_eq!(got.params.event, "pull_request");
        assert_eq!(got.params.headers, headers);
        assert_eq!(got.params.body_b64, "eyJhIjoxfQ==");
        assert_eq!(got.params.provenance, prov);
        assert_eq!(got.params.received_at_unix_ms, 1_700_000_000_000);
        assert_eq!(got.params.attempts, 2);
    }

    #[test]
    fn relay_response_without_ack_is_not_an_ack() {
        // A receiver that answers with an empty result object has done nothing.
        for raw in [r#"{"result":{}}"#, r#"{}"#, r#"{"result":{"ack":false}}"#] {
            let resp: RelayResponse = serde_json::from_str(raw).expect("parse");
            assert!(!resp.is_ack(), "{raw} must not read as an acknowledgement");
        }
    }

    #[test]
    fn relay_response_ack() {
        let resp = RelayResponse::ack();
        assert!(resp.is_ack());
        let round: RelayResponse =
            serde_json::from_slice(&serde_json::to_vec(&resp).expect("ser")).expect("de");
        assert!(round.is_ack());
    }

    #[test]
    fn relay_response_refuse() {
        let resp = RelayResponse::refuse(-32000, "dedup store locked");
        assert!(!resp.is_ack());
        assert_eq!(
            resp.error.as_ref().map(|e| e.message.as_str()),
            Some("dedup store locked")
        );
    }

    #[test]
    fn relay_response_with_both_halves_is_not_an_ack() {
        // A malformed receiver that sends result AND error must not be read as
        // success just because the result happens to say so.
        let resp: RelayResponse =
            serde_json::from_str(r#"{"result":{"ack":true},"error":{"code":-1,"message":"x"}}"#)
                .expect("parse");
        assert!(!resp.is_ack());
    }
}
