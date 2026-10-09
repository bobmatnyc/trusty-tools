//! The transport under [`DaemonClient`]: HTTP or the daemon's unix socket
//! (#6288 step 1).
//!
//! Why: ADR-0032 and the 2026-09-14 owner ruling on #6288 make the daemon's
//! unix socket the only same-host transport. A sandboxed session has no
//! loopback TCP at all (Q2), and on 2026-09-14 a hung `127.0.0.1:7880` blocked
//! every `tm` command while the daemon still answered over the socket. Every
//! `DaemonClient` method was written against `reqwest`; this module is the one
//! seam that lets the same method run over either transport.
//!
//! What: [`DaemonRequest`] is an HTTP-shaped builder (`query`, `json`,
//! `timeout`, `send`) and [`DaemonResponse`] an HTTP-shaped answer (`status`,
//! `error_for_status`, `json`, `text`). Over HTTP they wrap `reqwest`. Over the
//! socket, `send` maps the verb and path onto the daemon's JSON-RPC method via
//! [`super::socket_routes`], merges path captures, query and body into one
//! `params` object, and projects the JSON-RPC error code back onto the HTTP
//! status the route would have answered.
//!
//! **No fallback.** A client built over the socket never opens a TCP
//! connection: an absent or refusing socket is a [`DaemonCallError`] naming
//! the socket path, and a route with no socket method is an error too. A stale
//! `http_addr` can name an unrelated process, so retrying over HTTP would turn
//! a dead daemon into a wrong answer (ADR-0032, #6284).
//!
//! Test: `transport_tests.rs`; end to end in `tests/tm_cli_socket.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use super::DaemonClient;
use super::config;
use super::socket_routes;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;

/// Why a daemon call did not produce a response.
///
/// Why typed: `tm doctor` and `tm status` tell "nothing is listening" apart
/// from "something answered badly", and only the transport knows which it was.
/// What: [`Self::Unreachable`] when no connection was established — a refused
/// TCP connect, or a unix socket that is absent or refuses the dial — and
/// [`Self::Failed`] for everything after a connection existed.
/// Test: `socket_call_to_an_absent_socket_is_unreachable_and_names_the_path`.
#[derive(Debug, thiserror::Error)]
pub enum DaemonCallError {
    /// No connection was established.
    #[error("trusty-mpm daemon unreachable over {target}: {source}")]
    Unreachable {
        /// The socket path or base URL that was dialled.
        target: String,
        /// Why the dial failed.
        #[source]
        source: anyhow::Error,
    },
    /// A connection existed and the exchange failed.
    #[error("trusty-mpm daemon call over {target} failed: {source}")]
    Failed {
        /// The socket path or base URL that was used.
        target: String,
        /// What went wrong.
        #[source]
        source: anyhow::Error,
    },
}

impl DaemonCallError {
    /// True when nothing accepted the connection.
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }

    /// The OS error when the dial itself was refused with permission denied
    /// (`EPERM`/`EACCES`) — a sandbox profile or a directory mode, not a
    /// stopped daemon (#6288).
    pub fn denied_os_error(&self) -> Option<&std::io::Error> {
        let (Self::Unreachable { source, .. } | Self::Failed { source, .. }) = self;
        source
            .chain()
            .filter_map(|e| e.downcast_ref::<std::io::Error>())
            .find(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    }

    /// True when the exchange outlived its timeout, on either transport.
    pub fn is_timeout(&self) -> bool {
        let Self::Failed { source, .. } = self else {
            return false;
        };
        source
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
            || matches!(
                source.downcast_ref::<trusty_common::uds::UdsRpcError>(),
                Some(trusty_common::uds::UdsRpcError::Timeout { .. })
            )
    }
}

/// A daemon request under construction, for either transport.
#[must_use = "a DaemonRequest does nothing until `send` is awaited"]
pub struct DaemonRequest {
    inner: Pending,
}

enum Pending {
    Http {
        target: String,
        builder: reqwest::RequestBuilder,
    },
    Socket(SocketCall),
}

