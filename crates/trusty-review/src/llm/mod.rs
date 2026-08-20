//! LLM provider abstraction for trusty-review.
//!
//! Why: the pipeline and diff summarizer must call the LLM through a trait
//! so the provider (OpenRouter, Bedrock, or Fireworks) is an implementation
//! detail invisible to the caller, and so tests can inject mocks.
//! What: defines the `LlmProvider` trait (single non-streaming `complete`
//! call), `LlmRequest` / `LlmResponse` data shapes, the `build_provider`
//! factory, and re-exports the `LlmError` enum, `OpenRouterProvider`,
//! `BedrockProvider`, `FireworksProvider`, and `models` constants.
//! Test: each submodule carries its own unit tests; this module's
//! `provider_trait_object_compiles` smoke-tests that the trait is
//! object-safe; `provider_factory_*` tests cover the routing logic.

pub mod bedrock;
pub mod error;
pub mod fireworks;
pub mod models;
pub mod openrouter;
pub mod schema;

// #5675: strict-mode compliance for the whole set of sent schemas, kept here
// (not beside any one builder) so a new schema cannot be added without it.
#[cfg(test)]
#[path = "all_schemas_tests.rs"]
mod all_schemas_tests;

pub use bedrock::BedrockProvider;
pub use error::LlmError;
pub use fireworks::FireworksProvider;
pub use openrouter::OpenRouterProvider;
pub use schema::enforce_strict_mode;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::{Provider, ReviewConfig};

// ─── Chat message (simplified, no tool-use for review calls) ─────────────────

/// A single chat message for an LLM completion request.
///
/// Why: the review pipeline sends system + user messages; we do not need
/// the full `trusty_common::ChatMessage` tool-call fields for review calls.
/// What: a minimal role + content pair.
/// Test: covered transitively by `LlmRequest` tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    /// Content text.
    pub content: String,
}

// ─── Structured output spec ───────────────────────────────────────────────────

/// Specification for forced structured output from an LLM provider.
///
/// Why: eliminates the "silent fail-safe APPROVE" problem caused by models
/// not reliably emitting the required JSON verdict block in free-text
/// responses (confirmed in live Bedrock testing: Haiku always fail-safes,
/// Sonnet sometimes does).  When this spec is present the provider MUST
/// force the model to produce JSON conforming to the schema, and
/// `LlmResponse.text` will contain ONLY the clean JSON object —
/// directly deserializable without any fence-stripping.
/// What: holds a `name` (used as the Bedrock tool name and the
/// OpenRouter `json_schema.name`) and a `schema` JSON Schema object
/// describing the expected response shape.
/// When absent, provider behavior is unchanged (free text).
/// Test: `structured_output_spec_roundtrip` in this module;
/// provider-level tests in `bedrock` and `openrouter` modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSchema {
    /// Name for this schema (used as the Bedrock tool name and the
    /// OpenRouter `json_schema.name` field; must be a valid identifier).
    pub name: String,
    /// JSON Schema object describing the expected response structure.
    pub schema: serde_json::Value,
}

impl ResponseSchema {
    /// Build a schema spec, making it OpenAI strict-mode compliant on the way in.
    ///
    /// Why: #1235 added [`enforce_strict_mode`] but left applying it to each
    /// schema builder's own discipline, so `synthesis_schema` and
    /// `investigation_schema` were written by hand and never called it — the
    /// same defect shipped a second time as #5675 (`report_synthesis` rejected
    /// with `'additionalProperties' is required to be supplied and to be
    /// false`). Routing construction through one constructor makes compliance
    /// the default a new schema inherits rather than a step it must remember.
    /// What: applies [`enforce_strict_mode`] to `schema` — recursively setting
    /// `additionalProperties: false` and listing every property in `required`
    /// on each object node, including objects under an array's `items` — then
    /// stores it alongside `name`. Idempotent, so an already-strict schema is
    /// unchanged.
    /// Test: `every_sent_schema_is_openai_strict_compliant`,
    /// `synthesis_findings_items_is_strict`, and
    /// `schema_enumeration_is_complete_and_nothing_bypasses_new` in
    /// `llm/all_schemas_tests.rs`.
    pub fn new(name: impl Into<String>, mut schema: serde_json::Value) -> Self {
        schema::enforce_strict_mode(&mut schema);
        Self {
            name: name.into(),
            schema,
        }
    }
}

