//! Together.ai adapter — a thin config over the OpenAI-compatible core (#2488).
//!
//! Why: Together.ai serves its catalog (`meta-llama/*`, `Qwen/*`,
//! `deepseek-ai/*`, …) behind the same OpenAI-compatible `/chat/completions`
//! schema, so it reuses the shared core wholesale. Its only deltas from
//! OpenRouter are the base URL (`api.together.xyz/v1`), no attribution headers,
//! and — per the capability registry seed — `detailed_usage_accounting = false`
//! (the `usage:{include:true}` directive is OpenRouter-specific). Together's
//! prompt caching is AUTOMATIC/implicit (no explicit `cache_control`
//! breakpoints), modeled exactly like OpenAI-direct in the registry. Native
//! OpenAI-style tool-calling stays on.
//! What: [`build`] constructs an [`OpenAiCompatAdapter`] for a resolved Together
//! credential against a given base URL; [`factory`] is the production factory
//! (real base URL) registered into the [`crate::inference::Configurator`].
//! Test: inline `#[ignore]` `live_together_call`; offline round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// Together.ai API root; the core appends `/chat/completions`.
pub const TOGETHER_BASE_URL: &str = "https://api.together.xyz/v1";

/// Build a Together adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests can point the
/// exact same adapter at [`crate::inference::test_support::MockInferenceServer`]
/// while production uses [`TOGETHER_BASE_URL`].
/// What: requires the resolved key (Together is a keyed provider — a missing key
/// is [`InferenceError::MissingCredential`]), then constructs an
/// [`OpenAiCompatAdapter`] with no attribution headers and Together's registry
/// capabilities (`detailed_usage_accounting = false`), so the core sends a bare
/// OpenAI-compatible body.
/// Test: `crates/trusty-common/tests/inference_adapters.rs`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::Together,
        })?
        .clone();
    let config = OpenAiCompatConfig {
        name: ProviderId::Together.as_str().to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        extra_headers: Vec::new(),
        capabilities: *resolved.capabilities(),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(config)?))
}

/// Production factory: build a Together adapter against the real base URL.
///
/// Why: this is what [`super::register_default_factories`] registers into the
/// [`crate::inference::Configurator`] so a `together/*` slug (whose key
/// resolves) yields a live Together adapter.
/// What: delegates to [`build`] with [`TOGETHER_BASE_URL`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via a mock-URL
/// factory) and the `#[ignore]` live smoke test below.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, TOGETHER_BASE_URL)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    const TOGETHER_MODEL: &str = "meta-llama/Llama-3.3-70B-Instruct-Turbo";

    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::Together,
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named Together adapter that does NOT request
    /// detailed usage accounting and advertises native OpenAI-style tools.
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("tgp_v1_test"), TOGETHER_BASE_URL).expect("built");
        assert_eq!(adapter.name(), "together");
        assert!(!adapter.wants_detailed_usage());
        assert!(adapter.supports_native_tools());
        assert_eq!(adapter.capabilities().id, ProviderId::Together);
    }

    /// Why: a resolved provider with no key must be an explicit alarm.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved = ResolvedProvider::new(
            ProviderId::Together,
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo".to_string(),
            None,
        );
        let Err(err) = build(&resolved, TOGETHER_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::Together
            }
        ));
    }

    /// Live smoke test: send a trivial prompt to a cheap Together model.
    ///
    /// Why: end-to-end validation against the real API that the Together config
    /// + core produce a non-empty response and non-zero usage. Ignored so CI
    /// stays offline; run locally with a real key.
    /// What: reads `TOGETHER_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        together -- --ignored --nocapture` (with `TOGETHER_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires TOGETHER_API_KEY; skipped in CI"]
    async fn live_together_call() {
        let Ok(key) = std::env::var("TOGETHER_API_KEY") else {
            eprintln!("TOGETHER_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("TOGETHER_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::Together,
            TOGETHER_MODEL.to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, TOGETHER_BASE_URL).expect("build adapter");

        let mut req = ChatRequest::new(
            TOGETHER_MODEL,
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
            "live together ok — text: {text:?}, usage: {:?}",
            resp.usage()
        );
    }
}
