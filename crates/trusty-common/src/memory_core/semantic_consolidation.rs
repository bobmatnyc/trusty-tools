//! Inference-backed semantic consolidation for the dream cycle.
//!
//! Why: The NLP-only dream passes (dedup, prune, closet) cannot handle
//! semantic equivalence: `"ts"` and `"trusty-search"` are the same concept,
//! three overlapping facts about a single topic should collapse to one crisp
//! triple, and paraphrases waste recall slots. An LLM can see the semantic
//! layer the vector index cannot.
//! What: Defines the `Inference` trait (abstraction over any chat model) with
//! `OpenRouterInference` (production) and `MockInference` (tests), plus the
//! `SemanticConsolidator` that clusters near-duplicate drawers and delegates
//! canonicalization to the configured provider. Gracefully degrades to a no-op
//! when no inference backend is available.
//! Test: `cargo test -p trusty-common --features memory-core
//!        semantic_consolidation::tests::`.

use crate::memory_core::palace::Drawer;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

// ─── Public config ──────────────────────────────────────────────────────────

/// Configuration for the semantic-consolidation dream phase.
///
/// Why: lets operators tune the cost/quality trade-off without recompiling;
/// sane defaults prevent surprise LLM spend on small or unimportant palaces.
/// What: holds the enable flag, model id, similarity threshold for clustering,
/// and per-cycle call budget.
/// Test: `semantic_config_defaults` asserts the default field values.
#[derive(Debug, Clone)]
pub struct SemanticConsolidationConfig {
    /// Whether the phase runs at all (default `true`; no-op if no inference
    /// backend is available regardless of this flag).
    pub enabled: bool,
    /// OpenRouter / Ollama model id used for consolidation prompts.
    pub model: String,
    /// Cosine-similarity threshold above which two drawers are considered
    /// candidates for LLM-based consolidation (distinct from the NLP dedup
    /// threshold; lower here to catch near-synonyms the embedding model can
    /// see but the strict dedup pass ignores).
    pub similarity_threshold: f32,
    /// Maximum drawers in a single LLM prompt batch (keeps prompts short).
    pub max_batch_size: usize,
    /// Maximum total LLM calls per dream cycle (cost ceiling).
    pub max_calls_per_cycle: usize,
}

impl Default for SemanticConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "anthropic/claude-haiku-4-5".to_string(),
            similarity_threshold: 0.75,
            max_batch_size: 8,
            max_calls_per_cycle: 20,
        }
    }
}

// ─── Consolidation actions (wire + domain) ─────────────────────────────────

/// A single consolidation action emitted by the LLM.
///
/// Why: the LLM must return structured JSON so we can parse and apply actions
/// without further prompting; keeping the enum flat (alias / merge / flag)
/// covers the three cases from the issue spec.
/// What: `Alias` links two names for the same concept; `Merge` produces a
/// canonical drawer from a cluster; `Flag` marks a contradiction for human
/// review.
/// Test: `consolidation_action_deserializes` round-trips each variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ConsolidationAction {
    /// Two strings that refer to the same concept (e.g. `"ts"` → `"trusty-search"`).
    Alias { from: String, to: String },
    /// A cluster of drawers that should share one canonical summary.
    /// `canonical_content` is the LLM's output; `superseded_ids` are the
    /// drawer UUIDs that were consolidated.
    Merge {
        canonical_content: String,
        superseded_ids: Vec<Uuid>,
    },
    /// A drawer that contradicts another; flagged for human review.
    Flag { drawer_id: Uuid, reason: String },
}

