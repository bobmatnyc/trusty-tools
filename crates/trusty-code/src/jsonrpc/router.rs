//! Method-name -> async-handler registry and the JSON-RPC dispatch algorithm.
//!
//! Why: the transport loop (`crate::serve::transport`) only knows how to
//! turn bytes into a `trusty_common::mcp::Request` and a `Response` back
//! into bytes; it must not know *which* methods exist. The [`Router`] is
//! the seam in between: handlers register under a method name once, and
//! every future ticket that adds a method (`session.*` #2054, `task.*`
//! #2056, `harness.describe` #2066) does so with one `router.register(...)`
//! call, never touching the transport or the dispatch algorithm.
//!
//! What: [`MethodHandler`] is an object-safe async-handler trait, blanket
//! implemented for any `Fn(Value) -> Future<Output = Result<Value, RpcError>>`
//! (so plain `async fn(Value) -> Result<Value, RpcError>` items and closures
//! both register directly, no adapter boilerplate). [`Router::dispatch`]
//! implements the JSON-RPC 2.0 error model end-to-end for methods it knows
//! about:
//!   - `-32600 Invalid Request` — non-`"2.0"` `jsonrpc` version, or an empty
//!     method name.
//!   - `-32601 Method not found` — no handler registered under the name.
//!   - handler-returned [`RpcError`] (typically `-32602`/`-32603`) is copied
//!     onto the wire error verbatim, `data` included.
//!   - `-32700 Parse error` is produced by [`Router::dispatch_json`], not
//!     [`Router::dispatch`] — the STDIO transport (`crate::serve::transport`,
//!     one JSON object per line) and the HTTP transport
//!     (`crate::serve::http`, one JSON object per `POST /rpc` body) both
//!     receive raw bytes rather than an already-parsed `Request`, and both
//!     need the exact same "parse or -32700" behaviour. Centralising it here
//!     means the error message and code can never drift between the two
//!     wire formats.
//!
//! Per JSON-RPC 2.0 §4.1, a request with no `id` is a *notification*: the
//! server must never reply, even with an error, though the handler (if one
//! matches) still executes for its side effects. `dispatch` computes the
//! response normally and then swaps in `Response::suppressed()` at the end
//! when the original request had no `id`.
//!
//! Test: see `tests` below — one case per error code, a notification that
//! still executes its handler but suppresses the reply, and a successful
//! round trip.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use trusty_common::mcp::{Request, Response, error_codes};

use super::error::RpcError;

/// A boxed, pinned future returned by a method handler.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

/// Object-safe async handler for one JSON-RPC method.
///
/// Why: `Router` stores handlers as trait objects (`Arc<dyn MethodHandler>`)
/// so methods of different concrete closure/fn types can share one registry.
/// Async fns are not directly object-safe, hence the `call` -> boxed-future
/// indirection.
/// What: implemented for `Self` by the blanket impl below whenever `Self` is
/// `Fn(Value) -> Fut` with `Fut: Future<Output = Result<Value, RpcError>>` —
/// callers never need to implement this trait by hand.
/// Test: exercised indirectly by every `router::tests` case and by
/// `crate::serve::methods::tests`.
pub trait MethodHandler: Send + Sync {
    /// Invoke the handler with the request's `params` (defaulted to `Null`
    /// when the wire request omitted them).
    fn call(&self, params: Value) -> HandlerFuture;
}

impl<F, Fut> MethodHandler for F
where
    F: Fn(Value) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    fn call(&self, params: Value) -> HandlerFuture {
        Box::pin(self(params))
    }
}

/// Method-name -> handler registry and JSON-RPC 2.0 dispatcher.
///
/// Why: the single seam between the STDIO transport and tcode's business
/// methods. See the module docs for the extensibility rationale.
/// What: a thin `HashMap<String, Arc<dyn MethodHandler>>` plus `dispatch`.
/// Test: `router::tests::*`.
#[derive(Default)]
pub struct Router {
    handlers: HashMap<String, Arc<dyn MethodHandler>>,
}