struct SocketCall {
    socket: PathBuf,
    method: Method,
    path: String,
    params: Map<String, Value>,
    raw_body: Option<Value>,
    timeout: Duration,
    error: Option<anyhow::Error>,
}

impl DaemonRequest {
    /// Add query parameters: URL-encoded over HTTP, merged into `params` over
    /// the socket. Pass typed values (`u32`, `bool`), never pre-stringified
    /// ones: the socket decodes the route's typed struct from JSON.
    pub fn query<T: Serialize + ?Sized>(self, query: &T) -> Self {
        self.map(|b| b.query(query), |call| call.merge(query, "query"))
    }

    /// Set the JSON body: sent as-is over HTTP, merged into `params` over the
    /// socket (a route's params struct is its body plus its path and query).
    pub fn json<T: Serialize + ?Sized>(self, body: &T) -> Self {
        self.map(
            |b| b.json(body),
            |call| match serde_json::to_value(body) {
                Ok(Value::Object(map)) => call.params.extend(map),
                Ok(other) => call.raw_body = Some(other),
                Err(e) => call.error = Some(anyhow::anyhow!("encode request body: {e}")),
            },
        )
    }

    /// Bound this one request, overriding the client default.
    pub fn timeout(self, timeout: Duration) -> Self {
        self.map(|b| b.timeout(timeout), |call| call.timeout = timeout)
    }

    fn map(
        mut self,
        http: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
        socket: impl FnOnce(&mut SocketCall),
    ) -> Self {
        self.inner = match self.inner {
            Pending::Http { target, builder } => Pending::Http {
                target,
                builder: http(builder),
            },
            Pending::Socket(mut call) => {
                socket(&mut call);
                Pending::Socket(call)
            }
        };
        self
    }

    /// Send the request and read the whole answer.
    ///
    /// # Errors
    ///
    /// [`DaemonCallError::Unreachable`] when no connection was made;
    /// [`DaemonCallError::Failed`] for a route the socket does not serve, a
    /// daemon that predates the method, a timeout, or an unreadable answer. A
    /// refusal the daemon answered (404, 409, …) is NOT an error here — it is a
    /// [`DaemonResponse`] with that status, as over HTTP.
    pub async fn send(self) -> Result<DaemonResponse, DaemonCallError> {
        match self.inner {
            Pending::Http { target, builder } => send_http(target, builder).await,
            Pending::Socket(call) => call.send().await,
        }
    }
}

