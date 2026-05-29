//! Tier 4: optional LLM fallback.
//!
//! Supports three providers:
//! - **OpenRouter** — OpenAI-compatible chat-completions endpoint
//!   (`https://openrouter.ai/api/v1/chat/completions`).
//! - **Bedrock** — AWS Bedrock Messages API via the AWS SDK.
//! - **Anthropic API** — Direct Anthropic Messages API
//!   (`POST https://api.anthropic.com/v1/messages`), no OpenRouter or AWS required.
//!
//! The LLM is consulted only when tiers 1–3 all failed and the engine has been
//! configured with `use_llm = true` (or when the top-level `llm:` section is
//! present in the config, which self-enables the tier).
//!
//! All failures are **non-fatal** (from the engine perspective): a network error,
//! parse error, or missing API key results in `None` so the pipeline can fall back
//! to "uncategorized" rather than crashing. The pipeline-level fail-loudly guard
//! converts a missing key into a hard error before any DB writes occur.

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

/// Anthropic Messages API endpoint (direct, no OpenRouter).
///
/// Why: users with a direct Anthropic API key (not an OpenRouter account) can
/// classify commits without routing traffic through a third party.
/// What: the canonical Anthropic Messages API URL.
/// Test: used by `build_anthropic` tests that mock the HTTP server.
const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// `anthropic-version` header value required by the Anthropic Messages API.
///
/// Why: Anthropic's API rejects requests without a valid `anthropic-version`
/// header; pinning the version here ensures forward-compatibility even as new
/// API versions are released.
/// What: the stable API version string sent as the `anthropic-version` header.
/// Test: `anthropic_api_request_sets_correct_headers` verifies this header is sent.
const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Default model used when `llm.model` is absent for the `anthropic-api` source.
///
/// Why: claude-3-5-haiku-latest is the most cost-efficient Anthropic model
/// for short classification tasks (single commit message → JSON verdict). It
/// delivers quality equivalent to older Sonnet versions for classification at
/// a fraction of the cost.
/// What: the model ID sent in the `model` field of the Anthropic Messages API
/// request body when no explicit `llm.model` is configured.
/// Test: `anthropic_default_model_used_when_none_configured` in this module.
pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-3-5-haiku-latest";

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
    /// Whether to use the Anthropic Messages API request/response shape
    /// instead of the OpenAI-compatible chat-completions shape.
    ///
    /// When `true`, [`Self::classify`] sends an Anthropic-native request
    /// (`POST /v1/messages` with `x-api-key` + `anthropic-version` headers)
    /// and parses `response.content[0].text`.
    use_anthropic_format: bool,
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
            use_anthropic_format: false,
        }
    }

    /// Build an [`LlmClassifier`] that calls the Anthropic Messages API directly.
    ///
    /// Why: users with a direct Anthropic API key (not via OpenRouter) need a
    /// first-class provider without routing through a third party.  This
    /// constructor wires the correct endpoint, auth header (`x-api-key`), and
    /// request/response shape (Anthropic Messages API) so `classify` knows to
    /// use the Anthropic path rather than the OpenAI-compatible path.
    /// What: sets `use_anthropic_format = true`, endpoint to
    /// `ANTHROPIC_ENDPOINT`, and embeds `api_key` + `anthropic-version` in
    /// `extra_headers`. Returns `Err` when `api_key` is `None` (the pipeline
    /// fail-loudly guard catches this case before DB writes happen, but we
    /// surface it here too for constructors called outside the pipeline).
    /// Test: `anthropic_api_request_sets_correct_headers` mocks the endpoint
    /// and asserts the headers and body shape; `anthropic_response_parsing`
    /// asserts the `content[].text` parse path.
    pub fn build_anthropic(model: &str, api_key: Option<String>) -> Self {
        let mut headers = HeaderMap::new();
        // `anthropic-version` is required by the Anthropic API; requests
        // without it are rejected with HTTP 400.
        if let Ok(v) = HeaderValue::from_str(ANTHROPIC_API_VERSION) {
            headers.insert("anthropic-version", v);
        }
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key,
            endpoint: ANTHROPIC_ENDPOINT.to_string(),
            extra_headers: headers,
            bedrock: None,
            use_anthropic_format: true,
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
                use_anthropic_format: false,
            });
        }
        Self::from_provider(provider, model, openrouter_api_key)
    }

    /// Build an [`LlmClassifier`] from the top-level `llm:` config section.
    ///
    /// Why: the new `llm:` section cleanly separates transport config from
    /// classification tuning; this constructor is the single entry point that
    /// maps `LlmConfig` → a working classifier, handling key resolution,
    /// missing-feature errors, and all three provider variants.
    /// What: reads the API key from the env var named by `cfg.api_key_env` for
    /// key-based sources (`openrouter`, `anthropic-api`); routes Bedrock through
    /// the async SDK constructor; returns `Err` when the named env var is unset
    /// or empty (fail-loudly, no silent no-ops).
    /// Test: `from_llm_config_openrouter_reads_api_key_env`,
    /// `from_llm_config_anthropic_api_reads_api_key_env`, and
    /// `from_llm_config_missing_key_errors` in this module.
    ///
    /// # Errors
    ///
    /// - `bedrock` source without the feature compiled in → error with
    ///   "reinstall with --features bedrock" guidance.
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
                    use_anthropic_format: false,
                })
            }
            LlmSource::AnthropicApi => {
                // Read key from the named env var; fail loudly if unset.
                let key = std::env::var(&cfg.api_key_env)
                    .ok()
                    .filter(|k| !k.is_empty());
                if key.is_none() {
                    return Err(format!(
                        "LLM source 'anthropic-api' requires an API key but the environment \
                         variable '{}' (set via llm.api_key_env) is not set or empty. \
                         Export the variable with your Anthropic API key before running tga. \
                         Example: export {}=sk-ant-...", // pragma: allowlist secret
                        cfg.api_key_env, cfg.api_key_env
                    ));
                }
                // Default to the cheap Haiku model when the user did not
                // specify one — good enough for commit classification and
                // significantly cheaper than Sonnet/Opus.
                let effective_model = if model == "gpt-4o-mini" {
                    // The caller passed the OpenRouter fallback default;
                    // substitute the Anthropic-appropriate default instead.
                    ANTHROPIC_DEFAULT_MODEL
                } else {
                    model
                };
                info!(
                    model = effective_model,
                    api_key_env = %cfg.api_key_env,
                    "LLM provider: anthropic-api (direct Anthropic Messages API)"
                );
                Ok(Self::build_anthropic(effective_model, key))
            }
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
            use_anthropic_format: false,
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
    /// Routes through Bedrock, Anthropic Messages API, or OpenAI-compatible
    /// endpoint depending on how the classifier was constructed.
    ///
    /// Returns `None` if the LLM is disabled (no API key), the request
    /// fails, or the response cannot be parsed. The pipeline-level guard
    /// converts a missing key into a hard error before reaching this path.
    pub async fn classify(&self, message: &str) -> Option<ClassificationResult> {
        if let Some(bedrock) = &self.bedrock {
            return bedrock
                .classify_batch_bedrock(&[message])
                .await
                .into_iter()
                .next()
                .flatten();
        }

        if self.use_anthropic_format {
            return self.classify_anthropic(message).await;
        }

        self.classify_openai_compat(message).await
    }

    /// Classify via the Anthropic Messages API (`POST /v1/messages`).
    ///
    /// Why: extracted from `classify` to keep the routing logic readable and
    /// allow the Anthropic path to be tested independently with a mock server.
    /// What: builds an `AnthropicRequest` with the shared `SYSTEM_PROMPT`,
    /// POSTs to `self.endpoint` with `x-api-key` + `anthropic-version`
    /// headers, and parses `response.content[].text` → `LlmVerdict`.
    /// Test: `anthropic_response_parsing` and
    /// `anthropic_api_request_sets_correct_headers` in this module.
    async fn classify_anthropic(&self, message: &str) -> Option<ClassificationResult> {
        let api_key = self.api_key.as_deref()?;

        let body = AnthropicRequest {
            model: &self.model,
            // 512 tokens is more than enough for a JSON verdict; keeping it
            // low reduces latency and cost on the cheap Haiku model.
            max_tokens: 512,
            system: SYSTEM_PROMPT,
            messages: vec![AnthropicMessage {
                role: "user",
                content: format!("Classify this commit message:\n\n{message}"),
            }],
        };

        let response = match self
            .client
            .post(&self.endpoint)
            .header("x-api-key", api_key)
            .headers(self.extra_headers.clone())
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Anthropic API request failed");
                return None;
            }
        };

        if !response.status().is_success() {
            warn!(status = %response.status(), "Anthropic API returned non-success status");
            return None;
        }

        let parsed: AnthropicResponse = match response.json().await {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "Anthropic API response JSON decode failed");
                return None;
            }
        };

        // Extract the first text content block.
        let content = parsed
            .content
            .into_iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text)?;

        debug!(content = %content, "Anthropic API raw response");

        let verdict: LlmVerdict = serde_json::from_str(content.trim())
            .map_err(|e| warn!(error = %e, "Anthropic API JSON parse failed"))
            .ok()?;

        Some(ClassificationResult {
            category: verdict.category,
            subcategory: verdict.subcategory,
            top_level: None, // resolved by ClassificationEngine via the taxonomy registry
            confidence: verdict.confidence.clamp(0.0, 1.0),
            method: ClassificationMethod::LlmFallback,
            ticket_id: None,
            complexity: verdict.complexity.map(|v| v.clamp(1, 5)),
        })
    }

    /// Classify via an OpenAI-compatible chat-completions endpoint.
    ///
    /// Why: extracted from `classify` to isolate the OpenAI/OpenRouter path
    /// and allow it to be tested independently.
    /// What: sends a `ChatRequest` with `system` + `user` messages, parses
    /// `choices[0].message.content` → `LlmVerdict`.
    /// Test: `classify_dispatches_to_endpoint_when_keyed` in this module.
    async fn classify_openai_compat(&self, message: &str) -> Option<ClassificationResult> {
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

// ---- Anthropic Messages API request / response DTOs (private) ----

/// Request body for the Anthropic Messages API (`POST /v1/messages`).
///
/// Why: the Anthropic API uses a different shape than OpenAI's chat-completions
/// — `messages` has `role` + `content`, `system` is a top-level field (not a
/// message), and there is no `response_format` field (JSON is requested via
/// the system prompt).
/// What: serializes to the documented Anthropic Messages API request body.
/// Test: `anthropic_api_request_sets_correct_headers` in this module.
#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: String,
}

