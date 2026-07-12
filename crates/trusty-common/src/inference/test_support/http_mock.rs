//! [`MockInferenceServer`] — an in-process axum HTTP mock for adapter tests.
//!
//! Why: the concrete HTTP adapters (#2403 OpenRouter/Fireworks/OpenAI, #2407
//! Bedrock) must be tested against a real socket that returns canned
//! chat/completions JSON AND records the request the adapter sent — without a
//! live provider or credentials. This spins up a throwaway axum server on an
//! ephemeral loopback port that answers every request with a fixed status+body
//! while capturing each inbound request (method, path, headers, JSON body) so a
//! test can assert the adapter's request translation. It is gated behind the
//! `axum-server` feature so the base `inference-client` feature pulls in NO
//! HTTP-server dependency (per the crate's axum-gating discipline).
//! What: [`MockInferenceServer::spawn`] binds `127.0.0.1:0`, serves a fixed
//! response from a fallback handler, records every request into a shared buffer
//! exposed via [`MockInferenceServer::requests`]/[`MockInferenceServer::last_request`],
//! and exposes [`MockInferenceServer::url`]; dropping it triggers graceful
//! shutdown.
//! Test: inline `tests` — `mock_serves_canned_body`, `mock_captures_request_body`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::inference::error::InferenceError;

/// One request the mock received, captured for request-translation assertions.
///
/// Why: an adapter's whole job is turning a [`crate::inference::ChatRequest`]
/// into the right wire bytes + headers; a test can only verify that if the mock
/// remembers exactly what arrived. Recording the parsed JSON body plus the
/// method/path/headers lets a test assert the outbound translation precisely.
/// What: `method`/`path` from the request line, `headers` as lowercase
/// name→value pairs, and `body` as the parsed JSON (or `None` when the body was
/// absent or not JSON).
/// Test: `mock_captures_request_body`.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    /// HTTP method (e.g. `"POST"`).
    pub method: String,
    /// Request path (e.g. `"/v1/chat/completions"`).
    pub path: String,
    /// Request headers as lowercase-name → value pairs.
    pub headers: Vec<(String, String)>,
    /// Parsed JSON request body; `None` if the body was empty or not JSON.
    pub body: Option<Value>,
}

impl CapturedRequest {
    /// Look up a request header value by (case-insensitive) name.
    ///
    /// Why: header assertions (`Authorization`, `HTTP-Referer`, `X-Title`) read
    /// cleaner as `req.header("authorization")` than by scanning the vec.
    /// What: returns the first header whose lowercased name equals `name`
    /// lowercased, or `None`.
    /// Test: `mock_captures_request_body`.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Shared, thread-safe buffer of captured requests.
type CaptureBuf = Arc<Mutex<Vec<CapturedRequest>>>;

/// A running in-process HTTP mock returning one fixed response.
///
/// Why: the smallest real HTTP surface a concrete adapter can exercise
/// end-to-end, with request capture so the adapter's translation is assertable.
/// What: owns the server task, a shutdown channel, and the shared capture
/// buffer; [`Self::url`] is the base URL (`http://127.0.0.1:<port>`). Dropping
/// the value signals graceful shutdown so no test leaks a listener.
/// Test: `mock_serves_canned_body`, `mock_captures_request_body`.
pub struct MockInferenceServer {
    url: String,
    captured: CaptureBuf,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl MockInferenceServer {
    /// Spawn a mock that answers every request with `status` + `body` and
    /// records each inbound request.
    ///
    /// Why: adapter tests script one provider response, point the adapter at
    /// [`Self::url`], then read back the captured request via [`Self::requests`].
    /// What: binds an ephemeral loopback port, serves a fallback handler that
    /// captures the request and returns the canned `status`/`body` for any
    /// method/path, and returns the running handle. Errors as
    /// [`InferenceError::Provider`] if the bind fails.
    /// Test: `mock_serves_canned_body`, `mock_captures_request_body`.
    pub async fn spawn(status: u16, body: Value) -> Result<Self, InferenceError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| InferenceError::Provider(format!("mock bind failed: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| InferenceError::Provider(format!("mock addr failed: {e}")))?;
        let url = format!("http://{addr}");

        let status = StatusCode::from_u16(status)
            .map_err(|e| InferenceError::Provider(format!("invalid mock status: {e}")))?;
        let captured: CaptureBuf = Arc::new(Mutex::new(Vec::new()));
        let app =
            Router::new()
                .fallback(canned_handler)
                .with_state((status, body, captured.clone()));

        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });

        Ok(Self {
            url,
            captured,
            shutdown: Some(tx),
            handle,
        })
    }