// ─── Request / response types ─────────────────────────────────────────────────

/// Input to `LlmProvider::complete`.
///
/// Why: carries all per-call parameters so each role (reviewer, verifier,
/// summarizer) can vary temperature and max_tokens independently.
/// What: `model` is the fully-resolved model id for this call (set by
/// `RoleModels`); `system` is the system prompt; `messages` are the user
/// turns.  When `response_schema` is set, the provider MUST force
/// structured output (Bedrock tool-use / OpenRouter json_schema).
/// Test: constructed in `LlmProvider` implementation tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Fully-resolved model id for this call.
    pub model: String,
    /// System prompt (empty string = no system message).
    pub system: String,
    /// User conversation turns.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature (0.0–2.0).
    pub temperature: f32,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Optional structured-output spec.  When present, the provider forces
    /// the model to emit JSON conforming to this schema; `LlmResponse.text`
    /// will contain only the clean JSON object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<ResponseSchema>,
}

/// Output from `LlmProvider::complete`.
///
/// Why: captures the response text plus all telemetry fields required by
/// spec REV-330 and the Stage-3 `compare` mode (speed, cost, tokens).
/// What: `text` is the full response; `model` echoes the actual model id
/// used (providers may normalise or alias the requested id); `input_tokens` /
/// `output_tokens` come from the API usage field; `latency_ms` is wall-clock
/// end-to-end; `cost_usd` is an estimate from a pricing table; `finish_reason`
/// is the provider's normalised completion reason (`"stop"` / `"length"` / …)
/// when surfaced, used as the PRIMARY truncation signal (#1357).
/// Test: `llm_response_serde_roundtrip` in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Full response text.
    pub text: String,
    /// Model id actually used by the provider (echoed from the response).
    pub model: String,
    /// Number of input (prompt) tokens consumed.
    pub input_tokens: u32,
    /// Number of output (completion) tokens generated.
    pub output_tokens: u32,
    /// Wall-clock latency in milliseconds from request send to last byte received.
    pub latency_ms: u64,
    /// Estimated USD cost for this call based on the model pricing table.
    pub cost_usd: f64,
    /// Provider completion / stop reason, normalised to a lowercase token
    /// (#1357).  `Some("stop")` / `Some("end_turn")` means the model finished
    /// naturally; `Some("length")` / `Some("max_tokens")` means it was cut off
    /// at the output-token ceiling (a genuine truncation).  `None` when the
    /// provider did not surface a reason — callers fall back to the token-ratio
    /// heuristic in that case.  Defaulted on deserialise so older serialised
    /// `ReviewResult` logs (and test fakes) remain compatible.
    #[serde(default)]
    pub finish_reason: Option<String>,
}

// ─── Provider trait ───────────────────────────────────────────────────────────

/// LLM completion provider.
///
/// Why: the pipeline and tests depend on this trait rather than concrete
/// types so the provider can be swapped without touching pipeline code
/// (OpenRouter for local dev, Bedrock for production).
/// What: a single `complete` method that takes `LlmRequest` and returns
/// `LlmResponse` or `LlmError`.  Implementors are `Send + Sync` so they
/// can be held behind `Arc<dyn LlmProvider>`.
/// Test: `provider_trait_object_compiles` verifies the trait is object-safe.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Human-readable provider name, e.g. `"openrouter"` or `"bedrock"`.
    ///
    /// Why: logs and metrics tag calls with the provider name.
    /// What: a static string slice; no heap allocation.
    /// Test: each implementation asserts its own name.
    fn name(&self) -> &str;

    /// Execute a non-streaming completion and return the full response.
    ///
    /// Why: the pipeline needs the full text, token counts, latency, and cost
    /// in one call — streaming complicates token-count capture.
    /// What: sends `req` to the upstream API, waits for the full response,
    /// and returns `LlmResponse`.  Transient errors may be retried by the
    /// caller based on `LlmError::is_retryable`.
    /// Test: each provider implements mock-server tests.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
}

// ─── Provider factory ─────────────────────────────────────────────────────────

