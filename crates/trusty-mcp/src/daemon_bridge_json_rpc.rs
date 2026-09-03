//! One shared stdio↔UDS JSON-RPC forwarder (#6316).
//!
//! Why: trusty-memory's `commands/serve_stdio_bridge.rs` and trusty-analyze's
//! `mcp/stdio.rs` each grew their own copy of the same three things — refuse a
//! streaming method, rewrite `jsonrpc` to `"2.0"` before forwarding, dial the
//! daemon's Unix socket for one framed exchange. Two copies drift, and the way
//! they drift is silent: #6286 added `memory.activity_stream` to the daemon and
//! not to the bridge's refusal list, so an MCP client calling it waited forever
//! for a frame that was never coming. This module is the one copy those two
//! crates re-point onto.
//!
//! Until #6316 the shared crate could not own this: `trusty-common` depended on
//! `trusty-mcp` through its `tickets` feature, so a `trusty-mcp` →
//! `trusty-common` edge closed a cycle. PR #6726 removed that back-edge.
//!
//! What: [`DaemonBridgeJsonRpc`] wraps a [`UdsBridgeConfig`] and answers one
//! [`crate::Request`] at a time — suppress a notification, refuse a streaming
//! method, rewrite the envelope, forward it over
//! [`trusty_common::uds::send_framed_request_capped`], and map the daemon's
//! answer back to a [`crate::Response`]. [`DaemonBridgeJsonRpc::run_stdio`]
//! feeds that into [`crate::run_stdio_loop`].
//!
//! Nothing here is specific to one daemon. The streaming-method list, the
//! per-request timeout, the frame budget, and the label that appears in error
//! text are all the caller's; the envelope rewriting a caller needs on top (the
//! `--palace` default and caller-identity stamping trusty-memory injects) goes
//! in through [`DaemonBridgeJsonRpc::with_request_rewriter`].
//!
//! ## Failures are answers, never silence
//!
//! A daemon that is not listening, a daemon that never replies, and a daemon
//! that replies with something that is not a JSON-RPC response all produce a
//! JSON-RPC error response **carrying the request's own id**. None of them
//! produce an empty result, and none of them end the loop: an id-less or absent
//! answer is indistinguishable from a hang to a client that matches responses
//! to requests by id (#6309), and the next request may well succeed.
//!
//! STDOUT hygiene: nothing here writes to stdout except the JSON-RPC channel
//! itself. Diagnostics go to stderr.
//!
//! Test: `tests/daemon_bridge_json_rpc_uds.rs` drives the whole surface against
//! a real `UnixListener`; the unit tests below cover the pure helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::{Request, Response, error_codes};

/// Per-request forwarding budget when the caller states none.
///
/// Why: 60 s is trusty-memory's figure — headroom for a cold-start embedding
/// without letting one hung request wedge the stdio loop.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// What the forwarder needs to know about the daemon behind the socket.
///
/// Why: the values that differ between trusty-memory, trusty-analyze and
/// trusty-search are exactly these five. Making them the caller's is what lets
/// one forwarder serve all of them; hardcoding any of them is what produced the
/// per-crate copies this module replaces.
/// What: `socket` is the daemon's Unix socket; `daemon_label` names the daemon
/// in error text a human reads; `streaming_methods` is the refusal list (see
/// [`DaemonBridgeJsonRpc::answer`]); `request_timeout` and `max_frame_bytes` are
/// passed through to [`trusty_common::uds::send_framed_request_capped`].
/// Test: `config_defaults_are_the_documented_ones`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UdsBridgeConfig {
    /// The daemon's Unix socket. Dialled fresh for each forwarded request.
    pub socket: PathBuf,
    /// How the daemon is named in error messages (e.g. `"trusty-memory"`).
    pub daemon_label: String,
    /// Methods the daemon answers in many frames, which MCP stdio cannot carry.
    pub streaming_methods: Vec<String>,
    /// Ceiling on one dial-write-read exchange.
    pub request_timeout: Duration,
    /// Response-frame budget handed to the framed client.
    pub max_frame_bytes: u64,
}

