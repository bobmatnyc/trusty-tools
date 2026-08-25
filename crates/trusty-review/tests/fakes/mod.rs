//! An `AppState` that answers healthy without touching a network.
//!
//! Why: `uds_consumer_contract` is about what two consumers SEE, so the daemon
//! behind the socket has to answer deterministically and instantly. A real
//! `AppState` would call an inference provider and a trusty-search daemon, and
//! the test would then be measuring the developer's machine.
//!
//! Test: used by `uds_consumer_contract.rs`.

use std::sync::Arc;

use async_trait::async_trait;
use trusty_review::config::ReviewConfig;
use trusty_review::integrations::search_client::{
    EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
    SearchResult,
};
use trusty_review::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use trusty_review::service::AppState;

/// An LLM that always succeeds, so the inference probe reports `ok`.
struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: "ok".to_string(),
            model: req.model.clone(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A trusty-search that reports itself healthy, so the required dep is `Ok`.
struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

/// An `AppState` whose `review.health` answers `status: "ok"`.
pub fn healthy_state() -> AppState {
    AppState::new(
        ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FakeSearch),
        None,
    )
}
