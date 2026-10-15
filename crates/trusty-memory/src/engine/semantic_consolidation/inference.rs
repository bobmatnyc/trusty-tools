//! `Inference` trait and the production backend implementations.
//!
//! Why: Isolates the trait definition and its concrete implementations from
//! the types and consolidator so each file stays under the 500-SLOC cap.
//! What: `Inference` trait, `OpenRouterInference` (OpenRouter REST API),
//! and `OllamaInference` (local OpenAI-compatible server); the test double
//! `MockInference` lives in `mock.rs` (#9278).
//! Test: Unit coverage is via `MockInference`; live coverage requires
//! `--include-ignored` integration tests.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;

use crate::engine::palace::Drawer;

use super::types::{
    ConsolidationAction, OpenAiChatResponse, build_consolidation_prompt,
    parse_consolidation_actions,
};

pub(super) const OPENROUTER_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub(super) const CONSOLIDATION_PROMPT_SYSTEM: &str = r#"You are a knowledge consolidation assistant for a personal memory system. Given a batch of memory entries (drawers), identify:
1. Aliases: different names for the same concept (e.g. "ts" = "trusty-search")
2. Merge candidates: closely related facts that should be one canonical summary
3. Contradictions: entries that conflict (flag for human review; do NOT auto-resolve)

Return a JSON array of actions. Each action MUST have an "action" field.

Valid action types:
- {"action": "alias", "from": "<term>", "to": "<canonical_term>"}
- {"action": "merge", "canonical_content": "<single best summary>", "superseded_ids": ["<uuid>", ...]}
- {"action": "flag", "drawer_id": "<uuid>", "reason": "<why contradictory>"}

Rules:
- Be conservative: only merge if the entries express the SAME fact.
- Preserve nuance: if entries are related but distinct, do NOT merge.
- Return an empty array [] if no consolidation is warranted.
- The canonical_content for a merge MUST be a complete, standalone sentence or paragraph.
- Return ONLY the JSON array, no other text."#;

// ─── Inference trait ────────────────────────────────────────────────────────

/// Abstraction over an LLM backend for consolidation prompts.
///
/// Why: tests must use a deterministic `MockInference` without real network
/// calls; production code plugs in `OpenRouterInference` or `OllamaInference`.
/// What: one async method `consolidate` that takes a batch of drawers and
/// returns a (possibly empty) list of `ConsolidationAction`s.
/// Test: `MockInference` implements this trait for unit tests.
#[async_trait]
pub(crate) trait Inference: Send + Sync {
    /// Human-readable backend name (used in tracing spans).
    fn name(&self) -> &str;

    /// Send a batch of drawers to the model and return consolidation actions.
    ///
    /// Why: batching keeps prompt overhead low and lets the model see
    /// relationships between the entries.
    /// What: implementations format the drawers into a structured prompt,
    /// call the upstream model, parse the JSON action list from the response,
    /// and return the parsed actions (empty if none apply or parsing fails).
    /// Test: `MockInference::consolidate` returns pre-configured fixtures.
    async fn consolidate(&self, drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>>;
}

// ─── OpenRouter inference implementation ────────────────────────────────────

/// LLM backend backed by the OpenRouter API.
///
/// Why: zero-config cloud access for users who supply an `OPENROUTER_API_KEY`.
/// What: uses `reqwest` to POST a single-turn chat-completion (non-streaming)
/// with a structured consolidation prompt; parses the first response choice as
/// a JSON array of `ConsolidationAction`s.
/// Test: tested via `#[ignore]`'d live integration test; unit coverage is
/// through `MockInference`.
pub(crate) struct OpenRouterInference {
    api_key: String,
    model: String,
}

impl OpenRouterInference {
    /// Create a new OpenRouter inference backend.
    ///
    /// Why: centralises the field assignment so callers avoid poking internals.
    /// What: stores `api_key` and `model` verbatim.
    /// Test: `openrouter_inference_new_stores_fields`.
    pub(crate) fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl Inference for OpenRouterInference {
    fn name(&self) -> &str {
        "openrouter"
    }

    async fn consolidate(&self, drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>> {
        if self.api_key.is_empty() {
            return Err(anyhow!("OpenRouter API key is empty"));
        }
        if drawers.is_empty() {
            return Ok(vec![]);
        }

        let user_content = build_consolidation_prompt(drawers);
        let messages = vec![
            serde_json::json!({"role": "system", "content": CONSOLIDATION_PROMPT_SYSTEM}),
            serde_json::json!({"role": "user", "content": user_content}),
        ];

        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
        });

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("build reqwest client for OpenRouterInference")?;

        let resp = client
            .post(OPENROUTER_COMPLETIONS_URL)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/bobmatnyc/trusty-tools")
            .header("X-Title", "trusty-memory")
            .json(&body)
            .send()
            .await
            .context("POST OpenRouter consolidation")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("OpenRouter HTTP {status}: {text}"));
        }

        let payload: OpenAiChatResponse = resp
            .json()
            .await
            .context("parse OpenRouter consolidation response")?;

        let raw_content = payload
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        parse_consolidation_actions(&raw_content)
    }
}

// ─── Ollama inference implementation ────────────────────────────────────────

/// LLM backend backed by a local Ollama (or any OpenAI-compatible) server.
///
/// Why: local model = zero cost + privacy. Preferred over OpenRouter when both
/// are available.
/// What: same single-turn non-streaming POST as `OpenRouterInference` but
/// targeting the local server's `/v1/chat/completions`.
/// Test: tested via `#[ignore]`'d live integration test; unit coverage via
/// `MockInference`.
pub(crate) struct OllamaInference {
    base_url: String,
    model: String,
}

impl OllamaInference {
    /// Create a new Ollama inference backend.
    ///
    /// Why: mirrors `OpenRouterInference::new` for uniform construction.
    /// What: stores `base_url` (no trailing slash) and `model` verbatim.
    /// Test: `ollama_inference_new_stores_fields`.
    pub(crate) fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl Inference for OllamaInference {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn consolidate(&self, drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>> {
        if drawers.is_empty() {
            return Ok(vec![]);
        }

        let user_content = build_consolidation_prompt(drawers);
        let messages = vec![
            serde_json::json!({"role": "system", "content": CONSOLIDATION_PROMPT_SYSTEM}),
            serde_json::json!({"role": "user", "content": user_content}),
        ];
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
        });

        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("build reqwest client for OllamaInference")?;

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama HTTP {status}: {text}"));
        }

        let payload: OpenAiChatResponse = resp
            .json()
            .await
            .context("parse Ollama consolidation response")?;

        let raw_content = payload
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        parse_consolidation_actions(&raw_content)
    }
}