impl UdsBridgeConfig {
    /// A config with the default timeout, the shared frame budget, and no
    /// streaming methods.
    pub fn new(socket: impl Into<PathBuf>, daemon_label: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
            daemon_label: daemon_label.into(),
            streaming_methods: Vec::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_frame_bytes: trusty_common::uds::MAX_FRAME_BYTES,
        }
    }

    /// Set the methods this daemon streams, which the bridge refuses.
    ///
    /// This list must equal the daemon router's own stream-method registration.
    /// The bridge refuses before it dials, so it cannot ask the router what was
    /// registered — a method the daemon streams and this list omits is
    /// forwarded as an ordinary call and the client waits forever (#6286).
    #[must_use]
    pub fn with_streaming_methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.streaming_methods = methods.into_iter().map(Into::into).collect();
        self
    }

    /// Replace the per-request forwarding budget.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Replace the response-frame budget.
    #[must_use]
    pub fn with_max_frame_bytes(mut self, max_frame_bytes: u64) -> Self {
        self.max_frame_bytes = max_frame_bytes;
        self
    }
}

/// A caller-supplied rewrite applied to each request envelope before forwarding.
type RequestRewriter = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// The forwarder: stdio JSON-RPC in, framed UDS JSON-RPC out.
///
/// Why: see the module docs — one implementation of the refuse/normalise/
/// forward/map sequence, so a fix to any of the four lands once.
/// What: holds a [`UdsBridgeConfig`] and an optional envelope rewriter.
/// [`Self::answer`] is the per-request seam (what tests drive);
/// [`Self::run_stdio`] wires that seam into [`crate::run_stdio_loop`].
/// Test: `tests/daemon_bridge_json_rpc_uds.rs`.
pub struct DaemonBridgeJsonRpc {
    config: UdsBridgeConfig,
    rewriter: Option<RequestRewriter>,
}

impl std::fmt::Debug for DaemonBridgeJsonRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonBridgeJsonRpc")
            .field("config", &self.config)
            .field("rewriter", &self.rewriter.is_some())
            .finish()
    }
}

impl DaemonBridgeJsonRpc {
    /// Build a forwarder that forwards each envelope unchanged.
    pub fn new(config: UdsBridgeConfig) -> Self {
        Self {
            config,
            rewriter: None,
        }
    }