/// Outcome of one consolidation run: new canonical drawers to add, alias
/// triples to store, and drawer ids flagged as superseded or contradictory.
///
/// Why: keeps the consolidator side-effect-free — callers (dream.rs) apply
/// the returned actions rather than having the consolidator mutate the palace
/// directly.
/// What: additive-only lists; original drawers are preserved, superseded ones
/// get a `superseded_by` link in the KG.
/// Test: tested via `SemanticConsolidator` unit tests.
#[derive(Debug, Default)]
pub struct ConsolidationResult {
    /// New canonical drawers to add to the palace.
    pub canonical_drawers: Vec<CanonicalDrawer>,
    /// Alias pairs discovered: `(from_term, to_canonical_term)`.
    pub aliases: Vec<(String, String)>,
    /// Drawer ids that are now superseded by a canonical drawer.
    pub superseded_ids: Vec<Uuid>,
    /// Drawer ids flagged for human review (contradiction / uncertainty).
    pub flagged_ids: Vec<(Uuid, String)>,
    /// Number of LLM calls consumed.
    pub llm_calls: usize,
    /// Response cache hits (saved LLM calls).
    pub cache_hits: usize,
}

/// A new canonical drawer produced by the LLM from a cluster.
///
/// Why: we need to store both the canonical text and the list of superseded
/// drawer ids so dream.rs can write the canonical drawer and set the
/// `superseded_by` link on the originals.
/// What: plain struct; `canonical_for` is sorted for deterministic hashing.
/// Test: checked through `SemanticConsolidator`.
#[derive(Debug, Clone)]
pub struct CanonicalDrawer {
    pub content: String,
    pub importance: f32,
    pub tags: Vec<String>,
    /// Ids of the drawers this canonical entry replaces.
    pub canonical_for: Vec<Uuid>,
}

// ─── Inference trait ────────────────────────────────────────────────────────

