//! Method-name dispatch: the decision half of the server, over bytes.
//!
//! Why: `webhook_relay::serve::dispatch_frame` is a synchronous function over a
//! byte slice precisely so every arm — including the ones that must never
//! succeed — is assertable without a socket. That property is worth keeping and
//! is why the decision lives here rather than inside the accept loop. What
//! changes is the number of methods: `webhook_relay` serves exactly one and
//! hardcodes it, so its dispatcher cannot be reused by a service with three.
//!
//! What: [`RpcRouter`] maps a method name to an [`RpcMethod`]. [`typed_method`]
//! wraps an `async fn(Req) -> Result<Resp, RpcError>` so the caller names its
//! own request and response types and never touches `serde_json::Value`.
//! [`RpcRouter::dispatch`] parses one frame, checks the envelope, looks up the
//! method, and returns the response frame to write — every failure as a coded
//! JSON-RPC error rather than a dropped connection, so a drifted client reads
//! the reason instead of a transport failure.
//!
//! Dispatch is `async` and takes `&self`, so one router serves every connection
//! concurrently; nothing here holds a lock across a handler call.
//!
//! Test: `super::tests` — `dispatch_*`.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::wire::{
    CODE_INVALID_REQUEST, CODE_PARSE_ERROR, JSONRPC_VERSION, RpcError, RpcRequest, RpcResponse,
};

/// One method's implementation.
///
/// Why: object-safe so a router can hold a heterogeneous set of methods behind
/// `Arc<dyn RpcMethod>`. Most callers never name this trait — [`typed_method`]
/// builds an implementation from an ordinary async function.
/// What: takes the raw `params` value and returns the raw `result` value, or an
/// [`RpcError`] that becomes the response's error half verbatim.
/// Test: `dispatch_routes_to_the_registered_handler`.
#[async_trait]
pub trait RpcMethod: Send + Sync + 'static {
    /// Run this method against one decoded `params` payload.
    async fn call(&self, params: serde_json::Value) -> Result<serde_json::Value, RpcError>;
}

/// Adapter behind [`typed_method`]; `PhantomData<fn(Req) -> Resp>` keeps the
/// struct `Send + Sync` whatever `Req` and `Resp` are.
struct Typed<Req, Resp, F> {
    call: F,
    _types: PhantomData<fn(Req) -> Resp>,
}

#[async_trait]
impl<Req, Resp, F, Fut> RpcMethod for Typed<Req, Resp, F>
where
    Req: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Resp, RpcError>> + Send + 'static,
{
    async fn call(&self, params: serde_json::Value) -> Result<serde_json::Value, RpcError> {
        let request: Req = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("params do not decode: {e}")))?;
        let response = (self.call)(request).await?;
        // A handler whose own response type will not serialise is a programmer
        // error on this side of the wire, so it reports as internal rather than
        // as anything the caller could have sent differently.
        serde_json::to_value(&response)
            .map_err(|e| RpcError::internal(format!("serialize response: {e}")))
    }
}

/// Build an [`RpcMethod`] from an async function over the caller's own types.
///
/// Why: this is what keeps the server generic without pushing `serde_json::
/// Value` into every service that uses it. The caller defines `Req` and `Resp`;
/// the decode failure becomes [`RpcError::invalid_params`] with the serde
/// message attached, which is the error shape an axum `Json` extractor produces
/// for the same bad body.
/// What: wraps `call` so `params` is deserialised into `Req` before it runs and
/// its `Resp` is serialised back into the response's `result`.
/// Test: `dispatch_reports_invalid_params_for_an_undecodable_payload`,
/// `dispatch_routes_to_the_registered_handler`.
pub fn typed_method<Req, Resp, F, Fut>(call: F) -> impl RpcMethod
where
    Req: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Resp, RpcError>> + Send + 'static,
{
    Typed::<Req, Resp, F> {
        call,
        _types: PhantomData,
    }
}

