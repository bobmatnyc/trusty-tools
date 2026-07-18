//! Configuration types, result types, and shared helpers for semantic
//! consolidation.
//!
//! Why: Isolates the data-model layer so `inference.rs` and `consolidator.rs`
//! can import from here without pulling in unrelated logic.
//! What: `SemanticConsolidationConfig`, `ConsolidationAction`,
//! `ConsolidationResult`, `CanonicalDrawer`, `inference_available`,
//! `parse_consolidation_actions`, `build_consolidation_prompt`,
//! `batch_cache_key`, `apply_actions_to_result`.
//! Test: `semantic_config_defaults`, `consolidation_action_deserializes`,
//! `parse_consolidation_actions_round_trips`, `batch_cache_key_is_deterministic`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::memory_core::palace::Drawer;

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
    let env_key = std::env::var(crate::env_vars::ENV_OPENROUTER_API_KEY).unwrap_or_default();
    !env_key.trim().is_empty()
}

// ─── Model / provider compatibility validation (issue #2593) ──────────────

/// Vendor prefixes used by OpenRouter's `vendor/model` naming convention
/// (e.g. `anthropic/claude-haiku-4-5`, `openai/gpt-4o-mini`).
///
/// Why: most of these prefixes are ambiguous on their own — Ollama's own
/// registry does host several vendor-namespaced pulls (`qwen/…`,
/// `mistralai/…`, `nvidia/…`, `meta-llama/…`, `deepseek/…`), so a bare prefix
/// match alone would false-positive on a legitimately-configured local model.
/// `validate_ollama_model` additionally requires the id to carry no explicit
/// `:tag` suffix before rejecting it — see that function's doc for the full
/// rule. This is a curated allowlist of the exact failure mode observed
/// (issue #2593: the OpenRouter-style default `anthropic/claude-haiku-4-5`
/// resolved against Ollama), not an exhaustive OpenRouter vendor catalog.
/// What: consulted only by `validate_ollama_model`.
/// Test: `validate_ollama_model_rejects_cloud_prefix`.
const CLOUD_VENDOR_PREFIXES: &[&str] = &[
    "anthropic/",
    "openai/",
    "google/",
    "x-ai/",
    "mistralai/",
    "meta-llama/",
    "cohere/",
    "perplexity/",
    "deepseek/",
    "qwen/",
    "nvidia/",
];

/// Reject a consolidation model id that cannot possibly resolve against a
/// local Ollama server.
///
/// Why (issue #2593): the default `semantic.model` is the OpenRouter-style id
/// `anthropic/claude-haiku-4-5`. When a deployment enables the local-model
/// path (`local_model_enabled = true`, no OpenRouter key) without overriding
/// `semantic.model`, every consolidation call 404s against Ollama and the
/// dream cycle retried the failure every cycle forever, producing recurring
/// log noise and redb write-lock churn on the retried batches. Catching the
/// mismatch once at consolidator-build time lets the caller fail loud and
/// disable the phase instead of retrying blindly.
/// What: returns `Err` (an actionable message naming the model, the resolved
/// provider, and the config knob to fix) only when `model` BOTH starts with a
/// known OpenRouter/cloud-vendor prefix AND carries no `:tag` suffix. Several
/// of those prefixes (`qwen/`, `mistralai/`, `nvidia/`, `meta-llama/`,
/// `deepseek/`) are also real, pullable Ollama namespaces, and a real local
/// pull is near-always referenced with an explicit tag
/// (`vendor/model:tag`) — so the presence of a `:tag` suffix is treated as
/// evidence the operator deliberately configured that vendor-namespaced
/// model locally, and bypasses the check. `Ok(())` in every other case. Does
/// NOT verify the model is actually pulled in Ollama (that requires a
/// network call this function deliberately avoids) — it only catches the
/// specific, unambiguous "this is not a local-model id" case.
/// Test: `validate_ollama_model_rejects_cloud_prefix`,
/// `validate_ollama_model_accepts_local_tag`,
/// `validate_ollama_model_accepts_tagged_vendor_namespaced_pull`.
pub fn validate_ollama_model(model: &str) -> Result<()> {
    let Some(prefix) = CLOUD_VENDOR_PREFIXES
        .iter()
        .find(|p| model.starts_with(**p))
    else {
        return Ok(());
    };
    if model.contains(':') {
        // Carries an explicit tag (e.g. "qwen/qwen2.5:7b-instruct") — treat
        // as a deliberately-configured local pull, not a copy-pasted
        // OpenRouter id.
        return Ok(());
    }
    anyhow::bail!(
        "semantic consolidation model '{model}' has the OpenRouter/cloud vendor prefix \
         '{prefix}' but resolves to the local Ollama backend (local_model_enabled=true, no \
         OpenRouter API key configured) — Ollama cannot serve this model id and every call \
         would fail. Fix: set `DreamConfig.semantic.model` to a locally-pulled Ollama tag \
         (e.g. \"llama3.1\"; if you really did pull \"{model}\" locally, add its explicit \
         `:tag` suffix — e.g. \"{model}:latest\" — to bypass this check), or configure an \
         OpenRouter API key (`DreamConfig.openrouter_api_key` / OPENROUTER_API_KEY env var) to \
         route this model through OpenRouter instead. Semantic consolidation is disabled until \
         the config is fixed."
    );
}

// ─── Internal prompt/response helpers ───────────────────────────────────────

/// Internal: build the user-turn content for a consolidation prompt.
///
/// Why: shared by both `OpenRouterInference` and `OllamaInference`.
/// What: formats each drawer as `ID: <id>\nContent: <content>\n---\n`.
/// Test: exercised indirectly via inference integration tests.
pub(super) fn build_consolidation_prompt(drawers: &[Drawer]) -> String {
    let mut lines = Vec::new();
    for d in drawers {
        lines.push(format!("ID: {}\nContent: {}\n", d.id, d.content));
    }
    lines.join("---\n")
}

/// Internal wire types for deserialising OpenAI-compatible chat responses.
#[derive(Deserialize)]
pub(super) struct OpenAiChatResponse {
    pub(super) choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
pub(super) struct OpenAiChoice {
    pub(super) message: OpenAiMessage,
}

#[derive(Deserialize)]
pub(super) struct OpenAiMessage {
    pub(super) content: Option<String>,
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

// ─── Cache and action helpers ────────────────────────────────────────────────

/// Compute a cache key for a batch of drawers: SHA-256 over sorted drawer
/// content strings (first 16 bytes, hex-encoded).
///
/// Why: same batch content → same key → one LLM call per unique batch.
/// What: sorts by drawer id for determinism, SHA-256s the joined content,
/// returns the first 32 hex chars.
/// Test: `batch_cache_key_is_deterministic`.
pub(super) fn batch_cache_key(batch: &[Drawer]) -> String {
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
pub(super) fn apply_actions_to_result(
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
