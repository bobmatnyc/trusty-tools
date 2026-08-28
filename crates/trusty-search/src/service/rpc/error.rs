//! One HTTP refusal becomes one JSON-RPC error frame (#6285 slice 2).
//!
//! Why: every route this slice moves already reports its failures as an HTTP
//! status beside a JSON body. Without one conversion each registration would
//! spell its own status-to-code table, and the status a caller sees over HTTP
//! and the code it sees over the socket would be free to drift apart for the
//! same refusal.
//!
//! What: [`rpc_error_from_http`] derives the code from the status and carries
//! the body's own wording as the message.
//!
//! **Why `retryable` becomes a code.** trusty-search's index-scoped error
//! contract (`crates/trusty-search/CLAUDE.md`) makes `retryable` a field a
//! consumer branches on and states it is never absent, and
//! [`RpcError`] carries only `code` and `message` — a body field has nowhere to
//! go. So the discriminant becomes the code, which is what trusty-mpm's
//! `CODE_WORKSPACE_GONE` / `CODE_PANE_GONE` pair did for the two HTTP 422
//! classes its `x-trusty-resume-reason` header used to separate (#6288 slice 4):
//! a 503 that will clear answers [`CODE_UNAVAILABLE`] and one that never will
//! answers [`CODE_UNAVAILABLE_PERMANENT`].
//!
//! `restore_via` does NOT survive, and deliberately: it names
//! `POST /indexes/{id}/search`, a route that does not exist on this transport.
//! The retire slice replaces it with the method name that reloads a cold-parked
//! index.
//!
//! Test: `error_tests.rs`.

use trusty_common::uds::server::{RpcError, CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS};

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;

/// HTTP 404 over the socket.
///
/// The same number `trusty-analyze` (`service::events::CODE_NOT_FOUND`) and
/// `trusty-mpm` (`daemon::error::CODE_NOT_FOUND`) use for the same meaning, so
/// a later consolidation into `trusty-common` is a move rather than a
/// renumbering.
pub const CODE_NOT_FOUND: i64 = -32004;

/// HTTP 503 over the socket, for a refusal whose body says `retryable: true` —
/// a cold-parked index, a load in flight, an embedder still initialising, a
/// corpus read that failed. Waiting or retrying is a real remedy.
///
/// The same number trusty-mpm uses for its 503 class.
pub const CODE_UNAVAILABLE: i64 = -32002;

/// HTTP 503 over the socket, for a refusal whose body says `retryable: false` —
/// a permanently failed restore, or a lane the index was built without
/// (`kg_unavailable` on a `skip_kg` index). Retrying fails identically forever;
/// only operator action clears it.
///
/// A free slot around the numbers listed above: `trusty-common` holds
/// -32010/-32011 for the streaming pair and trusty-mpm holds -32001 through
/// -32009 plus -32023/-32024.
pub const CODE_UNAVAILABLE_PERMANENT: i64 = -32012;

/// The JSON-RPC code an HTTP status projects onto.
///
/// Why a function rather than a match at each call site: the read surface has
/// nine methods and four refusal classes between them, and one table is what
/// makes "the code is a pure function of what HTTP would have sent" checkable.
/// What: 400 is the caller's fault; 404 is permanent absence; 503 splits on
/// `permanent` — see [`refusal_is_permanent`]. Everything else is internal, and
/// the caller could not have sent any of them differently.
/// Test: `status_and_permanence_pick_the_code`.
pub fn code_for(status: u16, permanent: bool) -> i64 {
    match status {
        400 => CODE_INVALID_PARAMS,
        404 => CODE_NOT_FOUND,
        503 if permanent => CODE_UNAVAILABLE_PERMANENT,
        503 => CODE_UNAVAILABLE,
        _ => CODE_INTERNAL_ERROR,
    }
}

/// Whether a refusal body says its condition will never clear on its own.
///
/// Why two sources rather than one: `retryable` is the field the index-scoped
/// contract names, and it settles the question wherever it is present. It is
/// absent from exactly one body on this surface — `kg_unavailable`, which
/// predates the field. That body carries the same `reason` vocabulary
/// `vector_unavailable` documents, where `skipped_by_config` means the index
/// was BUILT without the lane and only a `PATCH .../config` changes that.
/// Reading it is what stops the socket telling a caller to poll a refusal that
/// will answer identically forever.
/// What: `retryable` when present, else `reason == "skipped_by_config"`, else
/// not permanent — for a status that means "not now", waiting is the safer
/// advice to give than "never".
/// Test: `a_kg_disabled_lane_is_permanent_without_a_retryable_field`,
/// `an_unrecognised_503_body_is_treated_as_retryable`.
pub fn refusal_is_permanent(body: &serde_json::Value) -> bool {
    if let Some(retryable) = body.get("retryable").and_then(serde_json::Value::as_bool) {
        return !retryable;
    }
    body.get("reason").and_then(serde_json::Value::as_str) == Some("skipped_by_config")
}

/// Render one `(status, body)` refusal as the frame a socket client reads.
///
/// Why: the read handlers' cores return exactly this pair, so this is the one
/// place the socket turns a route's refusal into an error frame.
/// What: the code comes from [`code_for`]. The message is the body's own
/// wording — `error` names the class (`index_not_resident`,
/// `unknown index: <id>`) and `message` carries the operator-facing detail, so
/// both are joined when both are present rather than picking one and losing the
/// other. A body with neither is rendered whole, which is never nothing.
/// Test: `refusal_message_joins_error_and_message`,
/// `refusal_without_an_error_field_renders_the_whole_body`.
pub fn rpc_error_from_http(status: axum::http::StatusCode, body: &serde_json::Value) -> RpcError {
    let code = code_for(status.as_u16(), refusal_is_permanent(body));

    let error = body.get("error").and_then(serde_json::Value::as_str);
    let detail = body.get("message").and_then(serde_json::Value::as_str);
    let message = match (error, detail) {
        (Some(e), Some(m)) => format!("{e}: {m}"),
        (Some(e), None) => e.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => body.to_string(),
    };
    RpcError::new(code, message)
}