    /// Rewrite each request envelope before it is forwarded.
    ///
    /// Why: trusty-memory injects a `--palace` default and its own resolved
    /// caller identity into every forwarded `tools/call` (DOC-53 §4.3). That is
    /// genuinely memory's business, not the transport's — but it has to happen
    /// between "the client sent this" and "the socket receives this", which is
    /// inside this module. The hook keeps the policy at the consumer without
    /// forcing the consumer to own a second copy of the transport.
    /// What: `rewrite` runs after [`crate::Request`] is serialised and after
    /// `jsonrpc` is normalised, so a rewriter that drops the field cannot
    /// un-normalise it — the field is re-stamped on the way out.
    /// Test: `a_rewriter_sees_the_envelope_and_its_edit_reaches_the_daemon`.
    #[must_use]
    pub fn with_request_rewriter<F>(mut self, rewrite: F) -> Self
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.rewriter = Some(Arc::new(rewrite));
        self
    }

    /// The socket this forwarder dials.
    pub fn socket(&self) -> &Path {
        &self.config.socket
    }

    /// Answer exactly one MCP request.
    ///
    /// Why: the per-request seam. Splitting it out of the stdio closure is what
    /// makes the three failure arms testable without a real stdin, and what
    /// keeps the id-carrying discipline of #6309 in one place rather than in a
    /// closure body.
    /// What: a notification (no `id`, or a `notifications/*` method) is
    /// suppressed per MCP §4.1. A streaming method — checked through the
    /// `tools/call` envelope as well as the outer `method` — is refused with
    /// `INVALID_REQUEST` rather than forwarded, because
    /// [`crate::run_stdio_loop`] writes exactly one response per request and
    /// has no frame sequence to put a token stream in. Everything else is
    /// normalised, rewritten, forwarded, and mapped back.
    ///
    /// A daemon that answered with a JSON-RPC error is a success here: that
    /// error is the answer and reaches the client unaltered. Only a transport
    /// failure or a reply that is not a JSON-RPC response becomes an error of
    /// this bridge's own making, and each of those names its cause and carries
    /// the request's id.
    /// Test: `a_dead_socket_answers_with_an_error_naming_the_daemon`,
    /// `a_silent_daemon_answers_with_a_timeout_error`,
    /// `a_malformed_daemon_reply_is_reported_rather_than_passed_through`,
    /// `a_streaming_method_is_refused_before_the_socket_is_dialled`.
    pub async fn answer(&self, req: Request) -> Response {
        // MCP §4.1: an id-less request gets no reply. Decided from the REQUEST,
        // before the daemon is touched — forwarding a notification would earn a
        // response frame that corrupts the stdio channel.
        if is_notification(&req) {
            return Response::suppressed();
        }

        // #6309: captured before the forward. An error frame with no id matches
        // no pending call, so the client waits instead of failing.
        let id = req.id.clone();

        let envelope = normalise_jsonrpc(request_to_value(&req));

        if let Some(method) = effective_method(&envelope)
            && self.config.streaming_methods.iter().any(|m| m == method)
        {
            return Response::err(
                id,
                error_codes::INVALID_REQUEST,
                format!(
                    "{method} answers as a stream, which MCP stdio cannot carry \
                     (one response per request). Dial {} directly with a framed \
                     streaming client to read it.",
                    self.config.socket.display()
                ),
            );
        }

        // The rewriter runs on the normalised envelope, and `jsonrpc` is
        // re-stamped afterwards so a rewriter cannot undo the normalisation the
        // daemon's router requires.
        let envelope = match &self.rewriter {
            Some(rewrite) => normalise_jsonrpc(rewrite(envelope)),
            None => envelope,
        };

        match self.forward(&envelope).await {
            Ok(reply) => self.map_reply(id, reply),
            Err(cause) => {
                // Stderr only: stdout is the JSON-RPC channel. The crate has no
                // `tracing` dependency and `daemon_bridge` reports the same way.
                eprintln!("daemon bridge: transport error: {cause}");
                Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    format!(
                        "the {} daemon at {} could not be reached: {cause}",
                        self.config.daemon_label,
                        self.config.socket.display()
                    ),
                )
            }
        }
    }

    /// One framed exchange on the daemon's socket.
    ///
    /// The reply is decoded as an untyped [`Value`] rather than a concrete
    /// response struct: the bridge forwards envelopes it does not interpret, and
    /// [`Self::map_reply`] is where the shape is checked. A reply that is not
    /// JSON at all fails here, as `UdsRpcError::Decode`.
    async fn forward(&self, envelope: &Value) -> Result<Value, trusty_common::uds::UdsRpcError> {
        trusty_common::uds::send_framed_request_capped(
            &self.config.socket,
            envelope,
            self.config.request_timeout,
            self.config.max_frame_bytes,
        )
        .await
    }

    /// Map the daemon's reply onto the response this bridge emits.
    ///
    /// Why: the client's `jsonrpc` and `id` are this bridge's contract, not the
    /// daemon's. [`Response::ok`] and [`Response::err`] both stamp
    /// `jsonrpc: "2.0"`, so whatever the daemon put in that field, the frame
    /// reaching the client carries the one version MCP stdio speaks.
    /// What: prefers the daemon's echoed `id` when it is non-null and falls back
    /// to the request's own, so a daemon that dropped the id still produces a
    /// matchable answer. A reply that is not a JSON object, or that carries
    /// neither `result` nor `error`, is reported as an error naming the daemon
    /// — never passed through as an empty result.
    /// Test: `a_malformed_daemon_reply_is_reported_rather_than_passed_through`,
    /// `a_daemon_error_reaches_the_client_unaltered`.
    fn map_reply(&self, request_id: Option<Value>, reply: Value) -> Response {
        let id = reply
            .get("id")
            .cloned()
            .filter(|v| !v.is_null())
            .or(request_id);

        if let Some(result) = reply.get("result").cloned() {
            return Response::ok(id, result);
        }

        if let Some(error) = reply.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .map(|c| c as i32)
                .unwrap_or(error_codes::INTERNAL_ERROR);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown daemon error");
            return Response::err(id, code, message);
        }

        Response::err(
            id,
            error_codes::INTERNAL_ERROR,
            format!(
                "the {} daemon at {} replied with something that is not a JSON-RPC \
                 response (no result and no error): {reply}",
                self.config.daemon_label,
                self.config.socket.display()
            ),
        )
    }

    /// Run the MCP stdio loop, forwarding every request to the daemon.
    ///
    /// Why: the entry point a `serve --stdio` command calls. It owns the loop so
    /// a consumer's command function is the config plus this one call.
    /// What: hands [`Self::answer`] to [`crate::run_stdio_loop`], which reads
    /// line-delimited JSON from stdin and writes one response per non-suppressed
    /// request to stdout. Returns `Ok(())` when stdin reaches EOF — the client
    /// closing the pipe is how an MCP server is told to exit (#457). Readiness
    /// of the daemon is the caller's to establish before calling this; a request
    /// that arrives with nothing listening is answered with an error, not a
    /// crash.
    ///
    /// # Errors
    ///
    /// Only an I/O failure on stdin or stdout. A daemon failure is a response,
    /// not an `Err` — see [`Self::answer`].
    ///
    /// Test: `a_request_is_forwarded_and_the_reply_comes_back` covers the
    /// forwarding this wires up; `stdio_loop_exits_on_eof` covers the loop.
    pub async fn run_stdio(self) -> anyhow::Result<()> {
        let bridge = Arc::new(self);
        crate::run_stdio_loop(move |req| {
            let bridge = Arc::clone(&bridge);
            async move { bridge.answer(req).await }
        })
        .await
    }
}

