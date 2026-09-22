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
//! ## Recoverable inside one client session (#8351)
//!
//! An MCP client launches this bridge once and never re-spawns it. Three
//! properties keep a session alive across a daemon outage:
//!
//! 1. [`DaemonBridgeJsonRpc::with_local_handler`] answers chosen methods — the
//!    `initialize`/`tools/list` handshake, in practice — from THIS process, so
//!    a daemon that is down at handshake does not make the client mark the
//!    server failed for the rest of the session.
//! 2. Every forwarded request dials afresh, so a tool call that failed while
//!    the daemon was down succeeds on the next call after it returns, with no
//!    bridge restart.
//! 3. [`DaemonBridgeJsonRpc::with_socket_resolver`] re-resolves the daemon's
//!    address per request rather than trusting the one resolved at process
//!    start, so a bridge that resolved a stale or wrong path heals too.
//!
//! The bridge still never starts a daemon (#1152): recovering means answering
//! what it can and reporting what it cannot, not spawning anything.
//!
//! STDOUT hygiene: nothing here writes to stdout except the JSON-RPC channel
//! itself. Diagnostics go to stderr.
//!
//! Test: `tests/daemon_bridge_json_rpc_uds.rs` drives the whole surface against
//! a real `UnixListener`; the unit tests below cover the pure helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use trusty_common::uds::ConnectRetry;

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
/// passed through to [`trusty_common::uds::send_framed_request_retrying`].
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
    /// Build identifier for the bridge, quoted in transport-error text (#8351).
    pub bridge_version: String,
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
            // #8351: this crate's version unless the consumer names its own.
            bridge_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Name the consumer's own build in transport-error text (#8351).
    ///
    /// Why: the #8351 incident produced an error naming a socket path no code
    /// on the host could construct, and the binary that produced it had already
    /// been replaced — so the report could not be attributed to a build. An
    /// error that carries the bridge's version is attributable from its text
    /// alone. The default is this crate's version, which is right for the
    /// generic `trusty-mcp <service>` binary and wrong for a consumer that
    /// builds its own bridge, so a consumer passes `env!("CARGO_PKG_VERSION")`.
    /// Test: `a_named_bridge_version_appears_in_the_transport_error`.
    #[must_use]
    pub fn with_bridge_version(mut self, version: impl Into<String>) -> Self {
        self.bridge_version = version.into();
        self
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

/// A caller-supplied resolver run once per forwarded request (#8351).
type SocketResolver = Arc<dyn Fn() -> anyhow::Result<PathBuf> + Send + Sync>;

/// A caller-supplied answer produced without the daemon (#8351).
type LocalHandler = Arc<dyn Fn(&Request) -> Option<Value> + Send + Sync>;

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
    /// #8351: re-resolves the socket per request. See [`Self::resolve_socket`].
    resolver: Option<SocketResolver>,
    /// #8351: answers chosen methods in-process. See [`Self::answer`].
    local: Option<LocalHandler>,
    /// #8267: true until the first request is forwarded. See [`Self::forward`].
    first_dial: AtomicBool,
}

impl std::fmt::Debug for DaemonBridgeJsonRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonBridgeJsonRpc")
            .field("config", &self.config)
            .field("rewriter", &self.rewriter.is_some())
            .field("resolver", &self.resolver.is_some())
            .field("local", &self.local.is_some())
            .finish()
    }
}

impl DaemonBridgeJsonRpc {
    /// Build a forwarder that forwards each envelope unchanged.
    pub fn new(config: UdsBridgeConfig) -> Self {
        Self {
            config,
            rewriter: None,
            resolver: None,
            local: None,
            first_dial: AtomicBool::new(true),
        }
    }

    /// Re-resolve the daemon's socket for every forwarded request (#8351).
    ///
    /// Why: a bridge lives for a whole client session, and a `PathBuf` baked in
    /// at process start is a fact that can go stale — a data-directory move, an
    /// override that was set for one process, or a resolution that was simply
    /// wrong. A stale path cannot heal while the process lives, so the session
    /// stays broken even after the daemon is healthy. Re-resolving costs one
    /// path join per request and turns "wrong forever" into "wrong until the
    /// next call".
    /// What: `resolve` runs before each forward and its answer is used for the
    /// dial AND for the error text, so the path a failure names is the path the
    /// bridge actually tried. A resolver that fails is reported as an error
    /// carrying the request's id — never silently downgraded to the configured
    /// path, which is the fallback ONLY when no resolver is attached.
    /// Test: `a_resolver_is_consulted_on_every_request`,
    /// `a_failing_resolver_is_an_error_not_a_fallback`.
    #[must_use]
    pub fn with_socket_resolver<F>(mut self, resolve: F) -> Self
    where
        F: Fn() -> anyhow::Result<PathBuf> + Send + Sync + 'static,
    {
        self.resolver = Some(Arc::new(resolve));
        self
    }