async fn send_http(
    target: String,
    builder: reqwest::RequestBuilder,
) -> Result<DaemonResponse, DaemonCallError> {
    let classify = |e: reqwest::Error| {
        if e.is_connect() {
            DaemonCallError::Unreachable {
                target: target.clone(),
                source: e.into(),
            }
        } else {
            DaemonCallError::Failed {
                target: target.clone(),
                source: e.into(),
            }
        }
    };
    let resp = builder.send().await.map_err(classify)?;
    let status = resp.status();
    let resume_reason = resp
        .headers()
        .get(RESUME_REASON_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.bytes().await.map_err(classify)?.to_vec();
    Ok(DaemonResponse {
        status,
        body,
        resume_reason,
    })
}

impl SocketCall {
    fn merge<T: Serialize + ?Sized>(&mut self, value: &T, what: &str) {
        match serde_json::to_value(value) {
            Ok(Value::Object(map)) => self.params.extend(map),
            // `&[("k", v), …]` serializes as an array of two-element arrays.
            Ok(Value::Array(pairs)) => {
                for pair in pairs {
                    match pair {
                        Value::Array(kv) if matches!(kv.as_slice(), [Value::String(_), _]) => {
                            if let [Value::String(k), v] = kv.as_slice() {
                                self.params.insert(k.clone(), v.clone());
                            }
                        }
                        // #6288: never drop a field silently — the daemon would
                        // run the route as if it had not been sent.
                        other => {
                            self.error = Some(anyhow::anyhow!(
                                "{what} entry {other} is not a (string key, value) pair"
                            ));
                            return;
                        }
                    }
                }
            }
            Ok(Value::Null) => {}
            Ok(other) => self.error = Some(anyhow::anyhow!("unsupported {what} shape: {other}")),
            Err(e) => self.error = Some(anyhow::anyhow!("encode {what}: {e}")),
        }
    }

    async fn send(mut self) -> Result<DaemonResponse, DaemonCallError> {
        let target = self.socket.display().to_string();
        let failed = |source: anyhow::Error| DaemonCallError::Failed {
            target: target.clone(),
            source,
        };
        if let Some(e) = self.error.take() {
            return Err(failed(e));
        }
        let Some((rpc_method, captures)) = socket_routes::resolve(&self.method, &self.path) else {
            return Err(failed(anyhow::anyhow!(
                "`{} {}` has no method on the daemon socket yet (#6288); \
                 this tm does not fall back to TCP",
                self.method,
                self.path
            )));
        };
        self.params.extend(captures);
        let params = match self.raw_body.take() {
            Some(raw) if self.params.is_empty() => raw,
            _ => Value::Object(self.params),
        };
        let frame = serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": 1,
            "method": rpc_method,
            "params": params,
        });
        let response: RpcResponse =
            trusty_common::uds::send_framed_request(&self.socket, &frame, self.timeout)
                .await
                .map_err(|e| {
                    if e.is_dial_failure() {
                        DaemonCallError::Unreachable {
                            target: target.clone(),
                            source: e.into(),
                        }
                    } else {
                        failed(e.into())
                    }
                })?;
        match (response.result, response.error) {
            (Some(result), _) => Ok(DaemonResponse {
                status: StatusCode::OK,
                body: serde_json::to_vec(&result).map_err(|e| failed(e.into()))?,
                resume_reason: None,
            }),
            (None, Some(e)) if e.code == socket_routes::CODE_METHOD_NOT_FOUND => {
                Err(failed(anyhow::anyhow!(
                    "the daemon does not serve `{rpc_method}` — it predates this tm; \
                     restart it (`tm restart`) ({})",
                    e.message
                )))
            }
            (None, Some(e)) => Ok(DaemonResponse {
                status: socket_routes::status_for_code(e.code),
                resume_reason: socket_routes::resume_reason_for_code(e.code).map(str::to_owned),
                body: e.message.into_bytes(),
            }),
            (None, None) => Err(failed(anyhow::anyhow!(
                "`{rpc_method}` answered with neither a result nor an error"
            ))),
        }
    }
}

/// A daemon answer, read whole, from either transport.
#[derive(Debug)]
pub struct DaemonResponse {
    status: StatusCode,
    body: Vec<u8>,
    resume_reason: Option<String>,
}

/// The header HTTP uses to tell the two 422 resume refusals apart.
const RESUME_REASON_HEADER: &str = "x-trusty-resume-reason";

impl DaemonResponse {
    /// Which 422 resume refusal this is: the `x-trusty-resume-reason` header
    /// over HTTP, the distinct RPC code over the socket (`workspace_missing`
    /// or `pane_gone`).
    pub fn resume_reason(&self) -> Option<&str> {
        self.resume_reason.as_deref()
    }

    /// The HTTP status, or the status the RPC error code projects onto.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// `Ok(self)` on 2xx; else an error carrying the status and the body text.
    ///
    /// # Errors
    ///
    /// On any non-2xx status.
    pub fn error_for_status(self) -> anyhow::Result<Self> {
        if self.status.is_success() {
            return Ok(self);
        }
        let text = String::from_utf8_lossy(&self.body).trim().to_string();
        if text.is_empty() {
            anyhow::bail!("{}", self.status)
        }
        anyhow::bail!("{}: {text}", self.status)
    }

    /// Decode the body as JSON.
    ///
    /// # Errors
    ///
    /// When the body is not `T`.
    pub async fn json<T: DeserializeOwned>(self) -> anyhow::Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// The body as text.
    ///
    /// # Errors
    ///
    /// Never; the signature mirrors `reqwest::Response::text`.
    pub async fn text(self) -> anyhow::Result<String> {
        Ok(String::from_utf8_lossy(&self.body).into_owned())
    }
}

