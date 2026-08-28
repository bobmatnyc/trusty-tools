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
//!
//! [`rpc_error_from_http`]: crate::service::rpc::error::rpc_error_from_http
//! [`CODE_UNAVAILABLE`]: crate::service::rpc::error::CODE_UNAVAILABLE
//! [`CODE_UNAVAILABLE_PERMANENT`]: crate::service::rpc::error::CODE_UNAVAILABLE_PERMANENT
//! [`RpcError`]: trusty_common::uds::server::RpcError

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

/// HTTP 408 over the socket — the interactive-query deadline (#907) expired
/// before the handler answered (#6285 slice 3).
///
/// Distinct from [`CODE_UNAVAILABLE`] because the remedy is different: a busy
/// daemon clears on its own, while a query that outran the deadline will do it
/// again unless the caller narrows it. Folding it into `internal_error` would
/// tell the caller to file a bug about its own too-broad query.
///
/// The same number `trusty-analyze` uses for the same meaning
/// (`service::events::CODE_DEADLINE_EXCEEDED`).
pub const CODE_DEADLINE_EXCEEDED: i64 = -32005;

/// HTTP 403 over the socket — the #767 allowlist has not approved this root
/// (#6285 slice 4).
///
/// Why not `internal_error`: the remedy is the operator's and the body names it
/// (`trusty-search index add <root>`). Telling a caller to file a bug about a
/// policy decision it can resolve is the wrong instruction.
///
/// The same number `trusty-mpm` uses for the same meaning
/// (`daemon::error::CODE_FORBIDDEN`).
pub const CODE_FORBIDDEN: i64 = -32003;

/// HTTP 409 over the socket — this write collides with a registration that
/// already exists (#6285 slice 4).
///
/// Why distinct from [`CODE_NOT_FOUND`] and [`CODE_INVALID_PARAMS`]: the request
/// is well-formed and the subject exists; what is wrong is the state of the
/// registry. #2336, #3993 and #5357 all answer 409, and each names in the body
/// the OTHER id or root the caller has to deal with — a code that said "your
/// params were bad" would send it back to re-check a request that was correct.
///
/// The same number `trusty-mpm` uses for the same meaning
/// (`daemon::error::CODE_CONFLICT`).
pub const CODE_CONFLICT: i64 = -32009;

/// HTTP 429 over the socket — the #120 reindex cooldown is still running
/// (#6285 slice 4).
///
/// Why not [`CODE_UNAVAILABLE`]: both clear on their own, but this one clears at
/// a time the body states (`retry_after_secs`), and retrying before then fails
/// identically. Folding it into the generic busy code would invite the tight
/// retry loop the cooldown exists to break.
///
/// A free slot beside [`CODE_UNAVAILABLE_PERMANENT`]: `trusty-common` holds
/// -32010/-32011 and trusty-mpm holds -32000 through -32009 plus -32023/-32024.
pub const CODE_TOO_MANY_REQUESTS: i64 = -32013;

/// The JSON-RPC code an HTTP status projects onto.
///
/// Why a function rather than a match at each call site: the read surface has
/// nine methods and four refusal classes between them, and one table is what
/// makes "the code is a pure function of what HTTP would have sent" checkable.
/// What: 400 is the caller's fault; 404 is permanent absence; 408 is the query
/// deadline; 503 splits on `permanent` — see [`refusal_is_permanent`].
/// Everything else is internal, and the caller could not have sent any of them
/// differently.
///
/// #6285 slice 3 added 408: the query surface is the only one HTTP bounds with
/// a per-request deadline, so the read surface never produced that status.
///
/// #6285 slice 4 added 403, 409 and 429. The write surface is the first to
/// refuse for a reason the CALLER can act on but did not cause by malforming
/// its request — an unapproved root, a registry collision, a cooldown — and all
/// three fell to `internal_error`, which tells a caller to file a bug about a
/// refusal whose remedy the body already names. 500 is deliberately still
/// `internal_error`: a corpus that would not open or an HNSW that would not
/// allocate is not something the caller could have sent differently.
/// Test: `status_and_permanence_pick_the_code`,
/// `the_write_surface_statuses_each_pick_their_own_code`.
pub fn code_for(status: u16, permanent: bool) -> i64 {
    match status {
        400 => CODE_INVALID_PARAMS,
        403 => CODE_FORBIDDEN,
        404 => CODE_NOT_FOUND,
        408 => CODE_DEADLINE_EXCEEDED,
        409 => CODE_CONFLICT,
        429 => CODE_TOO_MANY_REQUESTS,
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
