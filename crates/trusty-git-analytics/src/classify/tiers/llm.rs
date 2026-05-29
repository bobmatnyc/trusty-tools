//! Tier 4: optional LLM fallback.
//!
//! Sends an OpenAI-compatible chat completion request asking the model to
//! emit a JSON object with `category`, `subcategory`, and `confidence`. The
//! LLM is consulted only when tiers 1–3 all failed and the engine has been
//! configured with `use_llm = true`.
//!
//! All failures are **non-fatal**: a network error, parse error, or missing
//! API key results in `None` so the pipeline can fall back to
//! "uncategorized" rather than crashing.

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::classify::tiers::bedrock::BedrockClassifier;
use crate::classify::tiers::ClassificationResult;
use crate::core::config::{LlmConfig, LlmSource};
use crate::core::models::ClassificationMethod;

/// OpenAI-compatible chat completion endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// OpenRouter chat completion endpoint (OpenAI-compatible schema).
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Project identity sent to OpenRouter as `HTTP-Referer` (for usage analytics).
const OPENROUTER_REFERER: &str = "https://github.com/bobmatnyc/trusty-git-analytics";

/// Project identity sent to OpenRouter as `X-Title`.
const OPENROUTER_TITLE: &str = "trusty-git-analytics";

/// System prompt instructing the model to return strict JSON.
///
/// Why: shared between the HTTP (OpenRouter/Anthropic-API) path and the
/// Bedrock path so both send identical instructions and parse the same JSON
/// shape — including the `complexity` field. This closes the P0 complexity
/// gap where the Bedrock path was using its own trimmed prompt that omitted
/// the complexity instruction.
/// What: instructs the model to return a JSON object with `category`,
/// `subcategory`, `confidence`, and `complexity` fields.
/// Test: `system_prompt_requests_complexity` in this module; also used by
/// `bedrock::tests::shared_system_prompt_contains_complexity_instruction`.
pub const SYSTEM_PROMPT: &str = "You are a git commit classifier. Respond with ONLY a JSON \
object: {\"category\": \"feature|bugfix|chore|documentation|refactor|test|ci|performance|style|build|revert|merge|breaking|uncategorized\", \
\"subcategory\": \"optional string or null\", \"confidence\": 0.0-1.0, \
\"complexity\": <integer 1-5>}. \
Complexity 1-5: \
1=trivial (config/version bump/typo), 2=simple (single-file bugfix), \
3=moderate (multi-file feature), 4=complex (cross-module/arch change), \
5=highly complex (system design/major refactor). \
No prose, no markdown. \
Example: {\"category\": \"bugfix\", \"subcategory\": \"null-check\", \
\"confidence\": 0.9, \"complexity\": 2}";

/// Tier-4 LLM-fallback classifier.
pub struct LlmClassifier {
    client: Client,
    model: String,
    api_key: Option<String>,
    endpoint: String,
    /// Provider-specific extra headers (e.g. OpenRouter attribution).
    extra_headers: HeaderMap,
    /// Bedrock backend, populated only when provider == `"bedrock"`. When
    /// `Some`, [`Self::classify`] routes through Bedrock instead of HTTP.
    bedrock: Option<BedrockClassifier>,
}

