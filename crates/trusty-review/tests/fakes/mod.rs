//! Offline stand-ins for the services `run_review` calls.
//!
//! Why: the model-eval harness drives the real pipeline over recorded reviewer
//! output. A real provider or daemon would make the run depend on a network
//! and on the developer's machine.
//! What: [`FakeLlm`] answers every request with one recorded text and reports
//! token counts from the request and reply size; [`FakeVerifier`] answers
//! single and batched verifier requests, refuting the findings it names;
//! [`FakeSearch`] and [`ReadyAnalyze`] report healthy, empty context.
//! Test: used by `tests/model_eval.rs`.

use async_trait::async_trait;
use trusty_review::integrations::analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
};
use trusty_review::integrations::search_client::{
    EmbedderState, HealthResponse as SearchHealth, IndexInfo, IndexStatusResponse, SearchClient,
    SearchClientError, SearchResult,
};
use trusty_review::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use trusty_review::pipeline::verify_batch::{BATCH_FINDING_HEADING, VERIFY_BATCH_SCHEMA_NAME};

/// Rough token count of `text`: one token per four bytes, rounded up.
pub fn approx_tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

/// Token count of everything a request sends.
fn request_tokens(req: &LlmRequest) -> u32 {
    let body: usize = req.messages.iter().map(|m| m.content.len()).sum();
    approx_tokens(&req.system).saturating_add(u32::try_from(body.div_ceil(4)).unwrap_or(u32::MAX))
}

/// A reviewer that answers every request with `response`.
pub struct FakeLlm {
    /// The full reply text, prose plus fenced JSON.
    pub response: String,
}

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.response.clone(),
            model: req.model.clone(),
            input_tokens: request_tokens(&req),
            output_tokens: approx_tokens(&self.response),
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A verifier that confirms every finding except those whose kind (the
/// reviewer's `title`) is in `refuted`.
pub struct FakeVerifier {
    /// Finding titles to answer `REFUTED`.
    pub refuted: Vec<String>,
}

impl FakeVerifier {
    fn judge(&self, section: &str) -> &'static str {
        let named = |t: &String| section.contains(&format!("- kind: {t}\n"));
        if self.refuted.iter().any(named) {
            "REFUTED"
        } else {
            "CONFIRMED"
        }
    }

    /// The verifier's answer: one judgment, or one per `### Finding N` section
    /// of a batched request (#8904).
    fn answer(&self, req: &LlmRequest) -> String {
        let user = req.messages.first().map_or("", |m| m.content.as_str());
        let batched = req
            .response_schema
            .as_ref()
            .is_some_and(|s| s.name == VERIFY_BATCH_SCHEMA_NAME);
        if !batched {
            return format!(r#"{{"judgment":"{}","reason":"test"}}"#, self.judge(user));
        }
        let entries: Vec<String> = user
            .split(BATCH_FINDING_HEADING)
            .skip(1)
            .enumerate()
            .map(|(i, section)| {
                format!(
                    r#"{{"finding":{},"judgment":"{}","reason":"test"}}"#,
                    i + 1,
                    self.judge(section)
                )
            })
            .collect();
        format!(r#"{{"judgments":[{}]}}"#, entries.join(","))
    }
}

#[async_trait]
impl LlmProvider for FakeVerifier {
    fn name(&self) -> &str {
        "fake-verifier"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let text = self.answer(&req);
        Ok(LlmResponse {
            input_tokens: request_tokens(&req),
            output_tokens: approx_tokens(&text),
            text,
            model: req.model.clone(),
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A trusty-search that reports a ready `main` index and finds nothing, so the
/// context gate passes and the reviewer sees the diff alone.
pub struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        Ok(IndexStatusResponse::ready(index_id))
    }

    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![IndexInfo {
            id: "main".to_string(),
            name: None,
            root_path: None,
        }])
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

/// A trusty-analyze that reports ready with empty enrichment.
pub struct ReadyAnalyze;

#[async_trait]
impl AnalyzeClient for ReadyAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Ok(AnalyzeHealthResponse {
            status: "ok".to_string(),
            search_reachable: true,
        })
    }

    async fn has_analysis(&self, _: &str) -> bool {
        true
    }

    async fn complexity_hotspots(
        &self,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(vec![])
    }

    async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(vec![])
    }
}
