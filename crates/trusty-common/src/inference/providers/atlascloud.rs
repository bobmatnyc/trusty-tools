//! AtlasCloud adapter — a thin config over the OpenAI-compatible core (#2536).
//!
//! Why: AtlasCloud (atlascloud.ai) serves its catalog behind the same
//! OpenAI-compatible `/chat/completions` schema, so it reuses the shared core
//! wholesale. Its only deltas from OpenRouter are the base URL
//! (`api.atlascloud.ai/v1`), no attribution headers, and — per the capability
//! registry seed — `detailed_usage_accounting = false` (the `usage:{include:true}`
//! directive is OpenRouter-specific). Native OpenAI-style tool-calling stays on.
//! Modeled exactly like the Together adapter (#2488).
//! What: [`build`] constructs an [`OpenAiCompatAdapter`] for a resolved AtlasCloud
//! credential against a given base URL; [`factory`] is the production factory
//! (real base URL) registered into the [`crate::inference::Configurator`].
//! Test: inline `#[ignore]` `live_atlascloud_call`; offline unit tests below.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// AtlasCloud API root; the core appends `/chat/completions`.
pub const ATLASCLOUD_BASE_URL: &str = "https://api.atlascloud.ai/v1";

/// Build an AtlasCloud adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests can point the
/// exact same adapter at [`crate::inference::test_support::MockInferenceServer`]
/// while production uses [`ATLASCLOUD_BASE_URL`].
/// What: requires the resolved key (AtlasCloud is a keyed provider — a missing
/// key is [`InferenceError::MissingCredential`]), then constructs an
/// [`OpenAiCompatAdapter`] with no attribution headers and AtlasCloud's registry
/// capabilities (`detailed_usage_accounting = false`), so the core sends a bare
/// OpenAI-compatible body.
/// Test: `factory_builds_named_adapter`, `missing_key_errors`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::AtlasCloud,
        })?
        .clone();
    let config = OpenAiCompatConfig {
        name: ProviderId::AtlasCloud.as_str().to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        extra_headers: Vec::new(),
        capabilities: *resolved.capabilities(),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(config)?))
}

/// Production factory: build an AtlasCloud adapter against the real base URL.
///
/// Why: this is what [`super::register_default_factories`] registers into the
/// [`crate::inference::Configurator`] so an `atlascloud/*` slug (whose key
/// resolves) yields a live AtlasCloud adapter.
/// What: delegates to [`build`] with [`ATLASCLOUD_BASE_URL`].
/// Test: the offline default-factory round-trip in
/// `crates/trusty-common/tests/inference_adapters.rs` and the `#[ignore]` live
/// smoke test below.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, ATLASCLOUD_BASE_URL)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    const ATLASCLOUD_MODEL: &str = "openai/gpt-5.6-sol";

    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::AtlasCloud,
            "atlascloud/openai/gpt-5.6-sol".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named AtlasCloud adapter that does NOT
    /// request detailed usage accounting and advertises native OpenAI-style tools.
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("ac_test_key"), ATLASCLOUD_BASE_URL).expect("built");
        assert_eq!(adapter.name(), "atlascloud");
        assert!(!adapter.wants_detailed_usage());
        assert!(adapter.supports_native_tools());
        assert_eq!(adapter.capabilities().id, ProviderId::AtlasCloud);
    }

    /// Why: a resolved provider with no key must be an explicit alarm.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved = ResolvedProvider::new(
            ProviderId::AtlasCloud,
            "atlascloud/openai/gpt-5.6-sol".to_string(),
            None,
        );
        let Err(err) = build(&resolved, ATLASCLOUD_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::AtlasCloud
            }
        ));
    }

    /// Live smoke test: send a trivial prompt to AtlasCloud's default model.
    ///
    /// Why: end-to-end validation against the real API that the AtlasCloud config
    /// + core produce a non-empty response and non-zero usage. Ignored so CI
    /// stays offline; run locally with a real key.
    /// What: reads `ATLASCLOUD_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        atlascloud -- --ignored --nocapture` (with `ATLASCLOUD_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires ATLASCLOUD_API_KEY; skipped in CI"]
    async fn live_atlascloud_call() {
        let Ok(key) = std::env::var("ATLASCLOUD_API_KEY") else {
            eprintln!("ATLASCLOUD_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("ATLASCLOUD_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::AtlasCloud,
            ATLASCLOUD_MODEL.to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, ATLASCLOUD_BASE_URL).expect("build adapter");

        let mut req = ChatRequest::new(
            ATLASCLOUD_MODEL,
            vec![
                ChatMessage::system("You are a concise assistant."),
                ChatMessage::user("Reply with exactly the word: pong"),
            ],
        );
        req.temperature = Some(0.0);
        req.max_tokens = Some(16);

        let resp = adapter.chat(&req).await.expect("live chat");
        let text = resp.first_text().expect("assistant text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            resp.usage().prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!(
            "live atlascloud ok — text: {text:?}, usage: {:?}",
            resp.usage()
        );
    }
}