impl LlmClassifier {
    /// Construct a new LLM classifier targeting the OpenAI chat-completions
    /// endpoint.
    ///
    /// `model` is provider-specific (e.g. `"gpt-4o-mini"`). If `api_key` is
    /// `None`, classification calls will return `None` immediately.
    pub fn new(model: &str, api_key: Option<String>) -> Self {
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            extra_headers: HeaderMap::new(),
            bedrock: None,
        }
    }

    /// Construct an LLM classifier configured for a specific provider.
    ///
    /// `provider` accepts:
    /// - `"openrouter"` — uses the OpenRouter endpoint. API key comes from
    ///   `openrouter_api_key` if `Some`, else the `OPENROUTER_API_KEY`
    ///   environment variable. Adds the `HTTP-Referer` / `X-Title` headers
    ///   that OpenRouter uses for attribution.
    /// - `"openai"` — uses the OpenAI endpoint. API key comes from
    ///   `OPENAI_API_KEY`.
    /// - `"auto"` (default) — tries OpenRouter first, falls back to OpenAI.
    ///
    /// If no API key can be resolved, the classifier is still constructed
    /// but every `classify` call will short-circuit to `None`.
    pub fn from_provider(
        provider: &str,
        model: &str,
        openrouter_api_key: Option<String>,
    ) -> Result<Self, String> {
        let normalized = provider.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "openrouter" => Ok(Self::build_openrouter(model, openrouter_api_key)),
            "openai" => Ok(Self::new(model, std::env::var("OPENAI_API_KEY").ok())),
            "bedrock" => {
                // Bedrock requires async SDK initialization. Direct sync
                // callers get a clear error so they know to use
                // [`Self::from_provider_async`] (or rebuild without the
                // feature, depending on the binary configuration).
                info!(model, "LLM provider: bedrock (requested via sync path)");
                #[cfg(feature = "bedrock")]
                {
                    Err("bedrock provider requires the async constructor; use \
                         LlmClassifier::from_provider_async"
                        .to_string())
                }
                #[cfg(not(feature = "bedrock"))]
                {
                    let _ = model;
                    Err(
                        "bedrock feature not compiled in — rebuild with --features bedrock"
                            .to_string(),
                    )
                }
            }
            "auto" | "" => {
                let or_key =
                    openrouter_api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
                if or_key.is_some() {
                    info!("LLM provider auto-selected: openrouter");
                    Ok(Self::build_openrouter(model, or_key))
                } else {
                    info!("LLM provider auto-selected: openai");
                    Ok(Self::new(model, std::env::var("OPENAI_API_KEY").ok()))
                }
            }
            other => {
                warn!(
                    provider = %other,
                    "unknown LLM provider; falling back to OpenAI endpoint"
                );
                Ok(Self::new(model, std::env::var("OPENAI_API_KEY").ok()))
            }
        }
    }

    /// Async variant of [`Self::from_provider`] that supports the `"bedrock"`
    /// provider (whose SDK requires async credential resolution).
    ///
    /// All other provider strings delegate to the sync constructor.
    ///
    /// Why: `from_provider` is called from sync engine setup; only the
    /// Bedrock arm truly needs `.await`, so the sync entry point handles
    /// the common case and this exists for async callers needing Bedrock.
    /// What: matches on `provider`, calls `BedrockClassifier::new(...)` for
    /// `"bedrock"`, otherwise falls back to sync.
    /// Test: indirectly via the CLI integration when invoked with
    /// `--provider bedrock`; the missing-feature path is asserted in the
    /// `bedrock` module's stub test.
    pub async fn from_provider_async(
        provider: &str,
        model: &str,
        openrouter_api_key: Option<String>,
    ) -> Result<Self, String> {
        if provider.trim().eq_ignore_ascii_case("bedrock") {
            info!(model, "LLM provider: bedrock (async init)");
            let bedrock = BedrockClassifier::new(model).await?;
            return Ok(Self {
                client: Client::new(),
                model: model.to_string(),
                api_key: None,
                endpoint: String::new(),
                extra_headers: HeaderMap::new(),
                bedrock: Some(bedrock),
            });
        }
        Self::from_provider(provider, model, openrouter_api_key)
    }

    /// Build an [`LlmClassifier`] from the top-level `llm:` config section.
    ///
    /// Why: the new `llm:` section cleanly separates transport config from
    /// classification tuning; this constructor is the single entry point that
    /// maps `LlmConfig` → a working classifier, handling key resolution,
    /// missing-feature errors, and the not-yet-implemented `anthropic-api`
    /// variant.
    /// What: reads the API key from the env var named by `cfg.api_key_env` for
    /// key-based sources; routes Bedrock through the async SDK constructor;
    /// returns `Err` with an actionable message for `anthropic-api`.
    /// Test: indirectly via the classify pipeline when `llm:` is set in the
    /// YAML config; key-resolution errors are asserted by
    /// `from_llm_config_missing_key_errors`.
    ///
    /// # Errors
    ///
    /// - `bedrock` source without the feature compiled in → error with
    ///   "reinstall with --features bedrock" guidance.
    /// - `anthropic-api` source → error with "not yet implemented" guidance.
    /// - Key-based source with unset / empty env var → error naming the
    ///   missing variable and how to set it.
    pub async fn from_llm_config(cfg: &LlmConfig, model: &str) -> Result<Self, String> {
        match &cfg.source {
            LlmSource::Openrouter => {
                // Read key from the named env var; fail loudly if unset.
                let key = std::env::var(&cfg.api_key_env)
                    .ok()
                    .filter(|k| !k.is_empty());
                if key.is_none() {
                    return Err(format!(
                        "LLM source 'openrouter' requires an API key but the environment \
                         variable '{}' (set via llm.api_key_env) is not set or empty. \
                         Export the variable with your OpenRouter API key before running tga.",
                        cfg.api_key_env
                    ));
                }
                info!(
                    model,
                    api_key_env = %cfg.api_key_env,
                    "LLM provider: openrouter (from llm: config section)"
                );
                Ok(Self::build_openrouter(model, key))
            }
            LlmSource::Bedrock => {
                info!(
                    model,
                    region = ?cfg.region,
                    "LLM provider: bedrock (from llm: config section)"
                );
                let bedrock = BedrockClassifier::with_region(model, cfg.region.as_deref()).await?;
                Ok(Self {
                    client: Client::new(),
                    model: model.to_string(),
                    api_key: None,
                    endpoint: String::new(),
                    extra_headers: HeaderMap::new(),
                    bedrock: Some(bedrock),
                })
            }
            LlmSource::AnthropicApi => Err("LLM source 'anthropic-api' is not yet implemented. \
                     Use 'openrouter' or 'bedrock' instead."
                .to_string()),
        }
    }

    /// Internal helper: build an OpenRouter-configured classifier with
    /// attribution headers set.
    fn build_openrouter(model: &str, api_key: Option<String>) -> Self {
        let key = api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        let mut headers = HeaderMap::new();
        // These are static, valid ASCII strings — `from_static` cannot panic
        // on them at runtime.
        headers.insert("HTTP-Referer", HeaderValue::from_static(OPENROUTER_REFERER));
        headers.insert("X-Title", HeaderValue::from_static(OPENROUTER_TITLE));
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key: key,
            endpoint: OPENROUTER_ENDPOINT.to_string(),
            extra_headers: headers,
            bedrock: None,
        }
    }

    /// Override the chat-completions endpoint URL (e.g. for Azure / local proxies).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Whether this classifier has a usable credential.
    ///
    /// `true` when an API key was resolved (for HTTP providers) or when a
    /// Bedrock backend is wired up. `false` indicates `classify` would
    /// short-circuit to `None` for HTTP providers — the pipeline uses this
    /// at startup to emit a single warning instead of silently producing
    /// "LLM fallback did not improve confidence" for every commit.
    pub fn has_api_key(&self) -> bool {
        self.bedrock.is_some() || self.api_key.is_some()
    }

    /// Classify `message` by calling the LLM.
    ///
    /// Returns `None` if the LLM is disabled (no API key), the request
    /// fails, or the response cannot be parsed.
    pub async fn classify(&self, message: &str) -> Option<ClassificationResult> {
        if let Some(bedrock) = &self.bedrock {
            return bedrock
                .classify_batch_bedrock(&[message])
                .await
                .into_iter()
                .next()
                .flatten();
        }
        let api_key = self.api_key.as_deref()?;

        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user",
                    content: format!("Classify this commit message:\n\n{message}"),
                },
            ],
            temperature: 0.0,
            response_format: Some(ResponseFormat {
                kind: "json_object".to_string(),
            }),
        };

        let response = match self
            .client
            .post(&self.endpoint)
            .bearer_auth(api_key)
            .headers(self.extra_headers.clone())
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "LLM request failed");
                return None;
            }
        };

        if !response.status().is_success() {
            warn!(status = %response.status(), "LLM returned non-success status");
            return None;
        }

        let parsed: ChatResponse = match response.json().await {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "LLM response JSON decode failed");
                return None;
            }
        };

        let content = parsed.choices.first()?.message.content.clone();
        debug!(content = %content, "LLM raw response");

        let verdict: LlmVerdict = serde_json::from_str(&content)
            .map_err(|e| warn!(error = %e, "LLM JSON parse failed"))
            .ok()?;

        Some(ClassificationResult {
            category: verdict.category,
            subcategory: verdict.subcategory,
            top_level: None, // resolved by ClassificationEngine via the taxonomy registry
            confidence: verdict.confidence.clamp(0.0, 1.0),
            method: ClassificationMethod::LlmFallback,
            ticket_id: None,
            // Clamp out-of-range LLM scores into the documented 1–5 band.
            complexity: verdict.complexity.map(|v| v.clamp(1, 5)),
        })
    }
}

