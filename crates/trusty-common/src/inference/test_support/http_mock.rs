//! [`MockInferenceServer`] — an in-process axum HTTP mock for adapter tests.
//!
//! Why: the concrete HTTP adapters that land in #2403/#2407 need to be tested
//! against a real socket that returns canned chat/completions JSON — without a
//! live provider or credentials. This spins up a throwaway axum server on an
//! ephemeral loopback port that answers every request with a fixed status+body,
//! giving those future adapters a URL to point at. It is gated behind the
//! `axum-server` feature so the base `inference-client` feature pulls in NO
//! HTTP-server dependency (per the crate's axum-gating discipline).
//! What: [`MockInferenceServer::spawn`] binds `127.0.0.1:0`, serves a fixed
//! response from a fallback handler, and exposes [`MockInferenceServer::url`];
//! dropping it triggers graceful shutdown.
//! Test: inline `tests` — `mock_serves_canned_body` (feature-gated).

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::inference::error::InferenceError;

/// A running in-process HTTP mock returning one fixed response.
///
/// Why: the smallest real HTTP surface a future adapter can exercise end-to-end.
/// What: owns the server task and a shutdown channel; [`Self::url`] is the base
/// URL (`http://127.0.0.1:<port>`). Dropping the value signals graceful
/// shutdown so no test leaks a listener.
/// Test: `mock_serves_canned_body`.
pub struct MockInferenceServer {
    url: String,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl MockInferenceServer {
    /// Spawn a mock that answers every request with `status` + `body`.
    ///
    /// Why: adapter tests script one provider response and point the adapter at
    /// [`Self::url`].
    /// What: binds an ephemeral loopback port, serves a fallback handler that
    /// returns the canned `status`/`body` for any method/path, and returns the
    /// running handle. Errors as [`InferenceError::Provider`] if the bind fails.
    /// Test: `mock_serves_canned_body`.
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
        let app = Router::new()
            .fallback(canned_handler)
            .with_state((status, body));

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

/// Fallback handler returning the configured canned response.
///
/// Why: one handler answers every method/path so an adapter can target any
/// endpoint shape (e.g. `/v1/chat/completions`).
/// What: returns the `(status, body)` from router state as a JSON response.
/// Test: `mock_serves_canned_body`.
async fn canned_handler(State((status, body)): State<(StatusCode, Value)>) -> impl IntoResponse {
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
}