/// Routing prefixes for model-id based provider selection.
///
/// Why: mirrors the `bedrock/` prefix convention from trusty-analyze so
/// operators can paste model ids from the CLAUDE.md examples without needing
/// to know which provider a model belongs to.
/// What: the model id is stripped of this prefix before being passed to the
/// provider constructor.
pub const BEDROCK_MODEL_PREFIX: &str = "bedrock/";
/// OpenRouter model-id prefix for explicit routing.
pub const OPENROUTER_MODEL_PREFIX: &str = "openrouter/";
/// Fireworks model-id prefix for explicit routing (matches trusty-code's
/// `fireworks/*` slug convention; the bare id is the provider-native
/// `accounts/fireworks/models/*` form).
pub const FIREWORKS_MODEL_PREFIX: &str = "fireworks/";

/// Strip the provider routing prefix from a model id, returning the bare id.
///
/// Why: `LlmRequest.model` must be the bare id sent to the upstream API — the
/// `bedrock/`, `openrouter/`, and `fireworks/` prefixes are routing hints only
/// and are never valid API model ids.  Using the prefixed string causes
/// Bedrock to receive `bedrock/us.anthropic.claude-sonnet-4-6` as the Converse
/// model id, which produces `POST /model/bedrock%2F.../converse` → HTTP 400
/// ValidationException.
/// What: strips the `bedrock/`, `openrouter/`, or `fireworks/` prefix if
/// present; returns the remaining string.  Bare ids (no prefix) are returned
/// unchanged.
/// Test: `prefix_stripped_model_id_bedrock`, `prefix_stripped_model_id_openrouter`,
/// `prefix_stripped_model_id_fireworks`, `prefix_stripped_model_id_bare`.
pub fn strip_provider_prefix(model: &str) -> &str {
    if let Some(bare) = model.strip_prefix(BEDROCK_MODEL_PREFIX) {
        return bare;
    }
    if let Some(bare) = model.strip_prefix(OPENROUTER_MODEL_PREFIX) {
        return bare;
    }
    if let Some(bare) = model.strip_prefix(FIREWORKS_MODEL_PREFIX) {
        return bare;
    }
    model
}

/// Resolve the effective provider and bare model id from a potentially-prefixed
/// model id string and an explicit provider hint.
///
/// Why: `--reviewer-model bedrock/us.anthropic.claude-sonnet-4-6` must route to
/// Bedrock regardless of the default provider; `openrouter/openai/gpt-5.4-mini`
/// must route to OpenRouter; `fireworks/accounts/fireworks/models/*` must route
/// to Fireworks. This function is the single source of truth for that logic.
///
/// Until #6114 an id with NO prefix was handed to `default_provider` whatever it
/// looked like, so an OpenRouter slug against a Bedrock-default path resolved to
/// Bedrock, which then rejected the id — and the run that eventually completed
/// used Bedrock's own default model rather than the one asked for. An
/// unambiguous shape now names its own provider, and a pair that cannot be
/// reconciled is an error instead of a substitution.
///
/// What: strips a known prefix, else infers the provider from the id's shape via
/// [`trusty_common::inference::infer_provider_from_model_shape`], else falls
/// back to `default_provider`. Whichever route is taken, the resulting pair is
/// checked for a shape conflict before it is returned. Precedence: explicit
/// prefix > unambiguous id shape > `default_provider`.
///
/// # Errors
///
/// [`LlmError::Validation`], naming both the requested id and the provider that
/// would execute it, when the two disagree.
///
/// Test: `provider_factory_prefix_routing`,
/// `bare_openrouter_slug_routes_to_openrouter_not_the_default`,
/// `bare_bedrock_profile_id_routes_to_bedrock_not_the_default`,
/// `prefix_that_contradicts_the_id_shape_is_an_error`,
/// `ambiguous_bare_id_still_uses_the_default_provider`.
pub fn resolve_provider_and_model(
    model: &str,
    default_provider: &Provider,
) -> Result<(Provider, String), LlmError> {
    if let Some(bare) = model.strip_prefix(BEDROCK_MODEL_PREFIX) {
        return reconcile(Provider::Bedrock, bare, model);
    }
    if let Some(bare) = model.strip_prefix(OPENROUTER_MODEL_PREFIX) {
        return reconcile(Provider::OpenRouter, bare, model);
    }
    if let Some(bare) = model.strip_prefix(FIREWORKS_MODEL_PREFIX) {
        return reconcile(Provider::Fireworks, bare, model);
    }
    // #6114: no routing prefix — let an unambiguous id shape name its provider
    // before the configured default gets it.
    if let Some(inferred) = trusty_common::inference::infer_provider_from_model_shape(model)
        .and_then(Provider::from_provider_id)
    {
        return Ok((inferred, model.to_string()));
    }
    reconcile(default_provider.clone(), model, model)
}