/// Run the forwarder over stdio with no envelope rewriting.
///
/// Why: the common case — trusty-analyze and trusty-search need the transport
/// and nothing else, so they should not have to name a builder to get it.
/// What: [`DaemonBridgeJsonRpc::new`] followed by
/// [`DaemonBridgeJsonRpc::run_stdio`].
///
/// # Errors
///
/// Only an I/O failure on stdin or stdout.
///
/// Test: `a_request_is_forwarded_and_the_reply_comes_back`.
pub async fn run_stdio_bridge(config: UdsBridgeConfig) -> anyhow::Result<()> {
    DaemonBridgeJsonRpc::new(config).run_stdio().await
}

/// True when the MCP spec forbids answering this request.
///
/// §4.1: a notification carries no `id`, and the `notifications/*` methods are
/// notifications by name even when a client attaches one.
fn is_notification(req: &Request) -> bool {
    req.id.is_none() || req.method.starts_with("notifications/")
}

/// Serialise a [`Request`] into the envelope that goes on the wire.
///
/// Infallible in practice — [`Request`] is always serialisable — and an empty
/// object on the impossible arm, which the daemon's router rejects with a parse
/// error rather than mis-executing.
fn request_to_value(req: &Request) -> Value {
    serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}))
}

/// Stamp `jsonrpc: "2.0"` on an outgoing envelope (#6286).
///
/// Why: [`Request`] declares `jsonrpc: Option<String>` and serialises it as
/// `null` when the client omitted the field. `trusty_common::uds::server::
/// RpcRouter` refuses any frame whose `jsonrpc` is not exactly `"2.0"`, so
/// without this a request a client sends today becomes a parse error for a
/// reason nothing in its body explains. Rewriting rather than refusing is
/// deliberate: a version the client never set is not a thing to fail it on.
/// Test: `an_absent_jsonrpc_is_normalised`, `a_wrong_jsonrpc_is_normalised`.
fn normalise_jsonrpc(mut envelope: Value) -> Value {
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    }
    envelope
}

