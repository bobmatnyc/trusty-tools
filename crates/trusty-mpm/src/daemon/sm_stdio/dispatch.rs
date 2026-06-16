//! The JSON-RPC 2.0 router for the SM stdio adapter (§1A.1 / §1A.2).
//!
//! Why: one place that turns a parsed [`Request`] into a JSON-RPC 2.0
//! [`Response`] — echoing the `id`, routing the method name to its handler in
//! [`super::methods`], and mapping a handler's typed [`MethodError`] onto the
//! correct JSON-RPC error code. Keeping the router separate from the handlers
//! keeps both legible and under the SLOC cap. Unknown methods become
//! `METHOD_NOT_FOUND`; bad params become `INVALID_PARAMS`; a notification (no id)
//! is suppressed — never a panic.
//! What: [`dispatch`] — `(&SmDispatcher, Request) -> Response`. It returns the
//! suppressed sentinel for id-less notifications, routes the 13 `sm.*` methods,
//! and folds every other method into a method-not-found error.
//! Test: `tests.rs` round-trips every method + the parse/unknown/notification
//! paths.

use serde_json::Value;
use trusty_common::mcp::{Response, error_codes};

use super::SmDispatcher;
use super::methods::{self, CODE_NOT_FOUND, CODE_UNAVAILABLE, MethodError};
use trusty_common::mcp::Request;

/// Route one request to its mapped SM surface and shape the JSON-RPC response.
///
/// Why: the single dispatch seam the stdio loop and every test drive. It echoes
/// the request `id`, suppresses id-less notifications (per JSON-RPC 2.0), routes
/// the method to its handler, and maps the handler's `Result<Value, MethodError>`
/// onto a `result`/`error` response. No panics: a missing handler →
/// method-not-found, a handler error → its mapped code.
/// What: matches `req.method` against the 13 `sm.*` names; on a hit awaits the
/// handler and builds `Response::ok` / the mapped error; on a miss builds
/// `Response::err(METHOD_NOT_FOUND)`.
/// Test: `tests.rs` — all 13 methods, unknown method, notification suppression.
pub async fn dispatch(d: &SmDispatcher, req: Request) -> Response {
    // JSON-RPC notifications (no id) must not produce a reply.
    if req.id.is_none() {
        return Response::suppressed();
    }
    let id = req.id.clone();
    let params = req.params.clone().unwrap_or(Value::Null);

    let result: Result<Value, MethodError> = match req.method.as_str() {
        "sm.chat" => methods::sm_chat(d, &params).await,
        "sm.health" => methods::sm_health(d, &params).await,
        "sm.goals.list" => methods::sm_goals_list(d, &params).await,
        "sm.goals.create" => methods::sm_goals_create(d, &params).await,
        "sm.goals.update" => methods::sm_goals_update(d, &params).await,
        "sm.sessions.launch" => methods::sm_sessions_launch(d, &params).await,
        "sm.sessions.list" => methods::sm_sessions_list(d, &params).await,
        "sm.sessions.get" => methods::sm_sessions_get(d, &params).await,
        "sm.sessions.send" => methods::sm_sessions_send(d, &params).await,
        "sm.sessions.stop" => methods::sm_sessions_stop(d, &params).await,
        "sm.sessions.resume" => methods::sm_sessions_resume(d, &params).await,
        "sm.sessions.kill" => methods::sm_sessions_kill(d, &params).await,
        "sm.context.get" => methods::sm_context_get(d, &params).await,
        other => {
            return Response::err(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("unknown SM method: {other}"),
            );
        }
    };

    match result {
        Ok(value) => Response::ok(id, value),
        Err(e) => Response::err(id, error_code_for(&e), e.to_string()),
    }
}

/// Map a [`MethodError`] class onto its JSON-RPC error code.
///
/// Why: the router must report the right code per failure class so a driver can
/// distinguish bad input (`-32602`) from a missing entity (`-32001`), an
/// unavailable surface (`-32002`), and a backend failure (`-32603`).
/// What: matches the variant to its code.
/// Test: `tests.rs` asserts the code for each error path.
fn error_code_for(e: &MethodError) -> i32 {
    match e {
        MethodError::InvalidParams(_) => error_codes::INVALID_PARAMS,
        MethodError::NotFound(_) => CODE_NOT_FOUND,
        MethodError::Unavailable(_) => CODE_UNAVAILABLE,
        MethodError::Internal(_) => error_codes::INTERNAL_ERROR,
    }
}