/// Refuse a `(provider, model)` pair whose id names a different provider.
///
/// Why: #6114 — the id and the provider that will execute it must agree, and
/// when they do not the only safe outcome is a loud stop. Silently running the
/// provider's default model is the failure this exists to prevent.
/// What: `Ok((provider, bare))` when the shape agrees or is ambiguous;
/// [`LlmError::Validation`] naming `requested`, the inferred provider, and
/// `provider` otherwise. `requested` is the operator's original string (prefix
/// included) so the message quotes what they actually typed.
/// Test: `prefix_that_contradicts_the_id_shape_is_an_error`,
/// `every_contradicting_pair_is_refused`.
fn reconcile(
    provider: Provider,
    bare: &str,
    requested: &str,
) -> Result<(Provider, String), LlmError> {
    if let Some(inferred) = trusty_common::inference::shape_mismatch(provider.provider_id(), bare) {
        return Err(LlmError::Validation(format!(
            "model id {requested:?} names the {inferred} provider by its shape, but this call \
             would run on {provider}. Refusing to substitute a different model than the one \
             requested. Either set the provider to {inferred}, or pin one explicitly with a \
             routing prefix (\"bedrock/…\", \"openrouter/…\", \"fireworks/…\").",
            inferred = inferred.as_str()
        )));
    }
    Ok((provider, bare.to_string()))
}