/// The method a request will actually run, seeing through `tools/call`.
///
/// Why: a streamed method arrives either way — bare as `{"method":
/// "memory.chat"}`, or wrapped as `tools/call` with `params.name`. Checking only
/// the outer field lets the wrapped form through, which is the silent-hang case.
/// Test: `a_streaming_method_is_refused_before_the_socket_is_dialled`.
fn effective_method(envelope: &Value) -> Option<&str> {
    let method = envelope.get("method")?.as_str()?;
    if method == "tools/call" {
        return envelope
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .or(Some(method));
    }
    Some(method)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> UdsBridgeConfig {
        UdsBridgeConfig::new("/tmp/absent.sock", "trusty-test")
    }

    /// Why: the defaults are part of the contract a consumer relies on when it
    /// names neither a timeout nor a budget.
    /// What: asserts the constructed defaults are the documented ones.
    /// Test: this test.
    #[test]
    fn config_defaults_are_the_documented_ones() {
        let c = config();
        assert_eq!(c.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(c.max_frame_bytes, trusty_common::uds::MAX_FRAME_BYTES);
        assert!(c.streaming_methods.is_empty());
    }

    /// Why: the #6286 trap — an omitted `jsonrpc` serialises as `null` and the
    /// daemon's router refuses it.
    /// What: an envelope with no `jsonrpc` gains `"2.0"`.
    /// Test: this test.
    #[test]
    fn an_absent_jsonrpc_is_normalised() {
        let out = normalise_jsonrpc(json!({"id": 1, "method": "ping"}));
        assert_eq!(out["jsonrpc"], "2.0");
    }

    /// Why: a client that sends a version this transport does not speak should
    /// still be forwarded, not failed on a field it did not choose.
    /// What: `jsonrpc: "1.0"` and `jsonrpc: null` both become `"2.0"`.
    /// Test: this test.
    #[test]
    fn a_wrong_jsonrpc_is_normalised() {
        assert_eq!(
            normalise_jsonrpc(json!({"jsonrpc": "1.0", "method": "ping"}))["jsonrpc"],
            "2.0"
        );
        assert_eq!(
            normalise_jsonrpc(json!({"jsonrpc": null, "method": "ping"}))["jsonrpc"],
            "2.0"
        );
    }

    /// Why: the wrapped form is the one a real MCP client sends, and missing it
    /// is what makes a streamed call hang instead of erroring.
    /// What: both the bare method and the `tools/call` envelope resolve to the
    /// same name; a `tools/call` with no `params.name` falls back to the outer.
    /// Test: this test.
    #[test]
    fn effective_method_sees_through_the_tools_call_envelope() {
        assert_eq!(
            effective_method(&json!({"method": "memory.chat"})),
            Some("memory.chat")
        );
        assert_eq!(
            effective_method(&json!({
                "method": "tools/call",
                "params": {"name": "memory.chat"}
            })),
            Some("memory.chat")
        );
        assert_eq!(
            effective_method(&json!({"method": "tools/call"})),
            Some("tools/call")
        );
        assert_eq!(effective_method(&json!({"id": 1})), None);
    }

    /// Why: MCP §4.1 — replying to a notification corrupts the stdio channel.
    /// What: an id-less request and a `notifications/*` request are both
    /// notifications; an ordinary call is not.
    /// Test: this test.
    #[test]
    fn notifications_are_recognised_before_the_daemon_is_touched() {
        let bare = Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "ping".into(),
            params: None,
        };
        assert!(is_notification(&bare));

        let named = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(is_notification(&named));

        let call = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "ping".into(),
            params: None,
        };
        assert!(!is_notification(&call));
    }

    /// Why: a notification must produce no wire write at all.
    /// What: `answer` returns the suppressed sentinel without dialling — the
    /// configured socket does not exist, so a dial would surface as an error.
    /// Test: this test.
    #[tokio::test]
    async fn a_notification_is_suppressed_without_dialling() {
        let bridge = DaemonBridgeJsonRpc::new(config());
        let resp = bridge
            .answer(Request {
                jsonrpc: Some("2.0".into()),
                id: None,
                method: "ping".into(),
                params: None,
            })
            .await;
        assert!(resp.suppress);
    }

    /// Why: the daemon's error is the answer; wrapping it would hide the code
    /// and message the client needs.
    /// What: `map_reply` passes a daemon error through with its own code.
    /// Test: this test.
    #[test]
    fn a_daemon_error_reaches_the_client_unaltered() {
        let bridge = DaemonBridgeJsonRpc::new(config());
        let resp = bridge.map_reply(
            Some(json!(7)),
            json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "no such tool"}}),
        );
        let err = resp.error.expect("the daemon's error survives the hop");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(err.message, "no such tool");
        assert_eq!(resp.id, Some(json!(7)));
        assert_eq!(resp.jsonrpc, "2.0");
    }

    /// Why: a daemon that dropped the id would otherwise produce an unmatchable
    /// answer, which a client cannot tell from a hang (#6309).
    /// What: a reply with a null id falls back to the request's own id.
    /// Test: this test.
    #[test]
    fn a_null_reply_id_falls_back_to_the_requests_own() {
        let bridge = DaemonBridgeJsonRpc::new(config());
        let resp = bridge.map_reply(Some(json!(42)), json!({"id": null, "result": {"ok": true}}));
        assert_eq!(resp.id, Some(json!(42)));
    }
}