    /// Answer chosen methods in this process instead of forwarding them (#8351).
    ///
    /// Why: an MCP client that cannot complete `initialize` marks the server
    /// failed for the whole session and never re-spawns it, so one moment of
    /// daemon downtime at handshake costs the session its tools — which is what
    /// #8351 reported. trusty-search has never had that failure mode because it
    /// answers the handshake in-process and forwards only tool bodies. This is
    /// that seam, made available to a forwarding bridge.
    /// What: `answer_locally` runs after notification suppression and before
    /// anything is normalised, rewritten, resolved or dialled. `Some(result)`
    /// becomes the response's `result` (carrying the request's own id);
    /// `None` means "not mine" and the request is forwarded as before. The
    /// handler must therefore answer only methods whose answer does not depend
    /// on daemon state.
    ///
    /// Drift is the consumer's to prevent: the values a handler returns have to
    /// come from the same in-process table the daemon serves them from, or the
    /// two answers diverge silently.
    /// Test: `a_local_handler_answers_without_a_daemon`,
    /// `a_local_handler_that_declines_still_forwards`.
    #[must_use]
    pub fn with_local_handler<F>(mut self, answer_locally: F) -> Self
    where
        F: Fn(&Request) -> Option<Value> + Send + Sync + 'static,
    {
        self.local = Some(Arc::new(answer_locally));
        self
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

    /// The configured socket.
    ///
    /// #8351: this is the CONFIGURED path, which is also the dialled one only
    /// while no resolver is attached — see [`Self::with_socket_resolver`].
    pub fn socket(&self) -> &Path {
        &self.config.socket
    }

    /// The socket for the next forward, resolved now (#8351).
    ///
    /// Why: split out so the resolver's failure is one decision with one error
    /// arm, rather than an `unwrap_or` at the dial site that would silently
    /// dial the configured path after the resolver said it could not answer.
    /// What: the resolver's answer when one is attached, the configured socket
    /// otherwise. A resolver error is returned, never swallowed.
    /// Test: `a_failing_resolver_is_an_error_not_a_fallback`.
    fn resolve_socket(&self) -> Result<PathBuf, String> {
        match &self.resolver {
            // `{:#}` so an `anyhow` chain reaches the client, not just its head.
            Some(resolve) => resolve().map_err(|e| format!("{e:#}")),
            None => Ok(self.config.socket.clone()),
        }
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
    /// #8351: two arms run before any of that. A method the local handler
    /// claims is answered from this process, so the handshake survives a daemon
    /// that is down; and the socket is re-resolved, so the path dialled and the
    /// path named in a failure are both current rather than whatever startup
    /// produced.
    /// Test: `a_dead_socket_answers_with_an_error_naming_the_daemon`,
    /// `a_silent_daemon_answers_with_a_timeout_error`,
    /// `a_malformed_daemon_reply_is_reported_rather_than_passed_through`,
    /// `a_streaming_method_is_refused_before_the_socket_is_dialled`,
    /// `a_local_handler_answers_without_a_daemon`,
    /// `a_failing_resolver_is_an_error_not_a_fallback`.
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

        // #8351: answered here, never forwarded. A client that cannot complete
        // `initialize` marks the server failed for the whole session.
        if let Some(answer_locally) = &self.local
            && let Some(result) = answer_locally(&req)
        {
            return Response::ok(id, result);
        }

        let envelope = normalise_jsonrpc(request_to_value(&req));

        // #8351: resolved per request, so a bridge that resolved a stale path
        // once heals on the next call instead of for the life of the process.
        // Resolved BEFORE the streaming refusal so the path that refusal tells
        // an operator to dial is the current one.
        let socket = match self.resolve_socket() {
            Ok(socket) => socket,
            Err(cause) => {
                // Stderr only: stdout is the JSON-RPC channel.
                eprintln!("daemon bridge: socket resolution failed: {cause}");
                return Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    format!(
                        "the {} socket path could not be resolved (bridge {}): {cause}",
                        self.config.daemon_label, self.config.bridge_version
                    ),
                );
            }
        };

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
                    socket.display()
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

        match self.forward(&socket, &envelope).await {
            Ok(reply) => self.map_reply(&socket, id, reply),
            Err(cause) => {
                // Stderr only: stdout is the JSON-RPC channel. The crate has no
                // `tracing` dependency and `daemon_bridge` reports the same way.
                eprintln!("daemon bridge: transport error: {cause}");
                Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    format!(
                        // #8351: the version makes a report attributable to a
                        // build; the #8351 incident's error was not.
                        "the {} daemon at {} could not be reached (bridge {}): \
                         {cause}. The next request is dialled fresh, so this \
                         recovers on its own once the daemon is back.",
                        self.config.daemon_label,
                        socket.display(),
                        self.config.bridge_version
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
    ///
    /// #8267: the FIRST forwarded request — the MCP `initialize`, in practice —
    /// dials under [`ConnectRetry::startup`] rather than the per-request bound.
    /// An MCP client launches this bridge and the daemon it forwards to in the
    /// same instant, so the bridge's first dial can legitimately precede the
    /// daemon's own bind; giving up on it the way a hundredth dial gives up is
    /// what left a whole session with a dead memory server. Every later request
    /// uses the ordinary bound, so a genuinely absent daemon still fails fast.
    async fn forward(
        &self,
        socket: &Path,
        envelope: &Value,
    ) -> Result<Value, trusty_common::uds::UdsRpcError> {
        trusty_common::uds::send_framed_request_retrying(
            socket,
            envelope,
            self.config.request_timeout,
            self.config.max_frame_bytes,
            self.next_retry_policy(),
        )
        .await
    }

    /// Take the connect-retry bound for the next dial, consuming the
    /// first-dial flag.
    ///
    /// Split out of [`Self::forward`] so the selection is testable without a
    /// socket: asserting it through `forward` would mean spending a real
    /// startup floor on a dead path.
    ///
    /// Test: `the_first_dial_spends_the_startup_bound_and_later_dials_do_not`.
    fn next_retry_policy(&self) -> ConnectRetry {
        if self.first_dial.swap(false, Ordering::Relaxed) {
            ConnectRetry::startup()
        } else {
            ConnectRetry::per_request()
        }
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
    fn map_reply(&self, socket: &Path, request_id: Option<Value>, reply: Value) -> Response {
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
                socket.display()
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
#[path = "daemon_bridge_json_rpc_tests.rs"]
mod tests;