/// Build an `Arc<dyn LlmProvider>` from the resolved role config.
///
/// Why: the CLI, HTTP server, and pipeline all need to construct a provider
/// from the same config fields (provider, model, API key); centralising this
/// avoids duplicating the `bedrock/` prefix check in every call site.  Takes
/// the whole `ReviewConfig` (rather than a single key) because each API-key
/// provider reads a different key field (`openrouter_api_key`,
/// `fireworks_api_key`).
/// What: resolves the effective provider via `resolve_provider_and_model`,
/// constructs the appropriate concrete type, and returns it as a trait object.
/// Returns `LlmError::AccessDenied` if OpenRouter or Fireworks is selected but
/// its API key is empty.  Returns `LlmError::Validation` if the model id and the
/// resolved provider disagree (#6114), or if Bedrock is selected but the model
/// id is missing an inference-profile prefix.
/// Test: `provider_factory_builds_bedrock`, `provider_factory_builds_openrouter`,
/// `provider_factory_prefix_routing`.
pub async fn build_provider(
    model: &str,
    default_provider: &Provider,
    config: &ReviewConfig,
) -> Result<Arc<dyn LlmProvider>, LlmError> {
    let (provider, bare_model) = resolve_provider_and_model(model, default_provider)?;
    match provider {
        Provider::Bedrock => {
            let p = BedrockProvider::new(bare_model, None).await?;
            Ok(Arc::new(p))
        }
        Provider::OpenRouter => {
            let p = OpenRouterProvider::new(&config.openrouter_api_key, bare_model)?;
            Ok(Arc::new(p))
        }
        Provider::Fireworks => {
            let p = FireworksProvider::new(&config.fireworks_api_key, bare_model)?;
            Ok(Arc::new(p))
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

/// Build the verifier-role provider, treating a build failure as fatal.
///
/// Why: `serve` and the #5192 webhook drain both need the daemon-strength
/// semantics — unlike the one-shot CLI they must NOT silently degrade to
/// no-verification, because that failure is exactly what the liveness gate
/// exists to catch. One implementation so the two cannot drift; `cli_verify`
/// delegates here.
/// What: `Ok(None)` when verification is disabled, `Ok(Some(_))` when it builds,
/// `Err` when it is enabled and the build fails.
///
/// # Errors
///
/// When verification is enabled and the provider cannot be built.
///
/// Test: the build path is network-bound; the decision branch is covered by
/// `pipeline::verify_liveness::tests` through `enforce_verifier_liveness`.
pub async fn build_verifier_required(
    config: &crate::config::ReviewConfig,
) -> anyhow::Result<Option<std::sync::Arc<dyn LlmProvider>>> {
    if !config.verification.enabled {
        return Ok(None);
    }
    let role = &config.role_models.verifier;
    let provider = build_provider(&role.model, &role.provider, config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build verifier provider: {e}"))?;
    Ok(Some(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_response_serde_roundtrip() {
        let resp = LlmResponse {
            text: "LGTM".to_string(),
            model: "openai/gpt-5.4-mini-20260317".to_string(),
            input_tokens: 512,
            output_tokens: 64,
            latency_ms: 1234,
            cost_usd: 0.000123,
            finish_reason: None,
        };
        let json = serde_json::to_string(&resp).expect("serialise");
        let back: LlmResponse = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.text, "LGTM");
        assert_eq!(back.model, "openai/gpt-5.4-mini-20260317");
        assert_eq!(back.input_tokens, 512);
        assert_eq!(back.output_tokens, 64);
        assert_eq!(back.latency_ms, 1234);
        assert!((back.cost_usd - 0.000123_f64).abs() < 1e-15);
    }

    #[test]
    fn llm_request_serde_roundtrip() {
        let req = LlmRequest {
            model: "openai/gpt-5.4-20260305".to_string(),
            system: "You are a reviewer.".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "Review this.".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 2048,
            response_schema: None,
        };
        let json = serde_json::to_string(&req).expect("serialise");
        let back: LlmRequest = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.model, "openai/gpt-5.4-20260305");
        assert_eq!(back.messages.len(), 1);
        assert!((back.temperature - 0.3_f32).abs() < f32::EPSILON);
        assert!(back.response_schema.is_none(), "no schema in roundtrip");
    }

    /// Verify that `ResponseSchema` serialises and deserialises correctly.
    ///
    /// Why: the schema is passed through serde_json::Value; ensuring roundtrip
    /// fidelity prevents silent schema corruption at the provider boundary.
    /// What: constructs a `ResponseSchema`, serialises to JSON, deserialises,
    /// asserts field equality.
    /// Test: this test itself.
    #[test]
    fn structured_output_spec_roundtrip() {
        let schema = ResponseSchema {
            name: "review_output".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "verdict": { "type": "string" }
                },
                "required": ["verdict"]
            }),
        };
        let json = serde_json::to_string(&schema).expect("serialise");
        let back: ResponseSchema = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.name, "review_output");
        assert_eq!(back.schema["properties"]["verdict"]["type"], "string");
    }

    /// Verify that `LlmRequest` with a `response_schema` roundtrips correctly.
    ///
    /// Why: the optional field must survive serde serialization; providers
    /// read it from the request to decide whether to use structured output.
    /// What: constructs a request with a schema, serialises, deserialises,
    /// asserts the schema name is preserved.
    /// Test: this test itself.
    #[test]
    fn llm_request_with_schema_roundtrip() {
        let req = LlmRequest {
            model: "us.anthropic.claude-sonnet-4-6".to_string(),
            system: "reviewer".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "review".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 1024,
            response_schema: Some(ResponseSchema {
                name: "review_output".to_string(),
                schema: serde_json::json!({"type": "object"}),
            }),
        };
        let json = serde_json::to_string(&req).expect("serialise");
        let back: LlmRequest = serde_json::from_str(&json).expect("deserialise");
        let schema = back.response_schema.expect("schema must survive roundtrip");
        assert_eq!(schema.name, "review_output");
    }

    /// Object-safety smoke-test: ensures `LlmProvider` can be used as a
    /// `dyn` trait object.
    #[test]
    fn provider_trait_object_compiles() {
        // This test just needs to compile; the type coercion proves
        // LlmProvider is object-safe.
        fn _accepts_dyn(_p: &dyn LlmProvider) {}
    }

    // ── Provider factory routing tests ────────────────────────────────────

    /// Resolve, treating an error as a test failure.
    fn resolved(model: &str, default_provider: &Provider) -> (Provider, String) {
        resolve_provider_and_model(model, default_provider)
            .unwrap_or_else(|e| panic!("{model:?} must resolve: {e}"))
    }

    #[test]
    fn provider_factory_prefix_routing() {
        // bedrock/ prefix forces Bedrock.
        let (prov, model) = resolved(
            "bedrock/us.anthropic.claude-sonnet-4-6",
            &Provider::OpenRouter,
        );
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");

        // openrouter/ prefix forces OpenRouter.
        let (prov, model) = resolved(
            "openrouter/openai/gpt-5.4-mini-20260317",
            &Provider::Bedrock,
        );
        assert_eq!(prov, Provider::OpenRouter);
        assert_eq!(model, "openai/gpt-5.4-mini-20260317");

        // Bare id whose shape matches the default keeps the default.
        let (prov, model) = resolved("us.anthropic.claude-sonnet-4-6", &Provider::Bedrock);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");

        // fireworks/ prefix forces Fireworks and strips only the routing
        // prefix (the provider-native accounts/fireworks/models/* id remains).
        let (prov, model) = resolved(
            "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct",
            &Provider::Bedrock,
        );
        assert_eq!(prov, Provider::Fireworks);
        assert_eq!(model, "accounts/fireworks/models/llama-v3p1-70b-instruct");
    }

    /// #6114 regression: the id from the #6093 mitigation run.
    ///
    /// Why: `anthropic/claude-opus-4.8` carries no routing prefix, so it used to
    /// resolve to whatever the path's default provider was — Bedrock, which
    /// rejected the id, after which the run that completed used Bedrock's own
    /// default model. The slug shape names OpenRouter; nothing else does.
    /// What: asserts the pair resolves to OpenRouter with the id unchanged.
    /// Test: this test itself.
    #[test]
    fn bare_openrouter_slug_routes_to_openrouter_not_the_default() {
        let (prov, model) = resolved("anthropic/claude-opus-4.8", &Provider::Bedrock);
        assert_eq!(
            prov,
            Provider::OpenRouter,
            "an unprefixed OpenRouter slug must not inherit the Bedrock default (#6114)"
        );
        assert_eq!(model, "anthropic/claude-opus-4.8");

        let (prov, _) = resolved("openai/gpt-5.4-mini-20260317", &Provider::Bedrock);
        assert_eq!(prov, Provider::OpenRouter);
    }

    /// #6114: the mirror case — a Bedrock inference profile under an OpenRouter
    /// default resolves to Bedrock rather than being sent to OpenRouter.
    #[test]
    fn bare_bedrock_profile_id_routes_to_bedrock_not_the_default() {
        let (prov, model) = resolved("us.anthropic.claude-sonnet-4-6", &Provider::OpenRouter);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");

        // The un-profiled dotted spelling is Bedrock's too. It fails the
        // inference-profile check later, which is the right error to get.
        let (prov, _) = resolved("anthropic.claude-sonnet-4-6", &Provider::OpenRouter);
        assert_eq!(prov, Provider::Bedrock);
    }

    /// #6114: an explicit prefix that contradicts the id's shape is an error
    /// naming both, never a silent run on the prefixed provider's default.
    #[test]
    fn prefix_that_contradicts_the_id_shape_is_an_error() {
        let err =
            resolve_provider_and_model("bedrock/anthropic/claude-opus-4.8", &Provider::OpenRouter)
                .expect_err("a bedrock/ prefix on an OpenRouter slug must not resolve");
        let msg = err.to_string();
        assert!(
            msg.contains("anthropic/claude-opus-4.8"),
            "the error must name the requested id: {msg}"
        );
        assert!(
            msg.contains("bedrock") && msg.contains("openrouter"),
            "the error must name both providers: {msg}"
        );
    }

    /// #6114: the refusal is symmetric across all three backends, so no pairing
    /// is left with a silent substitution.
    #[test]
    fn every_contradicting_pair_is_refused() {
        // A Bedrock profile id pinned to Fireworks by prefix cannot run.
        let err = resolve_provider_and_model(
            "fireworks/us.anthropic.claude-sonnet-4-6",
            &Provider::OpenRouter,
        )
        .expect_err("a bedrock profile id must not be sent to Fireworks");
        let msg = err.to_string();
        assert!(
            msg.contains("us.anthropic.claude-sonnet-4-6") && msg.contains("fireworks"),
            "the error must name the id and the provider: {msg}"
        );

        // A fireworks-native id pinned to OpenRouter by prefix cannot run.
        resolve_provider_and_model(
            "openrouter/accounts/fireworks/models/llama-v3p1-70b-instruct",
            &Provider::Bedrock,
        )
        .expect_err("a fireworks-native id must not be sent to OpenRouter");

        // Unprefixed, that same id resolves to Fireworks on its shape alone.
        let (prov, _) = resolved(
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
            &Provider::OpenRouter,
        );
        assert_eq!(prov, Provider::Fireworks);
    }

    /// #6114: an id whose shape names no provider still uses the default —
    /// inference narrows the fall-through, it does not remove it.
    #[test]
    fn ambiguous_bare_id_still_uses_the_default_provider() {
        for default in [Provider::Bedrock, Provider::OpenRouter, Provider::Fireworks] {
            let (prov, model) = resolved("claude-opus-4-5-20260101", &default);
            assert_eq!(prov, default, "an ambiguous id must keep the default");
            assert_eq!(model, "claude-opus-4-5-20260101");
        }
    }

    #[test]
    fn provider_factory_empty_bedrock_prefix_uses_bare_empty_string() {
        // Edge case: "bedrock/" with nothing after the slash.
        let (prov, model) = resolved("bedrock/", &Provider::OpenRouter);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "");
    }

    #[test]
    fn provider_factory_mixed_providers_in_compare_set() {
        // Simulate the mixed-provider compare set.
        let candidates = [
            "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "bedrock/us.anthropic.claude-sonnet-4-6",
            "openrouter/openai/gpt-5.4-mini-20260317",
            "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct",
        ];
        let expected = [
            (
                Provider::Bedrock,
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            ),
            (Provider::Bedrock, "us.anthropic.claude-sonnet-4-6"),
            (Provider::OpenRouter, "openai/gpt-5.4-mini-20260317"),
            (
                Provider::Fireworks,
                "accounts/fireworks/models/llama-v3p1-70b-instruct",
            ),
        ];
        for (candidate, (exp_prov, exp_model)) in candidates.iter().zip(expected.iter()) {
            let (prov, model) = resolved(candidate, &Provider::Bedrock);
            assert_eq!(prov, *exp_prov, "provider mismatch for {candidate}");
            assert_eq!(model, *exp_model, "model mismatch for {candidate}");
        }
    }

    // ── strip_provider_prefix regression tests ────────────────────────────

    /// Regression test: `bedrock/<id>` must strip to bare `<id>`.
    ///
    /// Why: guards against the Bug 1 regression where a prefixed model id
    /// reaches the Bedrock Converse API as the model parameter, causing HTTP 400.
    /// What: asserts `strip_provider_prefix("bedrock/X") == "X"` for real ids.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_bedrock() {
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-sonnet-4-6"),
            "us.anthropic.claude-sonnet-4-6",
            "bedrock/ prefix must be stripped"
        );
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "bedrock/ prefix must be stripped from date-versioned haiku id"
        );
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-opus-4-8"),
            "us.anthropic.claude-opus-4-8",
            "bedrock/ prefix must be stripped from opus id"
        );
    }

    /// Regression test: `openrouter/<id>` must strip to bare `<id>`.
    ///
    /// Why: same Bug 1 pattern applies to OpenRouter provider routing prefix.
    /// What: asserts `strip_provider_prefix("openrouter/X") == "X"`.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_openrouter() {
        assert_eq!(
            strip_provider_prefix("openrouter/openai/gpt-5.4-mini-20260317"),
            "openai/gpt-5.4-mini-20260317",
            "openrouter/ prefix must be stripped"
        );
    }

    /// Regression test: `fireworks/<id>` must strip to bare `<id>`.
    ///
    /// Why: same Bug 1 pattern — the `fireworks/` routing prefix is not part
    /// of the provider-native `accounts/fireworks/models/*` id and must never
    /// reach the wire.
    /// What: asserts `strip_provider_prefix("fireworks/X") == "X"`.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_fireworks() {
        assert_eq!(
            strip_provider_prefix("fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct"),
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
            "fireworks/ routing prefix must be stripped, leaving the provider-native id"
        );
    }

    /// Regression test: bare ids (no prefix) must be returned unchanged.
    ///
    /// Why: operators may pass bare ids when the provider is set separately;
    /// stripping must not mangle them.
    /// What: asserts `strip_provider_prefix("us.X") == "us.X"`.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_bare() {
        assert_eq!(
            strip_provider_prefix("us.anthropic.claude-sonnet-4-6"),
            "us.anthropic.claude-sonnet-4-6",
            "bare id must be returned unchanged"
        );
        assert_eq!(
            strip_provider_prefix("openai/gpt-5.4-mini-20260317"),
            "openai/gpt-5.4-mini-20260317",
            "bare OpenRouter id must not be stripped"
        );
    }
}
