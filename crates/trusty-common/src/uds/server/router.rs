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
//! [`RpcRouter::typed_stream`] registers a method that answers in many frames
//! instead of one (#6286). Streaming and unary names live in separate tables, so
//! one name is one or the other, and [`RpcRouter::dispatch_streaming`] is the
//! wider entry point that can return either. [`RpcRouter::dispatch`] keeps
//! answering with exactly one frame and refuses a streaming method rather than
//! producing an answer its caller cannot read.
//!
//! Dispatch is `async` and takes `&self`, so one router serves every connection
//! concurrently; nothing here holds a lock across a handler call.
//!
//! Test: `super::tests` — `dispatch_*` and `stream_*`.

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::stream::{RpcOutcome, RpcStreamMethod, typed_stream_method};
use super::wire::{
    CODE_INVALID_REQUEST, CODE_PARSE_ERROR, JSONRPC_VERSION, RpcError, RpcRequest, RpcResponse,
};

/// The request field a caller sets to ask for a stream (#6286).
///
/// Why it is read separately rather than added to [`RpcRequest`]: that struct's
/// fields are all public and it is not `#[non_exhaustive]`, so a new field would
/// break every external crate that builds one with a literal — a major bump for
/// an additive feature. Reading the flag through its own probe keeps
/// [`RpcRequest`] byte-identical as a public type.
///
/// The second parse costs one pass over the frame and is paid ONLY on a request
/// whose method is registered as a streaming one, which is about to do far more
/// work than a parse. A unary request never reaches it.
///
/// Test: `stream_opt_in_is_read_from_the_request_frame`.
#[derive(Deserialize)]
struct StreamOptIn {
    #[serde(default)]
    stream: bool,
}

/// The method name alone, read back off a frame (#6621).
///
/// Why it is a second parse rather than a value threaded out of [`RpcRouter::
/// dispatch_streaming`]: that function answers with an [`RpcOutcome`], and
/// widening it to carry the classification would change the signature every
/// caller of it already uses. The parse costs one pass over a frame the server
/// has already read into memory, and is paid ONLY by a router that registers at
/// least one liveness method — see [`RpcRouter::frame_is_liveness`].
///
/// Test: `frame_is_liveness_reads_the_method_name_off_the_frame`.
#[derive(Deserialize)]
struct MethodName {
    method: String,
}

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
    streams: BTreeMap<String, Arc<dyn RpcStreamMethod>>,
    fallback: Option<Arc<dyn RpcFallback>>,
    /// Names whose answers must not restart an idle window (#6621). Held
    /// separately from `methods` so marking a name is independent of how its
    /// handler was registered — including through a [`RpcFallback`].
    liveness: BTreeSet<String>,
}