// ---- request / response DTOs (private) ----

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

/// JSON verdict shape returned by the LLM (both HTTP and Bedrock paths).
///
/// Why: sharing this struct between the HTTP path (`llm.rs`) and the Bedrock
/// path (`bedrock.rs`) ensures both parse the same JSON keys. Before this was
/// made public the Bedrock path defined its own `Verdict` struct that omitted
/// the `complexity` field, producing no complexity scores (P0 gap).
/// What: deserializes `category`, `subcategory`, `confidence`, and
/// `complexity` from the model's JSON response.
/// Test: `llm_verdict_deserializes_complexity` in this module; also used by
/// `bedrock::classify_one` (integration path requires live AWS credentials).
#[derive(Debug, Deserialize)]
pub struct LlmVerdict {
    /// Classification category (e.g. `"bugfix"`, `"feature"`).
    pub category: String,
    /// Optional leaf label (e.g. `"null-check"`).
    #[serde(default)]
    pub subcategory: Option<String>,
    /// Confidence in this verdict (0.0–1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Optional 1–5 complexity score. Missing or out-of-range values are
    /// handled by the caller (clamped to 1–5, or left `None`).
    #[serde(default)]
    pub complexity: Option<u8>,
}

/// Why: when the model omits `confidence` (malformed response), use 0.5
/// so the verdict is not silently discarded by the confidence guard.
/// What: returns `0.5`.
/// Test: covered by `LlmVerdict` deserialization tests.
pub fn default_confidence() -> f64 {
    0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn has_api_key_reflects_key_state() {
        let with_key = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()));
        assert!(with_key.has_api_key());
        let without_key = LlmClassifier::new("gpt-4o-mini", None);
        assert!(!without_key.has_api_key());
    }

    #[tokio::test]
    async fn classify_returns_none_without_api_key() {
        let llm = LlmClassifier::new("gpt-4o-mini", None);
        assert!(llm.classify("feat: anything").await.is_none());
    }

    /// Regression for the hive-review finding in PR #(issue 99): the raw
    /// `LlmClassifier` always sets `ticket_id: None`. The engine wrapper
    /// (`Engine::llm_classify_only`) backfills it via regex extraction.
    /// This test pins the raw classifier's contract so any future change
    /// that starts surfacing `ticket_id` from the LLM verdict prompts the
    /// engine wrapper to be revisited (so we don't double-backfill).
    #[tokio::test]
    async fn classify_does_not_set_ticket_id() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "{\"category\": \"bugfix\", \
                                 \"subcategory\": null, \
                                 \"confidence\": 0.8}"
                }
            }]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let llm = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
        let r = llm
            .classify("fix: handle null in PROJ-1234 endpoint")
            .await
            .expect("LLM verdict");
        assert_eq!(r.ticket_id, None);
    }

    /// Why: the LLM tier is the only producer of complexity scores; if the
    /// `complexity` key stops deserializing, all scoring silently breaks.
    /// What: a JSON verdict with `"complexity": 3` must deserialize to
    /// `Some(3)`, and a verdict omitting the key must default to `None`.
    /// Test: `serde_json::from_str` two payloads and assert the field.
    #[test]
    fn llm_verdict_deserializes_complexity() {
        let with: LlmVerdict =
            serde_json::from_str(r#"{"category":"feature","confidence":0.9,"complexity":3}"#)
                .expect("deserialize verdict with complexity");
        assert_eq!(with.complexity, Some(3));

        let without: LlmVerdict =
            serde_json::from_str(r#"{"category":"feature","confidence":0.9}"#)
                .expect("deserialize verdict without complexity");
        assert_eq!(without.complexity, None);
    }

    /// Why: the model only emits complexity if the prompt asks for it; this
    /// pins the prompt so a future edit can't drop the request silently.
    /// What: asserts the system prompt mentions the `complexity` key.
    /// Test: substring check on `SYSTEM_PROMPT`.
    #[test]
    fn system_prompt_requests_complexity() {
        assert!(
            SYSTEM_PROMPT.contains("complexity"),
            "system prompt must instruct the model to return a complexity score"
        );
    }

    #[tokio::test]
    async fn classify_dispatches_to_endpoint_when_keyed() {
        // Regression: issue #99 — when the pipeline asks the LLM tier
        // directly, an HTTP call must happen even for messages a regex
        // tier would have caught. This test verifies the raw classifier
        // hits its configured endpoint and returns the LLM verdict.
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "{\"category\": \"feature\", \
                                 \"subcategory\": \"new-auth\", \
                                 \"confidence\": 0.91}"
                }
            }]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let llm = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
        let r = llm.classify("chore: bump deps").await.expect("LLM verdict");
        assert_eq!(r.category, "feature");
        assert_eq!(r.subcategory.as_deref(), Some("new-auth"));
        assert!((r.confidence - 0.91).abs() < 1e-6);
        assert_eq!(r.method, ClassificationMethod::LlmFallback);
    }
}
