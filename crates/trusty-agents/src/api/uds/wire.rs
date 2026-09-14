//! The `agents.request` wire types (#6433 slice 2).
//!
//! Why: the `--serve`/`--api` daemon carries 47 routes across 20 handler
//! modules. Retyping each as its own JSON-RPC method would restate the route
//! table in a second place, where the two can drift; the owner's ruling on
//! #6433 took the other branch — one method that carries an HTTP exchange, so
//! the axum `Router` stays the single definition of what this daemon answers.
//!
//! What: one request frame and one response frame, both plain JSON. A body is
//! carried base64-encoded because the router answers bytes, not text: the
//! embedded UI's assets are binary and a `String` field would either corrupt
//! them or force a second encoding path for the JSON routes.
//!
//! Test: `request_frame_round_trips`, `response_frame_round_trips`,
//! `body_bytes_survive_a_base64_round_trip`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

/// The one method that carries an HTTP exchange into the axum router.
pub const METHOD_REQUEST: &str = "agents.request";

/// Liveness method, answered without touching the router.
///
/// Registered with `RpcRouter::typed_liveness` for the reason #6621 gives:
/// every caller of a health method is a monitor, and answering one must not
/// count as the traffic that keeps a serve loop alive.
pub const METHOD_HEALTH: &str = "agents.health";

/// One HTTP request, as it crosses the socket.
///
/// Why: `path` carries its own query string rather than splitting it into a
/// separate field — axum matches on the full URI and a caller that had to
/// re-join the two would be re-implementing a parser that already exists.
/// What: `method` is an HTTP method token (`GET`, `POST`, …). `headers` is a
/// list, not a map, because HTTP permits repeats (`Set-Cookie`, `Accept`).
/// `body_b64` is `None` for a bodyless request.
/// Test: `request_frame_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HttpRequestFrame {
    /// HTTP method token, e.g. `GET`.
    pub method: String,
    /// Path plus any query string, e.g. `/api/agents?all=1`.
    pub path: String,
    /// Request headers, in order, duplicates preserved.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64 of the raw request body, or `None` when there is none.
    #[serde(default)]
    pub body_b64: Option<String>,
}

impl HttpRequestFrame {
    /// Build a bodyless request frame.
    pub fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            headers: Vec::new(),
            body_b64: None,
        }
    }

    /// Attach a raw body, base64-encoding it.
    #[must_use]
    pub fn with_body(mut self, body: &[u8]) -> Self {
        self.body_b64 = Some(BASE64.encode(body));
        self
    }

    /// Attach a JSON body and the matching `content-type`.
    ///
    /// # Errors
    ///
    /// When `value` cannot be serialized.
    pub fn with_json<T: Serialize>(self, value: &T) -> Result<Self, serde_json::Error> {
        let bytes = serde_json::to_vec(value)?;
        let mut frame = self.with_body(&bytes);
        frame
            .headers
            .push(("content-type".to_string(), "application/json".to_string()));
        Ok(frame)
    }

    /// Decode the body back to raw bytes.
    ///
    /// # Errors
    ///
    /// When `body_b64` is present but is not valid base64.
    pub fn body_bytes(&self) -> Result<Vec<u8>, base64::DecodeError> {
        match self.body_b64.as_deref() {
            Some(encoded) => BASE64.decode(encoded),
            None => Ok(Vec::new()),
        }
    }
}

/// One HTTP response, as it crosses the socket.
///
/// Test: `response_frame_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HttpResponseFrame {
    /// HTTP status code.
    pub status: u16,
    /// Response headers, in order, duplicates preserved.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64 of the raw response body.
    #[serde(default)]
    pub body_b64: String,
}

impl HttpResponseFrame {
    /// Build a response frame from a status and raw body bytes.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: &[u8]) -> Self {
        Self {
            status,
            headers,
            body_b64: BASE64.encode(body),
        }
    }

    /// Decode the body back to raw bytes.
    ///
    /// # Errors
    ///
    /// When `body_b64` is not valid base64.
    pub fn body_bytes(&self) -> Result<Vec<u8>, base64::DecodeError> {
        BASE64.decode(&self.body_b64)
    }

    /// Decode the body and parse it as JSON.
    ///
    /// # Errors
    ///
    /// When the body is not valid base64, or does not parse as JSON.
    pub fn json(&self) -> anyhow::Result<serde_json::Value> {
        let bytes = self.body_bytes()?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Whether `status` is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// What `agents.health` answers.
///
/// Why a separate shape from `GET /api/health`: that route goes through the
/// router and reports the daemon's build identity; this one proves only that a
/// serve loop is on the other end of the socket, which is the whole question a
/// liveness probe asks. Keeping them distinct is what lets the health method be
/// exempt from idle accounting without exempting a real route.
/// Test: `health_answers_over_a_real_socket`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HealthResponse {
    /// Always `"ok"`.
    pub status: String,
    /// The daemon's crate version.
    pub version: String,
}

impl HealthResponse {
    /// Build the fixed health answer for this build.
    pub fn current() -> Self {
        Self {
            status: "ok".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Params for a method that takes none.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoParams {}
