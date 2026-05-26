//! HTTP client for the remote `trusty-embedderd` process.
//!
//! Why: when the operator sets `TRUSTY_EMBEDDER=http://...` the embedder runs
//! as an independent process; trusty-search sends HTTP POST requests to it
//! instead of running ONNX in-process. This decouples the two processes so a
//! crash of one doesn't affect the other (issue #110 motivation).
//!
//! What: `RemoteEmbedderClient` holds a `reqwest::Client` and a base URL. The
//! `EmbedderClient::embed_batch` method sends a `POST /embed` JSON request and
//! deserialises the `EmbedResponse`. Errors are mapped to `EmbedderError`
//! variants with enough context for the caller to log and retry if desired.
//!
//! Test: `remote_embed_url_construction` tests the URL assembly.
//! End-to-end: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`.

use async_trait::async_trait;

use crate::{EmbedRequest, EmbedResponse, EmbedderClient, EmbedderError};

/// HTTP embedder client that delegates to a running `trusty-embedderd` instance.
///
/// Why: provides the remote half of the `EmbedderClient` trait so trusty-search
/// can switch to out-of-process embedding without any other code changes.
///
/// What: sends `POST <base_url>/embed` with a JSON `EmbedRequest` body and
/// returns the deserialized `EmbedResponse::vectors`. Uses a shared
/// `reqwest::Client` with connection pooling so repeated calls don't pay TCP
/// handshake cost.
///
/// Test: `remote_client_construction` below; HTTP round-trip in
/// `trusty-embedderd/tests/bit_identical.rs` (`#[ignore]`).
#[derive(Clone, Debug)]
pub struct RemoteEmbedderClient {
    client: reqwest::Client,
    embed_url: String,
}

impl RemoteEmbedderClient {
    /// Construct a new remote client pointing at `base_url`.
    ///
    /// Why: callers pass the base URL (e.g. `http://127.0.0.1:7890`) and the
    /// client appends `/embed` for the POST endpoint.
    ///
    /// What: builds a `reqwest::Client` with default TLS settings, stores the
    /// fully-qualified embed URL.
    ///
    /// Test: `remote_client_construction` below verifies the URL is stored
    /// correctly without sending any network requests.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let base = base.trim_end_matches('/').to_owned();
        let embed_url = format!("{base}/embed");
        let client = reqwest::Client::builder()
            .build()
            .expect("reqwest client construction is infallible on supported platforms");
        tracing::debug!(embed_url = %embed_url, "RemoteEmbedderClient constructed");
        Self { client, embed_url }
    }

    /// Base URL passed to `new`, without trailing slash.
    ///
    /// Why: callers (health checks, logging) sometimes need the base URL.
    ///
    /// What: extracts the base from the stored embed URL.
    ///
    /// Test: verified via `remote_client_construction`.
    pub fn base_url(&self) -> &str {
        self.embed_url
            .strip_suffix("/embed")
            .unwrap_or(&self.embed_url)
    }
}

#[async_trait]
impl EmbedderClient for RemoteEmbedderClient {
    /// Send a POST /embed request to the remote embedderd.
    ///
    /// Why: delegates all ONNX work to the standalone process so the caller's
    /// RSS is not inflated by the model and its ORT arena.
    ///
    /// What: serialises `texts` into an `EmbedRequest` JSON body, POSTs to
    /// `<base_url>/embed`, checks for HTTP errors, and deserialises the
    /// `EmbedResponse`. Validates that the response vector count matches the
    /// input text count.
    ///
    /// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`.
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let sent = texts.len();
        let req = EmbedRequest { texts };

        tracing::debug!(url = %self.embed_url, n = sent, "RemoteEmbedderClient: sending batch");

        let response = self.client.post(&self.embed_url).json(&req).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(unreadable body)".to_owned());
            return Err(EmbedderError::RemoteError {
                status: status.as_u16(),
                body,
            });
        }

        let resp: EmbedResponse = response.json().await?;

        if resp.vectors.len() != sent {
            return Err(EmbedderError::DimensionMismatch {
                sent,
                got: resp.vectors.len(),
            });
        }

        tracing::debug!(url = %self.embed_url, n = sent, "RemoteEmbedderClient: batch complete");
        Ok(resp.vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_client_construction() {
        // Why: guard against URL assembly bugs (double-slash, missing /embed).
        // What: construct a client and assert the embed URL ends with /embed.
        // Test: this test.
        let c = RemoteEmbedderClient::new("http://127.0.0.1:7890");
        assert_eq!(c.embed_url, "http://127.0.0.1:7890/embed");
        assert_eq!(c.base_url(), "http://127.0.0.1:7890");
    }

    #[test]
    fn remote_client_strips_trailing_slash() {
        // Why: callers might pass a trailing slash; the URL must still be correct.
        let c = RemoteEmbedderClient::new("http://127.0.0.1:7890/");
        assert_eq!(c.embed_url, "http://127.0.0.1:7890/embed");
    }

    #[tokio::test]
    async fn empty_batch_short_circuits() {
        // Why: empty batches should not incur network round-trips.
        // What: call embed_batch with empty vec; no network call means
        // we can test this without a running embedderd.
        let c = RemoteEmbedderClient::new("http://127.0.0.1:1"); // unreachable port
        let result = c
            .embed_batch(vec![])
            .await
            .expect("empty batch short-circuits");
        assert!(result.is_empty());
    }
}