/// Abstraction over an LLM backend for consolidation prompts.
///
/// Why: tests must use a deterministic `MockInference` without real network
/// calls; production code plugs in `OpenRouterInference` or `OllamaInference`.
/// What: one async method `consolidate` that takes a batch of drawers and
/// returns a (possibly empty) list of `ConsolidationAction`s.
/// Test: `MockInference` implements this trait for unit tests.
#[async_trait]
pub trait Inference: Send + Sync {
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

// ─── Gate: is inference available? ─────────────────────────────────────────

/// Check whether at least one inference backend is configured and potentially
/// reachable — WITHOUT making a network call.
///
/// Why: the dream cycle must not stall on a cold-start or missing config; a
/// cheap synchronous gate (env var check only) decides whether to attempt the
/// semantic phase at all.
/// What: returns `true` if `OPENROUTER_API_KEY` is non-empty in the
/// environment, or if `openrouter_api_key` is non-empty; `ollama_base_url` is
/// non-empty when the local model is enabled. Does NOT ping any endpoint.
/// Test: `inference_available_false_without_key` (no env var → false) and
/// `inference_available_true_with_key` (env var set → true).
pub fn inference_available(openrouter_api_key: &str, local_model_enabled: bool) -> bool {
    if !openrouter_api_key.trim().is_empty() {
        return true;
    }
    if local_model_enabled {
        return true;
    }
    // Fall back to env var (daemon callers that didn't thread the config
    // through to this level yet).
    let env_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    !env_key.trim().is_empty()
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
pub struct OpenRouterInference {
    api_key: String,
    model: String,
}

impl OpenRouterInference {
    /// Create a new OpenRouter inference backend.
    ///
    /// Why: centralises the field assignment so callers avoid poking internals.
    /// What: stores `api_key` and `model` verbatim.
    /// Test: `openrouter_inference_new_stores_fields`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

const OPENROUTER_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const CONSOLIDATION_PROMPT_SYSTEM: &str = r#"You are a knowledge consolidation assistant for a personal memory system. Given a batch of memory entries (drawers), identify:
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

fn build_consolidation_prompt(drawers: &[Drawer]) -> String {
    let mut lines = Vec::new();
    for d in drawers {
        lines.push(format!("ID: {}\nContent: {}\n", d.id, d.content));
    }
    lines.join("---\n")
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
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
pub struct OllamaInference {
    base_url: String,
    model: String,
}

impl OllamaInference {
    /// Create a new Ollama inference backend.
    ///
    /// Why: mirrors `OpenRouterInference::new` for uniform construction.
    /// What: stores `base_url` (no trailing slash) and `model` verbatim.
    /// Test: `ollama_inference_new_stores_fields`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
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

// ─── Mock inference (for tests) ─────────────────────────────────────────────

/// Deterministic inference backend for unit and integration tests.
///
/// Why: real LLM calls must never run during `cargo test` (they hit the
/// network, cost money, and are non-deterministic). `MockInference` returns
/// pre-configured fixture actions so tests are fast and reproducible.
/// What: constructed with a `Vec<ConsolidationAction>` that is cloned on
/// every `consolidate` call, plus a call counter so tests can assert the
/// right number of batches fired.
/// Test: Used directly in `semantic_consolidation::tests` and
/// `dream::tests::dream_cycle_semantic_consolidation_*`.
pub struct MockInference {
    /// Actions returned for every batch (regardless of batch content).
    pub fixture_actions: Vec<ConsolidationAction>,
    /// Counts how many `consolidate` calls have been made.
    pub call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl MockInference {
    /// Create a mock that always returns `fixture_actions`.
    ///
    /// Why: test setup: callers supply the desired output so assertions are
    /// predictable.
    /// What: stores the fixture list and initialises the call counter to 0.
    /// Test: exercised by every test in `semantic_consolidation::tests`.
    pub fn new(fixture_actions: Vec<ConsolidationAction>) -> Self {
        Self {
            fixture_actions,
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Create a mock that returns no actions (no-op consolidation).
    ///
    /// Why: integration tests that verify the dream cycle completes when
    /// consolidation finds nothing to do.
    /// What: constructs with an empty fixture list.
    /// Test: `dream_cycle_semantic_consolidation_no_inference`.
    pub fn no_op() -> Self {
        Self::new(vec![])
    }
}

#[async_trait]
impl Inference for MockInference {
    fn name(&self) -> &str {
        "mock"
    }

    async fn consolidate(&self, _drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.fixture_actions.clone())
    }
}

// ─── LLM response parsing ────────────────────────────────────────────────────

/// Parse the raw LLM text response into a list of `ConsolidationAction`s.
///
/// Why: models don't always return clean JSON (markdown fences, leading prose,
/// trailing commas). This parser tries to extract the first JSON array from
/// the response text.
/// What: strips markdown code fences, finds the first `[…]` substring, and
/// attempts `serde_json::from_str`. Returns an empty list on any parse error
/// so the dream cycle degrades gracefully instead of failing.
/// Test: `parse_consolidation_actions_round_trips`, `parse_handles_markdown_fence`,
/// `parse_returns_empty_on_garbage`.
pub fn parse_consolidation_actions(raw: &str) -> Result<Vec<ConsolidationAction>> {
    // Strip markdown code fences if present.
    let stripped = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find the first `[` and last `]` to extract the JSON array.
    let start = stripped.find('[').unwrap_or(0);
    let end = stripped.rfind(']').map(|i| i + 1).unwrap_or(stripped.len());
    if start >= end {
        return Ok(vec![]);
    }
    let json_slice = &stripped[start..end];

    match serde_json::from_str::<Vec<ConsolidationAction>>(json_slice) {
        Ok(actions) => Ok(actions),
        Err(e) => {
            tracing::debug!(
                raw = json_slice,
                error = %e,
                "failed to parse consolidation actions; treating as empty"
            );
            Ok(vec![])
        }
    }
}

// ─── SemanticConsolidator ───────────────────────────────────────────────────

/// Content-hash-keyed cache for consolidation results.
///
/// Why: the dream cycle may run frequently; re-consolidating the same batch
/// costs money and time. Keying by a hash of sorted drawer content lets us
/// skip batches we already processed.
/// What: `HashMap<String, Vec<ConsolidationAction>>` where the key is the
/// SHA-256 of the batch's sorted content strings (hex-encoded first 16 bytes
/// for compactness).
/// Test: cache hits verified via `MockInference::call_count`.
type ConsolidationCache = HashMap<String, Vec<ConsolidationAction>>;

/// Clusters semantically similar drawers and canonicalizes them via LLM.
///
/// Why: wraps the `Inference` trait + similarity threshold + call budget
/// behind one struct so `dream.rs` has a single callsite.
/// What: given a slice of drawers (already filtered to candidates via the
/// embedding-similarity threshold), batches them and calls `Inference::consolidate`;
/// applies the response cache to skip already-seen batches; returns a
/// `ConsolidationResult` for the caller to apply.
/// Test: `consolidator_merges_cluster`, `consolidator_caches_repeated_batches`,
/// `consolidator_respects_call_budget`.
pub struct SemanticConsolidator {
    inference: Arc<dyn Inference>,
    pub config: SemanticConsolidationConfig,
    cache: parking_lot::Mutex<ConsolidationCache>,
}

impl SemanticConsolidator {
    /// Create a new consolidator.
    ///
    /// Why: constructor that bundles the inference backend with the config.
    /// What: stores both; cache starts empty.
    /// Test: exercised by all `SemanticConsolidator` tests.
    pub fn new(inference: Arc<dyn Inference>, config: SemanticConsolidationConfig) -> Self {
        Self {
            inference,
            config,
            cache: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Run consolidation over a list of candidate drawers.
    ///
    /// Why: top-level entry point for the dream cycle; hides batching,
    /// caching, and budget enforcement.
    /// What: splits `drawers` into batches of `config.max_batch_size`, checks
    /// the response cache for each batch, calls `self.inference.consolidate` on
    /// cache misses, and stops once `config.max_calls_per_cycle` LLM calls have
    /// been made. Returns a `ConsolidationResult` accumulating all actions.
    /// Test: `consolidator_merges_cluster`, `consolidator_respects_call_budget`.
    pub async fn consolidate(&self, drawers: &[Drawer]) -> ConsolidationResult {
        let mut result = ConsolidationResult::default();
        let mut calls_made = 0usize;

        for batch in drawers.chunks(self.config.max_batch_size) {
            if calls_made >= self.config.max_calls_per_cycle {
                tracing::debug!(
                    budget = self.config.max_calls_per_cycle,
                    "semantic consolidation call budget exhausted"
                );
                break;
            }

            let cache_key = batch_cache_key(batch);

            // Check cache first.
            let cached = {
                let guard = self.cache.lock();
                guard.get(&cache_key).cloned()
            };

            let actions = if let Some(actions) = cached {
                result.cache_hits += 1;
                tracing::debug!(key = %cache_key, "semantic consolidation cache hit");
                actions
            } else {
                match self.inference.consolidate(batch).await {
                    Ok(actions) => {
                        calls_made += 1;
                        result.llm_calls += 1;
                        // Store in cache.
                        self.cache.lock().insert(cache_key, actions.clone());
                        actions
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "semantic consolidation LLM call failed; skipping batch"
                        );
                        calls_made += 1; // count as consumed to avoid infinite retry
                        vec![]
                    }
                }
            };

            apply_actions_to_result(actions, batch, &mut result);
        }

        result
    }
}

/// Compute a cache key for a batch of drawers: SHA-256 over sorted drawer
/// content strings (first 16 bytes, hex-encoded).
///
/// Why: same batch content → same key → one LLM call per unique batch.
/// What: sorts by drawer id for determinism, SHA-256s the joined content,
/// returns the first 32 hex chars.
/// Test: `batch_cache_key_is_deterministic`.
fn batch_cache_key(batch: &[Drawer]) -> String {
    use sha2::Digest;
    use std::collections::BTreeMap;
    let sorted: BTreeMap<Uuid, &str> = batch.iter().map(|d| (d.id, d.content.as_str())).collect();
    let mut hasher = sha2::Sha256::new();
    for (id, content) in &sorted {
        hasher.update(id.as_bytes());
        hasher.update(content.as_bytes());
    }
    let hash = hasher.finalize();
    ::hex::encode(&hash[..16])
}

/// Apply a list of `ConsolidationAction`s to the accumulator.
///
/// Why: separates parsing from state mutation so actions from cache hits and
/// live calls go through the same path.
/// What: for `Alias` → push to `result.aliases`; for `Merge` → build a
/// `CanonicalDrawer` and add superseded ids; for `Flag` → add to flagged_ids.
/// Test: exercised via `consolidator_merges_cluster`.
fn apply_actions_to_result(
    actions: Vec<ConsolidationAction>,
    batch: &[Drawer],
    result: &mut ConsolidationResult,
) {
    for action in actions {
        match action {
            ConsolidationAction::Alias { from, to } => {
                result.aliases.push((from, to));
            }
            ConsolidationAction::Merge {
                canonical_content,
                superseded_ids,
            } => {
                // Compute importance as the max among superseded drawers.
                let max_importance = batch
                    .iter()
                    .filter(|d| superseded_ids.contains(&d.id))
                    .map(|d| d.importance)
                    .fold(0.5f32, f32::max);

                // Union tags from superseded drawers.
                let mut tags: Vec<String> = Vec::new();
                for d in batch.iter().filter(|d| superseded_ids.contains(&d.id)) {
                    for t in &d.tags {
                        if !tags.contains(t) {
                            tags.push(t.clone());
                        }
                    }
                }

                result.canonical_drawers.push(CanonicalDrawer {
                    content: canonical_content,
                    importance: max_importance,
                    tags,
                    canonical_for: superseded_ids.clone(),
                });
                for id in superseded_ids {
                    if !result.superseded_ids.contains(&id) {
                        result.superseded_ids.push(id);
                    }
                }
            }
            ConsolidationAction::Flag { drawer_id, reason } => {
                result.flagged_ids.push((drawer_id, reason));
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::palace::Drawer;
    use uuid::Uuid;

    fn make_drawer(content: &str, importance: f32) -> Drawer {
        let room_id = Uuid::new_v4();
        let mut d = Drawer::new(room_id, content);
        d.importance = importance;
        d
    }

    /// Why: lock the default config values so accidental changes are caught.
    #[test]
    fn semantic_config_defaults() {
        let cfg = SemanticConsolidationConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.model, "anthropic/claude-haiku-4-5");
        assert!((cfg.similarity_threshold - 0.75).abs() < 1e-6);
        assert_eq!(cfg.max_batch_size, 8);
        assert_eq!(cfg.max_calls_per_cycle, 20);
    }

    /// Why: the JSON serialization of each action variant must round-trip
    /// cleanly through serde so LLM responses can be parsed unambiguously.
    #[test]
    fn consolidation_action_deserializes() {
        let alias_json = r#"{"action":"alias","from":"ts","to":"trusty-search"}"#;
        let action: ConsolidationAction = serde_json::from_str(alias_json).unwrap();
        assert_eq!(
            action,
            ConsolidationAction::Alias {
                from: "ts".into(),
                to: "trusty-search".into()
            }
        );

        let id = Uuid::new_v4();
        let merge_json = format!(
            r#"{{"action":"merge","canonical_content":"trusty-search is a hybrid search daemon","superseded_ids":["{id}"]}}"#
        );
        let action: ConsolidationAction = serde_json::from_str(&merge_json).unwrap();
        if let ConsolidationAction::Merge {
            canonical_content,
            superseded_ids,
        } = action
        {
            assert_eq!(canonical_content, "trusty-search is a hybrid search daemon");
            assert_eq!(superseded_ids, vec![id]);
        } else {
            panic!("expected Merge");
        }

        let flag_json =
            format!(r#"{{"action":"flag","drawer_id":"{id}","reason":"contradicts other entry"}}"#);
        let action: ConsolidationAction = serde_json::from_str(&flag_json).unwrap();
        assert_eq!(
            action,
            ConsolidationAction::Flag {
                drawer_id: id,
                reason: "contradicts other entry".into()
            }
        );
    }

    /// Why: the gate function must return false when no key is configured so
    /// the dream cycle skips LLM calls on unconfigured deployments.
    #[test]
    fn inference_available_false_without_key() {
        // Inline empty key, local_model disabled, no env var dependency.
        assert!(!inference_available("", false));
        assert!(!inference_available("   ", false));
    }

    /// Why: an inline key (not env var) must also enable inference.
    #[test]
    fn inference_available_true_with_inline_key() {
        assert!(inference_available("sk-test-key", false));
    }

    /// Why: local model flag alone is sufficient to enable inference.
    #[test]
    fn inference_available_true_with_local_model() {
        assert!(inference_available("", true));
    }

    /// Why: `parse_consolidation_actions` must extract actions from raw JSON
    /// returned by the LLM.
    #[test]
    fn parse_consolidation_actions_round_trips() {
        let id = Uuid::new_v4();
        let raw = format!(
            r#"[{{"action":"alias","from":"ts","to":"trusty-search"}},{{"action":"flag","drawer_id":"{id}","reason":"test"}}]"#
        );
        let actions = parse_consolidation_actions(&raw).unwrap();
        assert_eq!(actions.len(), 2);
    }

    /// Why: many models wrap their JSON in markdown fences; the parser must
    /// strip them.
    #[test]
    fn parse_handles_markdown_fence() {
        let raw = "```json\n[{\"action\":\"alias\",\"from\":\"a\",\"to\":\"b\"}]\n```";
        let actions = parse_consolidation_actions(raw).unwrap();
        assert_eq!(actions.len(), 1);
    }

    /// Why: if the model returns garbage, the dream cycle must not fail.
    #[test]
    fn parse_returns_empty_on_garbage() {
        let actions = parse_consolidation_actions("sorry, I cannot help with that").unwrap();
        assert!(actions.is_empty());
    }

    /// Why: same batch must produce the same cache key so cache hits work.
    #[test]
    fn batch_cache_key_is_deterministic() {
        let d1 = make_drawer("alpha content", 0.7);
        let d2 = make_drawer("beta content", 0.5);
        let batch = vec![d1.clone(), d2.clone()];
        let k1 = batch_cache_key(&batch);
        let k2 = batch_cache_key(&batch);
        assert_eq!(k1, k2);
    }

    /// Why: two different batches must have different keys so the cache
    /// doesn't return stale actions.
    #[test]
    fn batch_cache_key_differs_for_different_content() {
        let d1 = make_drawer("alpha content", 0.7);
        let d2 = make_drawer("totally different", 0.5);
        let k1 = batch_cache_key(&[d1]);
        let k2 = batch_cache_key(&[d2]);
        assert_ne!(k1, k2);
    }

    /// Why: `SemanticConsolidator` must produce a `CanonicalDrawer` and
    /// populate `superseded_ids` when the mock returns a `Merge` action.
    #[tokio::test]
    async fn consolidator_merges_cluster() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let mut d1 = make_drawer("ts is a search tool", 0.8);
        d1.id = id1;
        let mut d2 = make_drawer("trusty-search is a hybrid search daemon", 0.6);
        d2.id = id2;

        let actions = vec![ConsolidationAction::Merge {
            canonical_content: "trusty-search (ts) is a hybrid BM25+vector search daemon"
                .to_string(),
            superseded_ids: vec![id1, id2],
        }];

        let mock = Arc::new(MockInference::new(actions));
        let call_count = mock.call_count.clone();
        let cfg = SemanticConsolidationConfig {
            max_batch_size: 8,
            max_calls_per_cycle: 20,
            ..Default::default()
        };
        let consolidator = SemanticConsolidator::new(mock, cfg);

        let result = consolidator.consolidate(&[d1, d2]).await;

        assert_eq!(result.canonical_drawers.len(), 1);
        assert_eq!(
            result.canonical_drawers[0].content,
            "trusty-search (ts) is a hybrid BM25+vector search daemon"
        );
        assert!(result.superseded_ids.contains(&id1));
        assert!(result.superseded_ids.contains(&id2));
        assert_eq!(call_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// Why: once a batch has been processed, subsequent identical calls must
    /// return cached actions without hitting the inference backend.
    #[tokio::test]
    async fn consolidator_caches_repeated_batches() {
        let d = make_drawer("trusty-memory is a palace storage engine", 0.7);
        let actions = vec![ConsolidationAction::Alias {
            from: "tm".to_string(),
            to: "trusty-memory".to_string(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let call_count = mock.call_count.clone();
        let consolidator = SemanticConsolidator::new(
            mock,
            SemanticConsolidationConfig {
                max_batch_size: 8,
                max_calls_per_cycle: 20,
                ..Default::default()
            },
        );

        let r1 = consolidator.consolidate(std::slice::from_ref(&d)).await;
        let r2 = consolidator.consolidate(std::slice::from_ref(&d)).await;

        // Second run should hit the cache.
        assert_eq!(call_count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(r1.cache_hits, 0);
        assert_eq!(r2.cache_hits, 1);
        assert_eq!(r1.aliases.len(), 1);
        assert_eq!(r2.aliases.len(), 1);
    }

    /// Why: the call budget must be honored so a palace with many drawers
    /// doesn't fire unlimited LLM calls in one cycle.
    #[tokio::test]
    async fn consolidator_respects_call_budget() {
        // 10 drawers with batch_size=2 would normally require 5 calls.
        let drawers: Vec<Drawer> = (0..10)
            .map(|i| make_drawer(&format!("drawer content {i}"), 0.5))
            .collect();

        let mock = Arc::new(MockInference::no_op());
        let call_count = mock.call_count.clone();
        let consolidator = SemanticConsolidator::new(
            mock,
            SemanticConsolidationConfig {
                max_batch_size: 2,
                max_calls_per_cycle: 3, // cap at 3
                ..Default::default()
            },
        );

        let result = consolidator.consolidate(&drawers).await;

        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "should stop at budget of 3 calls"
        );
        assert_eq!(result.llm_calls, 3);
    }

    /// Why: aliases must be collected from the mock and surfaced in the result.
    #[tokio::test]
    async fn consolidator_collects_aliases() {
        let d = make_drawer("ts stands for trusty-search", 0.5);
        let actions = vec![ConsolidationAction::Alias {
            from: "ts".into(),
            to: "trusty-search".into(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let consolidator = SemanticConsolidator::new(mock, SemanticConsolidationConfig::default());

        let result = consolidator.consolidate(&[d]).await;

        assert_eq!(
            result.aliases,
            vec![("ts".to_string(), "trusty-search".to_string())]
        );
    }

    /// Why: flagged drawers must be surfaced without being deleted.
    #[tokio::test]
    async fn consolidator_flags_contradictions() {
        let d = make_drawer("trusty-search uses PostgreSQL for storage", 0.7);
        let id = d.id;
        let actions = vec![ConsolidationAction::Flag {
            drawer_id: id,
            reason: "contradicts: trusty-search uses redb".into(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let consolidator = SemanticConsolidator::new(mock, SemanticConsolidationConfig::default());

        let result = consolidator.consolidate(&[d]).await;

        assert_eq!(result.flagged_ids.len(), 1);
        assert_eq!(result.flagged_ids[0].0, id);
        assert!(result.superseded_ids.is_empty());
        assert!(result.canonical_drawers.is_empty());
    }
}