impl DaemonClient {
    /// Build a client that reaches the daemon over its unix socket only.
    ///
    /// Why: #6288 step 1 — the sandboxed CLI's commands must work with no TCP
    /// at all, and must never fall back to it.
    /// What: every request is sent through [`DaemonRequest`]'s socket arm to
    /// `socket`; `base_url` becomes the `unix:` label of that path, used only
    /// in messages.
    /// Test: `socket_call_round_trips_a_mapped_route`.
    pub fn over_socket(socket: impl Into<PathBuf>) -> Self {
        let socket = socket.into();
        Self {
            base: format!("unix:{}", socket.display()),
            http: config::default_client(),
            home: None,
            socket: Some(socket),
            base_url_pinned: false,
        }
    }

    /// Resolve the daemon's socket and build a socket-only client for it.
    ///
    /// # Errors
    ///
    /// When `TRUSTY_MPM_SOCKET` is unset and the data directory cannot be
    /// resolved.
    pub fn from_resolved_socket() -> anyhow::Result<Self> {
        Ok(Self::over_socket(resolve_daemon_socket()?))
    }

    /// The socket this client dials, or `None` for an HTTP client.
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// One line naming the transport, for `tm doctor` and `tm health`.
    pub fn transport_label(&self) -> String {
        match &self.socket {
            Some(path) => format!("unix socket {}", path.display()),
            None => format!("http {}", self.base),
        }
    }

    /// The HTTP client, for the few HTTP-only verbs still to move (#6288
    /// step 2). Over the socket it is never used by any mapped route.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Start a request for `method path` (path begins with `/`).
    pub fn request(&self, method: Method, path: impl Into<String>) -> DaemonRequest {
        let path = path.into();
        let inner = match &self.socket {
            Some(socket) => Pending::Socket(SocketCall {
                socket: socket.clone(),
                method,
                path,
                params: Map::new(),
                raw_body: None,
                timeout: config::DEFAULT_REQUEST_TIMEOUT,
                error: None,
            }),
            None => Pending::Http {
                target: self.base.clone(),
                builder: self.http.request(method, format!("{}{path}", self.base)),
            },
        };
        DaemonRequest { inner }
    }

    /// `GET path`.
    pub fn get(&self, path: impl Into<String>) -> DaemonRequest {
        self.request(Method::GET, path)
    }

    /// `POST path`.
    pub fn post(&self, path: impl Into<String>) -> DaemonRequest {
        self.request(Method::POST, path)
    }

    /// `PATCH path`.
    pub fn patch(&self, path: impl Into<String>) -> DaemonRequest {
        self.request(Method::PATCH, path)
    }

    /// `DELETE path`.
    pub fn delete(&self, path: impl Into<String>) -> DaemonRequest {
        self.request(Method::DELETE, path)
    }
}

/// The socket the trusty-mpm daemon binds, as the CLI resolves it.
///
/// Why: the daemon binds `trusty_common::daemon_socket_path("trusty-mpm")`
/// (`daemon::socket::socket_path`); the CLI must compute the identical path,
/// and a test or a sandbox profile pins another with `TRUSTY_MPM_SOCKET`.
/// What: `TRUSTY_MPM_SOCKET` when set and non-blank, else the data-dir path.
/// The default contains a space on macOS (`Application Support`); it is used
/// as a path, never split.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
///
/// Test: `tm_health_status_and_doctor_work_over_the_socket_alone` pins the
/// override; the daemon side is `socket_path_is_the_product_named_socket_under_the_data_dir`.
pub fn resolve_daemon_socket() -> anyhow::Result<PathBuf> {
    if let Ok(raw) = std::env::var(trusty_common::mpm_rpc::TRUSTY_MPM_SOCKET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    trusty_common::daemon_socket_path("trusty-mpm")
}