impl Router {
    /// Construct an empty router.
    ///
    /// Why: callers assemble a router by registering methods immediately
    /// after construction (see `crate::serve::build_router`).
    /// What: returns a `Router` with no registered methods.
    /// Test: `dispatch_unknown_method_returns_method_not_found` uses a
    /// router with exactly one registered method to prove unregistered
    /// names are still rejected.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a method handler under `method`.
    ///
    /// Why: the sole way new JSON-RPC methods enter the dispatcher.
    /// Re-registering the same name replaces the previous handler (last
    /// write wins) rather than erroring, since duplicate registration is a
    /// programmer error caught by tests, not a runtime condition to guard.
    /// What: inserts `handler` into the registry; returns `&mut Self` so
    /// call sites can chain multiple `.register(...)` calls.
    /// Test: `dispatch_registered_method_returns_handler_result`.
    pub fn register(
        &mut self,
        method: impl Into<String>,
        handler: impl MethodHandler + 'static,
    ) -> &mut Self {
        self.handlers.insert(method.into(), Arc::new(handler));
        self
    }

    /// Dispatch a parsed JSON-RPC request, returning the response to write
    /// back on the wire (or a suppressed response for notifications).
    ///
    /// Why: the single entry point the transport loop calls per request
    /// line. See the module docs for the full error-code contract.
    /// What: validates the envelope, looks up the handler, awaits it, and
    /// maps the result onto a `Response` — suppressing the reply entirely
    /// when `req.id` was absent (a notification).
    /// Test: `router::tests::*` (one case per code path).
    pub async fn dispatch(&self, req: Request) -> Response {
        let is_notification = req.id.is_none();
        let response = self.dispatch_replyable(&req).await;
        if is_notification {
            Response::suppressed()
        } else {
            response
        }
    }

    /// Compute the reply for `req` as if it were not a notification.
    ///
    /// Why: split out of `dispatch` so notification suppression is a single
    /// decision made once, after the (possibly side-effecting) handler has
    /// already run — matching JSON-RPC 2.0 §4.1's "notifications still
    /// execute, they just never get a reply" semantics.
    /// What: envelope validation, method lookup, handler invocation, result
    /// mapping. Never suppresses.
    /// Test: covered transitively via `dispatch`'s tests.
    async fn dispatch_replyable(&self, req: &Request) -> Response {
        let id = req.id.clone();

        if let Some(version) = &req.jsonrpc
            && version != "2.0"
        {
            return Response::err(
                id,
                error_codes::INVALID_REQUEST,
                format!("unsupported jsonrpc version \"{version}\" (expected \"2.0\")"),
            );
        }
        if req.method.trim().is_empty() {
            return Response::err(id, error_codes::INVALID_REQUEST, "method must not be empty");
        }

        let Some(handler) = self.handlers.get(req.method.as_str()) else {
            return Response::err(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("method not found: {}", req.method),
            );
        };

        let params = req.params.clone().unwrap_or(Value::Null);
        match handler.call(params).await {
            Ok(result) => Response::ok(id, result),
            Err(e) => {
                let mut resp = Response::err(id, e.code, e.message);
                if let Some(err) = resp.error.as_mut() {
                    err.data = e.data;
                }
                resp
            }
        }
    }

