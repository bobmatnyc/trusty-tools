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
//! the reason instead of a transport failure. [`RpcFallback`] is the catch-all
//! a service with its own generic dispatcher mounts instead of naming every
//! method (#6286).
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

/// The catch-all consulted when no registered method matches (#6286).
///
/// Why: a daemon that already owns a transport-agnostic dispatcher over
/// `(method, params)` — `trusty-memory`'s `transport::rpc::dispatch` is the
/// case this exists for — would otherwise have to re-register its ~75 methods
/// one at a time to mount on an [`RpcRouter`]. Registering the same table twice
/// is exactly the silent-drift risk the workspace's common-entry-point rule
/// exists to prevent, so the router grows a pass-through instead.
/// What: takes the method NAME as well as the params, because the fallback is
/// the thing that has to decide what the name means. Object-safe, so a router
/// holds it behind `Arc<dyn RpcFallback>` alongside its typed methods.
///
/// A fallback never sees a method the router itself serves: [`dispatch`]
/// consults it only after the [`BTreeMap`] lookup misses, so a name registered
/// with [`RpcRouter::method`] wins and a service can override one method of its
/// own dispatcher by registering it. An error the fallback returns becomes the
/// response's error half verbatim, the same as [`RpcMethod`]'s — a fallback
/// that refuses answers with a reason rather than dropping the connection.
///
/// [`dispatch`]: RpcRouter::dispatch
///
/// Test: `dispatch_routes_an_unregistered_method_to_the_fallback`,
/// `dispatch_prefers_a_registered_method_over_the_fallback`,
/// `dispatch_maps_a_fallback_error_to_an_rpc_error_response`.
#[async_trait]
pub trait RpcFallback: Send + Sync + 'static {
    /// Run `method` against one decoded `params` payload.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError>;
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
    fallback: Option<Arc<dyn RpcFallback>>,
}

impl std::fmt::Debug for RpcRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `method_names` alone reads as "serves nothing" for a router that is
        // all fallback, so the flag is part of what this value is.
        f.debug_struct("RpcRouter")
            .field("methods", &self.method_names().collect::<Vec<_>>())
            .field("fallback", &self.fallback.is_some())
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

    /// Serve every unregistered method through `fallback`, replacing any
    /// previous one (#6286).
    ///
    /// Why: mounts a service's own generic dispatcher without restating its
    /// method table here — see [`RpcFallback`] for what that buys and why the
    /// name is passed through.
    /// What: a router with a fallback answers
    /// [`RpcError::method_not_found`] for nothing; every name the [`BTreeMap`]
    /// misses goes to `fallback` instead. Registered methods still win.
    /// Test: `dispatch_routes_an_unregistered_method_to_the_fallback`,
    /// `dispatch_prefers_a_registered_method_over_the_fallback`.
    pub fn fallback(mut self, fallback: impl RpcFallback) -> Self {
        self.fallback = Some(Arc::new(fallback));
        self
    }

    /// The registered method names, in sorted order.
    ///
    /// Names a [`fallback`] serves are not listed — the router does not know
    /// them, which is the point of the seam.
    ///
    /// [`fallback`]: RpcRouter::fallback
    pub fn method_names(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    /// Decide the response for one request frame.
    ///
    /// What, in order: parse the frame; refuse a `jsonrpc` that is not
    /// [`JSONRPC_VERSION`]; look the method up; run the handler. A name no
    /// handler is registered for goes to the [`fallback`] if one is mounted and
    /// is refused with [`RpcError::method_not_found`] otherwise. A parse failure
    /// answers with a `null` id because there was no readable id to echo —
    /// every other arm echoes the request's.
    ///
    /// Never returns `Err`: a failure to serve is itself a response frame, so
    /// the caller always has something to write back. Hanging up instead would
    /// give the client a transport error where a reason exists. A fallback's
    /// error is no exception — it becomes this frame's error half verbatim.
    ///
    /// Test: `dispatch_routes_to_the_registered_handler`,
    /// `dispatch_reports_method_not_found_for_an_unregistered_method`,
    /// `dispatch_rejects_an_unparseable_frame`,
    /// `dispatch_rejects_a_wrong_jsonrpc_version`,
    /// `dispatch_reports_invalid_params_for_an_undecodable_payload`,
    /// `dispatch_propagates_a_handler_error_verbatim`,
    /// `dispatch_routes_an_unregistered_method_to_the_fallback`,
    /// `dispatch_prefers_a_registered_method_over_the_fallback`,
    /// `dispatch_maps_a_fallback_error_to_an_rpc_error_response`.
    ///
    /// [`fallback`]: RpcRouter::fallback
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
            return self.dispatch_unregistered(request).await;
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

    /// The method-lookup miss: hand the request to the fallback, or refuse it.
    ///
    /// Split out of [`dispatch`] so the `let … else` arm stays one line and the
    /// two answers a miss can have sit next to each other.
    ///
    /// Test: `dispatch_routes_an_unregistered_method_to_the_fallback`,
    /// `dispatch_maps_a_fallback_error_to_an_rpc_error_response`,
    /// `dispatch_reports_method_not_found_for_an_unregistered_method`.
    ///
    /// [`dispatch`]: RpcRouter::dispatch
    async fn dispatch_unregistered(&self, request: RpcRequest) -> RpcResponse {
        let Some(fallback) = self.fallback.as_ref() else {
            let known: Vec<&str> = self.method_names().collect();
            return RpcResponse::failure(
                request.id,
                RpcError::method_not_found(&request.method, &known),
            );
        };

        // Cloned out before the await for the same reason the method arm does
        // it: nothing borrowed from `self` is held across a handler call.
        let fallback = Arc::clone(fallback);
        let RpcRequest {
            id, method, params, ..
        } = request;
        match fallback.call(&method, params).await {
            Ok(result) => RpcResponse::success(id, result),
            // The fallback owns its own refusal codes, so its error crosses the
            // wire unrewritten — a downgrade to a generic internal error would
            // cost the caller the reason it was given.
            Err(error) => RpcResponse::failure(id, error),
        }
    }
}