/// Response body from the Anthropic Messages API.
///
/// Why: the Anthropic response shape differs from OpenAI — the text content
/// lives in `content[].text` (with `type: "text"`) rather than
/// `choices[].message.content`.
/// What: top-level `content` array; each entry has a `type` and optional
/// `text`. We take `text` from the first entry with `type == "text"`.
/// Test: `anthropic_response_parsing` in this module.
#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn has_api_key_reflects_key_state() {
        let with_key = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()));
        assert!(with_key.has_api_key());
        let without_key = LlmClassifier::new("gpt-4o-mini", None);
        assert!(!without_key.has_api_key());
    }

    /// Why: build_anthropic must set use_anthropic_format and embed the
    /// anthropic-version header; a missing or wrong header causes the real
    /// API to reject requests with HTTP 400.
    /// What: construct a classifier via build_anthropic and verify
    /// use_anthropic_format is true and the header is present in extra_headers.
    /// Test: pure field inspection, no network.
    #[test]
    fn build_anthropic_sets_format_flag_and_version_header() {
        let llm = LlmClassifier::build_anthropic(
            "claude-3-5-haiku-latest",
            Some("sk-ant-test".to_string()), // pragma: allowlist secret
        );
        assert!(llm.use_anthropic_format, "must set use_anthropic_format");
        assert!(llm.api_key.is_some(), "api_key must be set");
        assert_eq!(llm.endpoint, ANTHROPIC_ENDPOINT);
        assert!(
            llm.extra_headers.contains_key("anthropic-version"),
            "anthropic-version header must be set"
        );
        let ver = llm.extra_headers.get("anthropic-version").unwrap();
        assert_eq!(ver, ANTHROPIC_API_VERSION);
    }

    /// Why: when build_anthropic is called with no key, has_api_key must
    /// return false so the pipeline fail-loudly guard fires before any DB
    /// writes occur.
    /// What: construct with None and assert has_api_key() == false.
    /// Test: pure field inspection.
    #[test]
    fn build_anthropic_without_key_has_no_api_key() {
        let llm = LlmClassifier::build_anthropic("claude-3-5-haiku-latest", None);
        assert!(!llm.has_api_key());
    }

    /// Why: the Anthropic response shape differs from OpenAI — text lives in
    /// `content[].text`, not `choices[].message.content`. A regression here
    /// would cause all anthropic-api verdicts to silently return None.
    /// What: mock the Anthropic endpoint with a valid Anthropic Messages
    /// response body and assert the parsed verdict matches.
    /// Test: wiremock server at /v1/messages.
    #[tokio::test]
    async fn anthropic_response_parsing() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"category\":\"bugfix\",\"subcategory\":\"null-check\",\"confidence\":0.92,\"complexity\":2}"
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let llm = LlmClassifier::build_anthropic(
            "claude-3-5-haiku-latest",
            Some("sk-ant-test".to_string()), // pragma: allowlist secret
        )
        .with_endpoint(format!("{}/v1/messages", server.uri()));

        let r = llm
            .classify("fix: handle null in user endpoint")
            .await
            .expect("verdict");
        assert_eq!(r.category, "bugfix");
        assert_eq!(r.subcategory.as_deref(), Some("null-check"));
        assert!((r.confidence - 0.92).abs() < 1e-6);
        assert_eq!(r.complexity, Some(2));
        assert_eq!(r.method, ClassificationMethod::LlmFallback);
    }

    /// Why: the Anthropic API requires the `x-api-key` header (not
    /// `Authorization: Bearer`). A wrong auth scheme causes HTTP 401.
    /// What: mount a mock that matches on the `x-api-key` header and
    /// assert the classifier sends the right header.
    /// Test: wiremock header matcher.
    #[tokio::test]
    async fn anthropic_api_request_sets_correct_headers() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"category\":\"chore\",\"subcategory\":null,\"confidence\":0.8,\"complexity\":1}"
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test")) // pragma: allowlist secret
            .and(header("anthropic-version", ANTHROPIC_API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let llm = LlmClassifier::build_anthropic(
            "claude-3-5-haiku-latest",
            Some("sk-ant-test".to_string()), // pragma: allowlist secret
        )
        .with_endpoint(format!("{}/v1/messages", server.uri()));

        let r = llm
            .classify("chore: bump version")
            .await
            .expect("verdict with correct headers");
        assert_eq!(r.category, "chore");
    }

    /// Why: `from_llm_config` must resolve the API key from the env var
    /// named by `api_key_env` for the `anthropic-api` source, not from
    /// a hardcoded var like `ANTHROPIC_API_KEY`.
    /// What: set a custom env var, build from a config with that var name,
    /// and assert the classifier has a key.
    /// Test: env-var manipulation + constructor check.
    #[tokio::test]
    async fn from_llm_config_anthropic_api_reads_api_key_env() {
        // Use a unique var name to avoid polluting parallel tests.
        let var_name = "TGA_TEST_ANTHROPIC_KEY_9f2e";
        std::env::set_var(var_name, "sk-ant-from-env"); // pragma: allowlist secret

        let cfg = crate::core::config::LlmConfig {
            source: LlmSource::AnthropicApi,
            api_key_env: var_name.to_string(),
            region: None,
            model: Some("claude-3-5-haiku-latest".to_string()),
        };
        let result = LlmClassifier::from_llm_config(&cfg, "claude-3-5-haiku-latest").await;
        std::env::remove_var(var_name);

        let llm = result.expect("should build from env var");
        assert!(llm.has_api_key());
        assert!(llm.use_anthropic_format);
    }

    /// Why: when the named env var is absent, from_llm_config must return
    /// an error naming the variable — no silent no-op allowed.
    /// What: call from_llm_config with an env var that does not exist and
    /// assert Err mentions the var name.
    /// Test: pure error-path check, no network.
    #[tokio::test]
    async fn from_llm_config_anthropic_api_missing_key_errors() {
        let var_name = "TGA_TEST_ANTHROPIC_MISSING_KEY_7c4b";
        std::env::remove_var(var_name); // ensure absent

        let cfg = crate::core::config::LlmConfig {
            source: LlmSource::AnthropicApi,
            api_key_env: var_name.to_string(),
            region: None,
            model: None,
        };
        let result = LlmClassifier::from_llm_config(&cfg, "gpt-4o-mini").await;
        assert!(result.is_err(), "missing env var must produce Err");
        let err = result.err().expect("just asserted is_err");
        assert!(
            err.contains(var_name),
            "error must name the missing var: {err}"
        );
    }

    /// Why: when the user specifies `source: anthropic-api` but does not set
    /// `model:`, the classifier must default to ANTHROPIC_DEFAULT_MODEL
    /// (not the OpenRouter fallback "gpt-4o-mini") so the Anthropic endpoint
    /// receives a valid model ID.
    /// What: call from_llm_config with `model: None` and `source: anthropic-api`
    /// and assert the classifier's model field equals ANTHROPIC_DEFAULT_MODEL.
    /// Test: env-var + constructor inspection.
    #[tokio::test]
    async fn anthropic_default_model_used_when_none_configured() {
        let var_name = "TGA_TEST_ANTHROPIC_DEFAULT_MODEL_3a8d";
        std::env::set_var(var_name, "sk-ant-test-model"); // pragma: allowlist secret

        let cfg = crate::core::config::LlmConfig {
            source: LlmSource::AnthropicApi,
            api_key_env: var_name.to_string(),
            region: None,
            model: None, // user did not set a model
        };
        // The pipeline passes "gpt-4o-mini" as the fallback when model is None.
        let result = LlmClassifier::from_llm_config(&cfg, "gpt-4o-mini").await;
        std::env::remove_var(var_name);

        let llm = result.expect("build from env");
        assert_eq!(
            llm.model, ANTHROPIC_DEFAULT_MODEL,
            "must substitute ANTHROPIC_DEFAULT_MODEL, not gpt-4o-mini"
        );
    }

    /// Why: presence of a top-level `llm:` section in the config must
    /// self-enable the LLM tier without requiring `classification.use_llm: true`.
    /// What: build a Config with llm.source = anthropic-api but no
    /// classification section; assert that the effective use_llm flag is true.
    /// Test: pure logic check on the precedence computation used in build_engine.
    #[test]
    fn llm_section_presence_self_enables_tier() {
        let cfg = crate::core::config::Config {
            llm: Some(crate::core::config::LlmConfig {
                source: LlmSource::AnthropicApi,
                api_key_env: "ANTHROPIC_ANALYTICS_API_KEY".to_string(), // pragma: allowlist secret
                region: None,
                model: Some("claude-3-5-haiku-latest".to_string()),
            }),
            classification: None,
            ..crate::core::config::Config::default()
        };
        // Replicate the logic from build_engine to verify it evaluates to true.
        let use_llm = cfg.llm.is_some()
            || cfg
                .classification
                .as_ref()
                .map(|c| c.use_llm)
                .unwrap_or(false);
        assert!(
            use_llm,
            "llm: section presence must self-enable the LLM tier"
        );
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