impl std::fmt::Debug for RpcRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `method_names` alone reads as "serves nothing" for a router that is
        // all fallback, so the flag is part of what this value is.
        f.debug_struct("RpcRouter")
            .field("methods", &self.method_names().collect::<Vec<_>>())
            .field("streams", &self.stream_names().collect::<Vec<_>>())
            .field("liveness", &self.liveness_names().collect::<Vec<_>>())
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

    /// Mark `name` as a LIVENESS method: answering it does not count as
    /// activity for an idle-exit policy (#6621).
    ///
    /// Why: an on-demand service exits after an idle window, and "idle" is
    /// measured from the last ANSWERED request. A monitor that dials a health
    /// method on a poll loop therefore re-arms that window on every poll and the
    /// process never exits — `trusty-console` polls `analyze.health` every 15s
    /// against a 600s window, and the process it pinned lived 46 hours. Marking
    /// the method is what tells the accept loop the answer carried no work.
    ///
    /// What: records the NAME. The handler is registered exactly as it was —
    /// this only classifies. It is deliberately a separate call from
    /// [`method`]/[`typed`] so a name served through a [`fallback`] can be
    /// marked too, and so the classification is a visible line in the router
    /// definition rather than a string match buried in the serve loop.
    ///
    /// 🔴 Mark only methods whose answer is pure liveness — a status, a version,
    /// a reachability flag. A method that does the caller's work must never be
    /// marked: the service would exit under a client that is genuinely using it.
    ///
    /// [`method`]: RpcRouter::method
    /// [`typed`]: RpcRouter::typed
    /// [`fallback`]: RpcRouter::fallback
    ///
    /// Test: `serve_until_idle_ignores_a_registered_liveness_method`,
    /// `liveness_names_are_sorted_and_separate_from_the_method_table`.
    pub fn mark_liveness(mut self, name: impl Into<String>) -> Self {
        self.liveness.insert(name.into());
        self
    }

    /// Register a liveness method over the caller's own types — [`typed`]
    /// composed with [`mark_liveness`] (#6621).
    ///
    /// [`typed`]: RpcRouter::typed
    /// [`mark_liveness`]: RpcRouter::mark_liveness
    pub fn typed_liveness<Req, Resp, F, Fut>(self, name: impl Into<String>, call: F) -> Self
    where
        Req: DeserializeOwned + Send + 'static,
        Resp: Serialize + Send + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Resp, RpcError>> + Send + 'static,
    {
        let name = name.into();
        self.typed::<Req, Resp, F, Fut>(name.clone(), call)
            .mark_liveness(name)
    }

    /// The names marked as liveness, in sorted order (#6621).
    ///
    /// A service asserts its own classification against this — see
    /// `trusty-analyze`'s `rpc_health_is_registered_as_a_liveness_method`.
    pub fn liveness_names(&self) -> impl Iterator<Item = &str> {
        self.liveness.iter().map(String::as_str)
    }

    /// Whether `frame` names a method marked with [`mark_liveness`] (#6621).
    ///
    /// Why here rather than in the accept loop: the classification belongs to
    /// the router registration, so the loop asks a question instead of matching
    /// a string it would have to keep in step with every service.
    /// What: `false` without a second glance when no name is marked — every
    /// router that predates #6621 — so the parse is paid only where it decides
    /// something. An unparseable frame is not liveness: it is about to be
    /// refused, and a refusal is not activity the loop credits either way.
    ///
    /// [`mark_liveness`]: RpcRouter::mark_liveness
    ///
    /// Test: `frame_is_liveness_reads_the_method_name_off_the_frame`,
    /// `frame_is_liveness_is_false_for_a_router_that_marks_nothing`.
    pub(super) fn frame_is_liveness(&self, frame: &[u8]) -> bool {
        if self.liveness.is_empty() {
            return false;
        }
        serde_json::from_slice::<MethodName>(frame)
            .is_ok_and(|named| self.liveness.contains(&named.method))
    }

    /// Register a streaming `handler` under `name`, replacing any previous
    /// streaming registration (#6286).
    ///
    /// Streaming and unary names live in SEPARATE tables, so one name is one or
    /// the other and never both. Registering `"chat"` in both would leave the
    /// answer depending on the request's `stream` flag, which is a protocol the
    /// caller cannot reason about from the method name alone.
    ///
    /// Test: `stream_round_trips_many_frames_over_a_real_socket`.
    pub fn stream_method(mut self, name: impl Into<String>, handler: impl RpcStreamMethod) -> Self {
        self.streams.insert(name.into(), Arc::new(handler));
        self
    }

    /// Register a streaming async function over the caller's own request type —
    /// [`stream_method`] composed with [`typed_stream_method`].
    ///
    /// The function returns an `mpsc::Receiver`, so a producer that already
    /// fills an `mpsc::Sender` from a background task — `trusty-memory`'s chat
    /// handler is the case #6286 exists for — hands back its receiver and
    /// changes nothing else.
    ///
    /// [`stream_method`]: RpcRouter::stream_method
    ///
    /// Test: `stream_round_trips_many_frames_over_a_real_socket`,
    /// `stream_reports_invalid_params_before_opening_the_stream`.
    pub fn typed_stream<Req, F, Fut>(self, name: impl Into<String>, call: F) -> Self
    where
        Req: DeserializeOwned + Send + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<super::RpcStreamItems, RpcError>> + Send + 'static,
    {
        self.stream_method(name, typed_stream_method::<Req, F, Fut>(call))
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

    /// The registered streaming method names, in sorted order (#6286).
    ///
    /// Test: `stream_names_are_sorted_and_separate_from_unary_names`.
    pub fn stream_names(&self) -> impl Iterator<Item = &str> {
        self.streams.keys().map(String::as_str)
    }

    /// Decide the ONE response frame for one request frame.
    ///
    /// What, in order: parse the frame; refuse a `jsonrpc` that is not
    /// [`JSONRPC_VERSION`]; look the method up; run the handler. A name no
    /// handler is registered for goes to the [`fallback`] if one is mounted and
    /// is refused with [`RpcError::method_not_found`] otherwise. A parse failure
    /// answers with a `null` id because there was no readable id to echo —
    /// every other arm echoes the request's.
    ///
    /// A method registered with [`typed_stream`] is refused here with
    /// [`CODE_STREAM_REQUIRED`] rather than served (#6286): a caller of this
    /// function writes one frame and reads one frame, so it could not read the
    /// answer a stream produces. [`dispatch_streaming`] is the entry point that
    /// can. A router with no streaming methods never reaches that arm and
    /// behaves exactly as it did before #6286.
    ///
    /// Never returns `Err`: a failure to serve is itself a response frame, so
    /// the caller always has something to write back. Hanging up instead would
    /// give the client a transport error where a reason exists. A fallback's
    /// error is no exception — it becomes this frame's error half verbatim.
    ///
    /// [`typed_stream`]: RpcRouter::typed_stream
    /// [`dispatch_streaming`]: RpcRouter::dispatch_streaming
    /// [`CODE_STREAM_REQUIRED`]: super::CODE_STREAM_REQUIRED
    ///
    /// Test: `dispatch_routes_to_the_registered_handler`,
    /// `dispatch_reports_method_not_found_for_an_unregistered_method`,
    /// `dispatch_rejects_an_unparseable_frame`,
    /// `dispatch_rejects_a_wrong_jsonrpc_version`,
    /// `dispatch_reports_invalid_params_for_an_undecodable_payload`,
    /// `dispatch_propagates_a_handler_error_verbatim`,
    /// `dispatch_routes_an_unregistered_method_to_the_fallback`,
    /// `dispatch_prefers_a_registered_method_over_the_fallback`,
    /// `dispatch_maps_a_fallback_error_to_an_rpc_error_response`,
    /// `unary_request_for_a_streaming_method_is_refused_in_one_frame`.
    ///
    /// [`fallback`]: RpcRouter::fallback
    pub async fn dispatch(&self, frame: &[u8]) -> RpcResponse {
        let request = match self.envelope(frame) {
            Ok(request) => request,
            Err(response) => return response,
        };

        // A caller of `dispatch` writes one frame and reads one frame, so a
        // streaming method has to refuse in that shape rather than answer in a
        // shape the caller cannot read (#6286).
        if self.streams.contains_key(&request.method) {
            let method = request.method.clone();
            return RpcResponse::failure(request.id, RpcError::stream_required(&method));
        }

        self.call_unary(request).await
    }

    /// Decide the response for one request frame, allowing a streaming answer
    /// (#6286).
    ///
    /// Why: [`dispatch`] answers with a value, which cannot express "many frames
    /// follow". This is the wider decision [`super::handle_connection`] drives,
    /// kept separate so every existing caller of `dispatch` is untouched and a
    /// transport that can write only one frame cannot accidentally be handed a
    /// stream.
    ///
    /// What, in order: the same envelope checks [`dispatch`] runs, then the
    /// four-way decision between the caller's `stream` flag and whether the
    /// method is registered as streaming.
    ///
    /// | `"stream"` | method | answer |
    /// |---|---|---|
    /// | absent/false | unary or fallback | one [`RpcResponse`] — unchanged |
    /// | absent/false | streaming | one [`RpcResponse`], [`CODE_STREAM_REQUIRED`] |
    /// | true | streaming | the stream |
    /// | true | anything else | one terminal error frame, [`CODE_STREAM_UNSUPPORTED`] |
    ///
    /// A [`fallback`] is never consulted for a streaming request: [`RpcFallback`]
    /// answers with one value and has no way to produce a sequence, so a
    /// streaming request it "served" would silently become a one-item stream.
    ///
    /// The `stream` flag is read only when this router registers at least one
    /// streaming method. A router with none — every consumer that predates
    /// #6286 — cannot reach a streaming answer, so the flag cannot change what
    /// it says and is not parsed.
    ///
    /// Never returns `Err`, for the same reason [`dispatch`] does not: a refusal
    /// is a frame, so the connection always has something to write.
    ///
    /// [`dispatch`]: RpcRouter::dispatch
    /// [`fallback`]: RpcRouter::fallback
    /// [`CODE_STREAM_REQUIRED`]: super::CODE_STREAM_REQUIRED
    /// [`CODE_STREAM_UNSUPPORTED`]: super::CODE_STREAM_UNSUPPORTED
    ///
    /// Test: `dispatch_streaming_answers_a_unary_request_unchanged`,
    /// `stream_round_trips_many_frames_over_a_real_socket`,
    /// `stream_request_for_a_non_streaming_method_is_refused`,
    /// `unary_request_for_a_streaming_method_is_refused_in_one_frame`,
    /// `stream_opt_in_is_read_from_the_request_frame`.
    pub async fn dispatch_streaming(&self, frame: &[u8]) -> RpcOutcome {
        let request = match self.envelope(frame) {
            Ok(request) => request,
            Err(response) => return RpcOutcome::Single(response),
        };

        if self.streams.is_empty() {
            return RpcOutcome::Single(self.call_unary(request).await);
        }

        let wants_stream = serde_json::from_slice::<StreamOptIn>(frame)
            .map(|opt_in| opt_in.stream)
            .unwrap_or(false);

        match (wants_stream, self.streams.get(&request.method)) {
            (true, Some(handler)) => {
                // Cloned out of the map before the await for the same reason the
                // unary arm does it: nothing borrowed from `self` is held across
                // a handler call.
                let handler = Arc::clone(handler);
                match handler.call(request.params).await {
                    Ok(items) => RpcOutcome::Stream {
                        id: request.id,
                        items,
                    },
                    Err(error) => RpcOutcome::refused(request.id, error),
                }
            }
            (true, None) => {
                let streaming: Vec<&str> = self.stream_names().collect();
                RpcOutcome::refused(
                    request.id,
                    RpcError::stream_unsupported(&request.method, &streaming),
                )
            }
            (false, Some(_)) => {
                let method = request.method.clone();
                RpcOutcome::Single(RpcResponse::failure(
                    request.id,
                    RpcError::stream_required(&method),
                ))
            }
            (false, None) => RpcOutcome::Single(self.call_unary(request).await),
        }
    }

    /// Parse one frame and check its envelope, or produce the refusal frame.
    ///
    /// Split out so [`dispatch`] and [`dispatch_streaming`] share exactly one
    /// copy of the parse and version checks rather than drifting apart.
    ///
    /// [`dispatch`]: RpcRouter::dispatch
    /// [`dispatch_streaming`]: RpcRouter::dispatch_streaming
    ///
    /// Test: `dispatch_rejects_an_unparseable_frame`,
    /// `dispatch_rejects_a_wrong_jsonrpc_version`.
    fn envelope(&self, frame: &[u8]) -> Result<RpcRequest, RpcResponse> {
        let request: RpcRequest = match serde_json::from_slice(frame) {
            Ok(r) => r,
            Err(e) => {
                return Err(RpcResponse::failure(
                    serde_json::Value::Null,
                    RpcError::new(CODE_PARSE_ERROR, format!("unparseable request frame: {e}")),
                ));
            }
        };

        if request.jsonrpc != JSONRPC_VERSION {
            return Err(RpcResponse::failure(
                request.id,
                RpcError::new(
                    CODE_INVALID_REQUEST,
                    format!(
                        "unsupported jsonrpc version {:?}; this listener speaks {JSONRPC_VERSION}",
                        request.jsonrpc
                    ),
                ),
            ));
        }

        Ok(request)
    }

    /// Run one already-checked request against the unary method table, falling
    /// back where a fallback is mounted.
    async fn call_unary(&self, request: RpcRequest) -> RpcResponse {
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
