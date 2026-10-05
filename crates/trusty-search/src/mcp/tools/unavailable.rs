//! The `INDEX_UNAVAILABLE` error contract — structured daemon 503s at the MCP
//! boundary (issue #5350).
//!
//! Why: PR #5345 gave every "this index exists but cannot serve you" state a
//! machine-readable 503 body — an `error` code, an `index_id`, a `retryable`
//! flag, and on the cold-parked arm a `restore_via` hint naming the endpoint
//! that clears it. The MCP transport then formatted that body into
//! `POST <url> returned 503 Service Unavailable: {…}` and returned it as a
//! prose [`DispatchError::Transport`] string. Everything a caller would branch
//! on survived only as text inside an English sentence, so the residency and
//! vector-availability contract stopped at the HTTP boundary and never reached
//! an MCP consumer.
//!
//! #9168: the bridge now reaches the daemon over its socket, where the same
//! 503 body arrives as the refusal's `data` member; the contract is unchanged.
//!
//! What: [`classify_unavailable`] — the single place a daemon 503 becomes a
//! structured error — plus the JSON-RPC code and the `tools/call` envelope
//! wrapper. Deliberately shaped as [`super::not_ready`]'s twin: that module
//! does exactly this for 404, and one error convention on the tool surface is
//! worth more than a second bespoke one.
//!
//! The daemon body passes through VERBATIM. This module adds `error_code` and
//! `http_status` and changes nothing else, and it never synthesises a field the
//! daemon did not send — that is how a representation layer starts lying about
//! the state it is supposed to relay. Since #5917 every `index_corpus_unavailable`
//! body carries `retryable` in its own right (its two producers previously
//! disagreed: the open-failure arm sent only `transient`, the read-failure arm
//! only `retryable`), so no arm depends on this module filling the gap. #5349
//! tracks the availability model itself and is deliberately untouched here.
//!
//! Test: `mcp/tools/tests_unavailable.rs`.

use serde_json::Value;

use super::types::DispatchError;
use crate::service::daemon_client::DaemonCallError;

/// Application-level JSON-RPC error code for "the daemon answered 503 with a
/// structured availability verdict" (issue #5350).
///
/// Why: sits in the JSON-RPC 2.0 server-reserved range (`-32099` ..= `-32000`),
/// one slot below [`super::INDEX_NOT_READY_CODE`], so an orchestrator branches
/// on the numeric code alone. Distinct from `INDEX_NOT_READY` because the two
/// describe different states: not-ready means the index was never built, this
/// means it exists and is momentarily unable to answer.
/// What: emitted on bare-method invocations; the `tools/call` form carries the
/// same condition as `_meta.error_code = "INDEX_UNAVAILABLE"`.
/// Test: `bare_method_search_on_cold_parked_index_returns_structured_data`.
pub const INDEX_UNAVAILABLE_CODE: i32 = -32012;

/// Machine-readable discriminator carried in `_meta.error_code` / `error.data`.
///
/// The daemon's own narrower code (`index_not_resident`, `vector_unavailable`,
/// `index_restore_failed`, …) stays available beside it under `error`.
pub const INDEX_UNAVAILABLE: &str = "INDEX_UNAVAILABLE";

/// Turn a daemon unavailable refusal carrying its 503 body into a structured
/// dispatch error.
///
/// Why: this is the fix for #5350, carried onto the socket by #9168. The
/// daemon renders every availability verdict as an `unavailable` (or
/// `permanently unavailable`) refusal whose `data` is the 503 body verbatim
/// (`service::rpc::error::rpc_error_from_http`), so the fields a caller
/// branches on — `index_id`, `retryable`, `restore_via`, `reason`,
/// `transient`, `stages` — arrive as data, not prose.
///
/// What: returns `Some(DispatchError::IndexUnavailable { .. })` only when the
/// refusal's code is one of the two unavailable codes AND its `data` is a JSON
/// object with a string `error` field — the shape every availability verdict in
/// `service/server/degraded.rs` emits. Anything else returns `None` and the
/// caller falls through to its existing error unchanged, so a refusal without
/// `data`, or any other code, behaves exactly as before. The payload adds
/// `error_code` and `http_status: 503` (the status the daemon's verdict maps
/// to) and changes nothing else. It has no fallible step that could turn a
/// failure into a success: every path yields `None` or an `Err`.
///
/// Test: `classify_unavailable_ignores_non_503_and_unstructured_bodies`.
pub(super) fn classify_unavailable(e: &DaemonCallError) -> Option<DispatchError> {
    if !e.is_unavailable() {
        return None;
    }
    let obj = e.data()?.as_object()?;
    let code = obj.get("error").and_then(Value::as_str)?;

    let mut payload = obj.clone();
    payload.insert("error_code".to_owned(), Value::from(INDEX_UNAVAILABLE));
    payload.insert("http_status".to_owned(), Value::from(503u16));

    Some(DispatchError::IndexUnavailable {
        message: unavailable_message(code, obj),
        payload: Value::Object(payload),
    })
}

/// Human-readable text shown to the model alongside the structured payload.
///
/// Why: the model reads `content[]` prose first, so the retry decision and the
/// remedy must be legible there too — the structured payload is for branching,
/// not for reading.
/// What: prefers the daemon's own `message` (every `degraded.rs` arm writes one
/// that already names the state and the remedy) and otherwise composes one from
/// the fields that are always present. Appends `restore_via` when the daemon
/// named an endpoint that clears the state, because that hint is the single
/// most actionable field in the body and is easy to miss inside `_meta`.
/// Test: `unavailable_message_carries_the_restore_hint`.
fn unavailable_message(code: &str, obj: &serde_json::Map<String, Value>) -> String {
    let mut message = match obj.get("message").and_then(Value::as_str) {
        Some(m) => m.to_owned(),
        None => {
            let id = obj
                .get("index_id")
                .or_else(|| obj.get("index"))
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            format!(
                "The daemon cannot serve this request against index '{id}': {code} \
                 (unavailable). See the structured payload for the full verdict."
            )
        }
    };
    if let Some(via) = obj.get("restore_via").and_then(Value::as_str) {
        message.push_str(&format!(" Clear this state with: {via}."));
    }
    message
}

/// Wrap an `INDEX_UNAVAILABLE` failure in MCP's structured tool-error envelope.
///
/// Why: `tools/call` signals failures in band with `isError: true`, and putting
/// the payload under `_meta` — exactly as `STAGE_NOT_READY` and
/// `INDEX_NOT_READY` do — means a client that understands one understands this.
/// What: returns `{isError: true, content: [text], _meta: <payload>}`.
/// Test: `tools_call_search_on_cold_parked_index_returns_structured_meta`.
pub(super) fn wrap_index_unavailable_error(message: &str, payload: &Value) -> Value {
    serde_json::json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": message,
        }],
        "_meta": payload,
    })
}
