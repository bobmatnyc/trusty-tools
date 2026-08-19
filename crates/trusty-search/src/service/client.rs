use crate::core::indexer::{CodeChunk, SearchQuery};
use anyhow::Result;

/// HTTP client for the trusty-search daemon.
pub struct SearchClient {
    base_url: String,
    client: reqwest::Client,
}

impl SearchClient {
    /// Build a client for the trusty-search daemon at `base_url`.
    ///
    /// Why: the daemon answers on loopback, and reqwest routes `127.0.0.1`
    /// through an exported `HTTP_PROXY` — so without the shared proxy-free
    /// builder every call here fails on a machine with a proxy configured
    /// (#4392).
    /// What: `trusty_common::http_client::loopback_client_builder`, built. The
    /// `expect` mirrors `reqwest::Client::new`'s own contract — it panics on a
    /// build failure too, which only a broken TLS backend can cause.
    /// Test: proxy immunity is proven in `trusty_common::http_client::tests`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: trusty_common::http_client::loopback_client_builder()
                .build()
                .expect("reqwest client construction is infallible on supported platforms"),
        }
    }

    pub async fn health(&self) -> Result<bool> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    pub async fn search(&self, index_id: &str, query: SearchQuery) -> Result<Vec<CodeChunk>> {
        let resp = self
            .client
            .post(format!("{}/indexes/{}/search", self.base_url, index_id))
            .json(&query)
            .send()
            .await?;
        Ok(resp.json().await?)
    }
}
