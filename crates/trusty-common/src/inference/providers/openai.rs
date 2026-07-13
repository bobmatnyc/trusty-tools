//! OpenAI-direct adapter — the reference thin config over the shared core.
//!
//! Why: OpenAI's first-party `/chat/completions` IS the schema the core speaks,
//! so the OpenAI adapter is the minimal possible config: base URL + `Bearer`
//! auth (from the core), no attribution headers, no detailed-usage directive
//! (`detailed_usage_accounting = false` in the registry). It exists so the
//! two-stage resolver's `openai/*` explicit-family path (when `OPENAI_API_KEY`
//! resolves) builds a real first-party adapter rather than routing through
//! OpenRouter.
//! What: [`build`] constructs an [`OpenAiCompatAdapter`] for a resolved OpenAI
//! credential against a given base URL; [`factory`] is the production factory
//! (real base URL) registered into the [`Configurator`].
//! Test: inline `tests`; offline round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// OpenAI API root; the core appends `/chat/completions`.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Build an OpenAI-direct adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests can point the
/// exact same adapter at [`crate::inference::test_support::MockInferenceServer`]
/// while production uses [`OPENAI_BASE_URL`].
/// What: requires the resolved key (OpenAI is a keyed provider — a missing key
/// is [`InferenceError::MissingCredential`]), then constructs an
/// [`OpenAiCompatAdapter`] with no attribution headers and OpenAI's registry
/// capabilities.
/// Test: `crates/trusty-common/tests/inference_adapters.rs`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::OpenAI,
        })?
        .clone();
    let config = OpenAiCompatConfig {
        name: ProviderId::OpenAI.as_str().to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        extra_headers: Vec::new(),
        capabilities: *resolved.capabilities(),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(config)?))
}

/// Production factory: build an OpenAI adapter against the real base URL.
///
/// Why: registered by [`super::register_default_factories`] so an `openai/*`
/// slug (whose key resolves) yields a live first-party OpenAI adapter.
/// What: delegates to [`build`] with [`OPENAI_BASE_URL`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via a mock-URL
/// factory).
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, OPENAI_BASE_URL)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::OpenAI,
            "gpt-4o-mini".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named OpenAI adapter that does NOT request
    /// detailed usage accounting.
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("sk-openai-test"), OPENAI_BASE_URL) // pragma: allowlist secret
            .expect("built");
        assert_eq!(adapter.name(), "openai");
        assert!(!adapter.wants_detailed_usage());
        assert_eq!(adapter.capabilities().id, ProviderId::OpenAI);
    }

    /// Why: a resolved provider with no key must be an explicit alarm.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved = ResolvedProvider::new(ProviderId::OpenAI, "gpt-4o-mini".to_string(), None);
        let Err(err) = build(&resolved, OPENAI_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::OpenAI
            }
        ));
    }

    /// Live smoke test: send a trivial prompt to a cheap OpenAI model.
    ///
    /// Why: end-to-end validation against the real first-party OpenAI API that
    /// the OpenAI-direct config + shared core produce a non-empty response and
    /// non-zero usage. Ignored so CI stays offline; run locally with a real key.
    /// What: reads `OPENAI_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        openai -- --ignored --nocapture` (with `OPENAI_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY; skipped in CI"]
    async fn live_openai_call() {
        let Ok(key) = std::env::var("OPENAI_API_KEY") else {
            eprintln!("OPENAI_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("OPENAI_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::OpenAI,
            "gpt-4o-mini".to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, OPENAI_BASE_URL).expect("build adapter");

        let mut req = ChatRequest::new(
            "gpt-4o-mini",
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
        eprintln!("live openai ok — text: {text:?}, usage: {:?}", resp.usage());
    }
}