    /// The base URL the mock is listening on (`http://127.0.0.1:<port>`).
    ///
    /// Why: adapters need the endpoint to send requests to.
    /// What: returns the bound loopback URL.
    /// Test: `mock_serves_canned_body`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every request the mock has received so far, in arrival order.
    ///
    /// Why: request-translation assertions read the captured outbound body/headers.
    /// What: returns a clone of the capture buffer; a poisoned lock is recovered
    /// rather than propagated (test-support code must not panic the caller).
    /// Test: `mock_captures_request_body`.
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.captured
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The most recent request the mock received, if any.
    ///
    /// Why: the common single-call assertion wants just "what did the adapter
    /// send" without indexing.
    /// What: the last element of [`Self::requests`], or `None` when none yet.
    /// Test: `mock_captures_request_body`.
    pub fn last_request(&self) -> Option<CapturedRequest> {
        self.captured
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last()
            .cloned()
    }
}

impl Drop for MockInferenceServer {
    /// Signal graceful shutdown and abort the server task.
    ///
    /// Why: a test must not leak a bound port or a live task after its mock goes
    /// out of scope.
    /// What: sends on the shutdown channel (best-effort) and aborts the handle.
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

/// Fallback handler: capture the request, then return the canned response.
///
/// Why: one handler answers every method/path so an adapter can target any
/// endpoint shape (e.g. `/v1/chat/completions`) while its request is recorded.
/// What: records method/path/headers/body into the shared buffer, then returns
/// the `(status, body)` from router state as a JSON response. The `Bytes`
/// body extractor is last, per axum's body-consuming-extractor ordering rule.
/// Test: `mock_captures_request_body`.
async fn canned_handler(
    State((status, body, captured)): State<(StatusCode, Value, CaptureBuf)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> impl IntoResponse {
    let recorded = CapturedRequest {
        method: method.to_string(),
        path: uri.path().to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: serde_json::from_slice::<Value>(&raw).ok(),
    };
    captured
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(recorded);

    (status, axum::Json(body))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why: the mock must serve the exact status + body an adapter will parse.
    /// Test: itself.
    #[tokio::test]
    async fn mock_serves_canned_body() {
        let body = json!({
            "id": "gen-mock",
            "choices": [{"message": {"role": "assistant", "content": "mocked"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let server = MockInferenceServer::spawn(200, body.clone())
            .await
            .expect("spawn");
        let got: Value = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", server.url()))
            .send()
            .await
            .expect("send")
            .json()
            .await
            .expect("json");
        assert_eq!(got["id"], "gen-mock");
        assert_eq!(got["choices"][0]["message"]["content"], "mocked");
    }

    /// Why: the mock must capture the outbound request body + headers so adapter
    /// translation can be asserted.
    /// Test: itself.
    #[tokio::test]
    async fn mock_captures_request_body() {
        let server = MockInferenceServer::spawn(200, json!({"id": "x", "choices": []}))
            .await
            .expect("spawn");
        reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", server.url()))
            .header("Authorization", "Bearer sk-test") // pragma: allowlist secret
            .json(&json!({"model": "m", "messages": []}))
            .send()
            .await
            .expect("send");

        let req = server.last_request().expect("one request captured");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/chat/completions");
        assert_eq!(req.header("authorization"), Some("Bearer sk-test")); // pragma: allowlist secret
        assert_eq!(req.body.expect("json body")["model"], "m");
    }
}