    /// Parse `raw` as a single JSON-RPC request and dispatch it, mapping a
    /// JSON parse failure onto `-32700 Parse error` instead of propagating.
    ///
    /// Why: both wire transports (STDIO lines, HTTP request bodies) receive
    /// raw bytes rather than an already-parsed `Request`; this is the one
    /// place "parse or -32700" is implemented, so the message/code can never
    /// drift between them. See the module docs for the full rationale.
    /// What: `serde_json::from_slice::<Request>(raw)` then [`Self::dispatch`].
    /// On parse failure, returns `Response::err(None, error_codes::PARSE_ERROR,
    /// ...)` without ever invoking a handler — the request `id` is
    /// unrecoverable from malformed JSON, so the response carries a `null`
    /// id per the JSON-RPC 2.0 spec.
    /// Test: `dispatch_json_valid_request_dispatches`,
    /// `dispatch_json_malformed_bytes_return_parse_error`.
    pub async fn dispatch_json(&self, raw: &[u8]) -> Response {
        match serde_json::from_slice::<Request>(raw) {
            Ok(req) => self.dispatch(req).await,
            Err(e) => Response::err(
                None,
                error_codes::PARSE_ERROR,
                format!("invalid JSON-RPC: {e}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn req(id: Option<Value>, method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: Some("2.0".to_string()),
            id,
            method: method.to_string(),
            params,
        }
    }

    /// A registered method must return its handler's `Ok` result verbatim.
    #[tokio::test]
    async fn dispatch_registered_method_returns_handler_result() {
        let mut router = Router::new();
        router.register(
            "ping",
            |_params: Value| async move { Ok(json!({"pong": true})) },
        );

        let resp = router.dispatch(req(Some(json!(1)), "ping", None)).await;
        assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
        assert_eq!(resp.result, Some(json!({"pong": true})));
        assert_eq!(resp.id, Some(json!(1)));
        assert!(!resp.suppress);
    }

    /// Unregistered methods must return `-32601 Method not found`.
    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let mut router = Router::new();
        router.register("ping", |_params: Value| async move { Ok(json!({})) });

        let resp = router
            .dispatch(req(Some(json!(2)), "definitely_missing", None))
            .await;
        let err = resp.error.expect("expected an error response");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert!(err.message.contains("definitely_missing"));
    }

    /// A non-`"2.0"` `jsonrpc` field must return `-32600 Invalid Request`.
    #[tokio::test]
    async fn dispatch_wrong_jsonrpc_version_returns_invalid_request() {
        let router = Router::new();
        let mut r = req(Some(json!(3)), "ping", None);
        r.jsonrpc = Some("1.0".to_string());

        let resp = router.dispatch(r).await;
        let err = resp.error.expect("expected an error response");
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
    }

    /// An empty method name must return `-32600 Invalid Request`.
    #[tokio::test]
    async fn dispatch_empty_method_returns_invalid_request() {
        let router = Router::new();
        let resp = router.dispatch(req(Some(json!(4)), "", None)).await;
        let err = resp.error.expect("expected an error response");
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
    }

    /// A handler-returned `RpcError` must be copied onto the wire error,
    /// `data` included.
    #[tokio::test]
    async fn dispatch_handler_error_maps_to_response_error() {
        let mut router = Router::new();
        router.register("boom", |_params: Value| async move {
            Err(RpcError::invalid_params("bad field").with_data(json!({"field": "name"})))
        });

        let resp = router.dispatch(req(Some(json!(5)), "boom", None)).await;
        let err = resp.error.expect("expected an error response");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert_eq!(err.message, "bad field");
        assert_eq!(err.data, Some(json!({"field": "name"})));
    }

    /// A notification (no `id`) must still execute its handler's side
    /// effect but the reply must be suppressed.
    #[tokio::test]
    async fn dispatch_notification_executes_handler_but_suppresses_reply() {
        static RAN: AtomicBool = AtomicBool::new(false);
        let mut router = Router::new();
        router.register("notify_me", |_params: Value| async move {
            RAN.store(true, Ordering::SeqCst);
            Ok(json!({}))
        });

        let resp = router.dispatch(req(None, "notify_me", None)).await;
        assert!(resp.suppress, "notifications must suppress the reply");
        assert!(
            RAN.load(Ordering::SeqCst),
            "notification handler must still run"
        );
    }

    /// `params` must default to `Null` when the wire request omits them.
    #[tokio::test]
    async fn dispatch_missing_params_defaults_to_null() {
        let mut router = Router::new();
        router.register("echo_params", |params: Value| async move { Ok(params) });

        let resp = router
            .dispatch(req(Some(json!(6)), "echo_params", None))
            .await;
        assert_eq!(resp.result, Some(Value::Null));
    }

    /// `dispatch_json` on well-formed bytes must behave exactly like
    /// `dispatch` on the parsed request.
    #[tokio::test]
    async fn dispatch_json_valid_request_dispatches() {
        let mut router = Router::new();
        router.register(
            "ping",
            |_params: Value| async move { Ok(json!({"pong": true})) },
        );

        let raw = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = router.dispatch_json(raw).await;
        assert_eq!(resp.result, Some(json!({"pong": true})));
    }

    /// Malformed bytes must map to `-32700 Parse error`, not a panic.
    #[tokio::test]
    async fn dispatch_json_malformed_bytes_return_parse_error() {
        let router = Router::new();
        let resp = router.dispatch_json(b"not json at all").await;
        let err = resp.error.expect("expected a parse error");
        assert_eq!(err.code, error_codes::PARSE_ERROR);
    }
}