/// The set of methods one socket serves.
///
/// Why: the caller-supplied half of the server. Everything else in this module
/// family — hardening, framing, the accept loop — is fixed; this is the only
/// part a service defines.
/// What: a name-to-handler map with a consuming builder, and [`dispatch`] over
/// one raw frame. `BTreeMap` rather than `HashMap` so
/// [`RpcError::method_not_found`] lists the known methods in a stable order and
/// two runs of a test compare equal.
/// Test: `dispatch_*`.
///
/// [`dispatch`]: RpcRouter::dispatch
#[derive(Default)]
pub struct RpcRouter {
    methods: BTreeMap<String, Arc<dyn RpcMethod>>,
}

impl std::fmt::Debug for RpcRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcRouter")
            .field("methods", &self.method_names().collect::<Vec<_>>())
            .finish()
    }
}

impl RpcRouter {
    /// An empty router. Every method it is asked for reports
    /// [`RpcError::method_not_found`] until one is registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` under `name`, replacing any previous registration.
    pub fn method(mut self, name: impl Into<String>, handler: impl RpcMethod) -> Self {
        self.methods.insert(name.into(), Arc::new(handler));
        self
    }

    /// Register an async function over the caller's own request and response
    /// types — [`method`] composed with [`typed_method`].
    ///
    /// [`method`]: RpcRouter::method
    pub fn typed<Req, Resp, F, Fut>(self, name: impl Into<String>, call: F) -> Self
    where
        Req: DeserializeOwned + Send + 'static,
        Resp: Serialize + Send + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Resp, RpcError>> + Send + 'static,
    {
        self.method(name, typed_method::<Req, Resp, F, Fut>(call))
    }

    /// The registered method names, in sorted order.
    pub fn method_names(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    /// Decide the response for one request frame.
    ///
    /// What, in order: parse the frame; refuse a `jsonrpc` that is not
    /// [`JSONRPC_VERSION`]; refuse a method no handler is registered for; then
    /// run the handler. A parse failure answers with a `null` id because there
    /// was no readable id to echo — every other arm echoes the request's.
    ///
    /// Never returns `Err`: a failure to serve is itself a response frame, so
    /// the caller always has something to write back. Hanging up instead would
    /// give the client a transport error where a reason exists.
    ///
    /// Test: `dispatch_routes_to_the_registered_handler`,
    /// `dispatch_reports_method_not_found_for_an_unregistered_method`,
    /// `dispatch_rejects_an_unparseable_frame`,
    /// `dispatch_rejects_a_wrong_jsonrpc_version`,
    /// `dispatch_reports_invalid_params_for_an_undecodable_payload`,
    /// `dispatch_propagates_a_handler_error_verbatim`.
    pub async fn dispatch(&self, frame: &[u8]) -> RpcResponse {
        let request: RpcRequest = match serde_json::from_slice(frame) {
            Ok(r) => r,
            Err(e) => {
                return RpcResponse::failure(
                    serde_json::Value::Null,
                    RpcError::new(CODE_PARSE_ERROR, format!("unparseable request frame: {e}")),
                );
            }
        };

        if request.jsonrpc != JSONRPC_VERSION {
            return RpcResponse::failure(
                request.id,
                RpcError::new(
                    CODE_INVALID_REQUEST,
                    format!(
                        "unsupported jsonrpc version {:?}; this listener speaks {JSONRPC_VERSION}",
                        request.jsonrpc
                    ),
                ),
            );
        }

        let Some(handler) = self.methods.get(&request.method) else {
            let known: Vec<&str> = self.method_names().collect();
            return RpcResponse::failure(
                request.id,
                RpcError::method_not_found(&request.method, &known),
            );
        };

        // Cloned out of the map before the await so the borrow of `self.methods`
        // ends here — a handler is free to run for as long as it needs without
        // holding anything the next connection wants.
        let handler = Arc::clone(handler);
        match handler.call(request.params).await {
            Ok(result) => RpcResponse::success(request.id, result),
            Err(error) => RpcResponse::failure(request.id, error),
        }
    }
}
