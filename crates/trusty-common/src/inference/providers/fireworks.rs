//! Fireworks adapter — a thin config over the OpenAI-compatible core.
//!
//! Why: Fireworks AI serves `accounts/fireworks/models/*` slugs behind the same
//! OpenAI-compatible `/chat/completions` schema, so it reuses the shared core
//! wholesale. Its only deltas from OpenRouter are the base URL
//! (`api.fireworks.ai/inference/v1`), no attribution headers, and — per the
//! acceptance criteria — `wants_detailed_usage = false` with no prompt caching
//! (both already encoded in the capability registry seed, so the core neither
//! injects the usage directive nor advertises caching). Native tool-calling
//! stays on the registry's conservative allow-list posture.
//! What: [`build`] constructs an [`OpenAiCompatAdapter`] for a resolved Fireworks
//! credential against a given base URL; [`factory`] is the production factory
//! (real base URL) registered into the [`Configurator`].
//! Test: inline `#[ignore]` `live_fireworks_call`; offline round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// Fireworks API root; the core appends `/chat/completions`.
pub const FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";

/// Build a Fireworks adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests can point the
/// exact same adapter at [`crate::inference::test_support::MockInferenceServer`]
/// while production uses [`FIREWORKS_BASE_URL`].
/// What: requires the resolved key (Fireworks is a keyed provider — a missing
/// key is [`InferenceError::MissingCredential`]), then constructs an
/// [`OpenAiCompatAdapter`] with no attribution headers and Fireworks' registry
/// capabilities (`detailed_usage_accounting = false`, `prompt_caching = false`),
/// so the core sends a bare OpenAI-compatible body.
/// Test: `crates/trusty-common/tests/inference_adapters.rs`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::Fireworks,
        })?
        .clone();
    let config = OpenAiCompatConfig {
        name: ProviderId::Fireworks.as_str().to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        extra_headers: Vec::new(),
        capabilities: *resolved.capabilities(),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(config)?))
}

/// Production factory: build a Fireworks adapter against the real base URL.
///
/// Why: this is what [`super::register_default_factories`] registers into the
/// [`crate::inference::Configurator`] so a `fireworks/*` slug (whose key
/// resolves) yields a live Fireworks adapter.
/// What: delegates to [`build`] with [`FIREWORKS_BASE_URL`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via a mock-URL
/// factory) and the `#[ignore]` live smoke test below.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, FIREWORKS_BASE_URL)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    const FIREWORKS_MODEL: &str = "accounts/fireworks/models/llama-v3p1-8b-instruct";

    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::Fireworks,
            "fireworks/llama-v3p1-8b-instruct".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named Fireworks adapter that does NOT
    /// request detailed usage and does NOT advertise prompt caching.
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("fw-test"), FIREWORKS_BASE_URL).expect("built");
        assert_eq!(adapter.name(), "fireworks");
        assert!(!adapter.wants_detailed_usage());
        assert!(!adapter.supports_prompt_caching());
        assert!(adapter.supports_native_tools());
    }

    /// Why: a resolved provider with no key must be an explicit alarm.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved =
            ResolvedProvider::new(ProviderId::Fireworks, "fireworks/llama".to_string(), None);
        let Err(err) = build(&resolved, FIREWORKS_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::Fireworks
            }
        ));
    }

    /// Live smoke test: send a trivial prompt to a cheap Fireworks model.
    ///
    /// Why: end-to-end validation against the real API that the Fireworks config
    /// + core produce a non-empty response and non-zero usage. Ignored so CI
    /// stays offline; run locally with a real key.
    /// What: reads `FIREWORKS_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        fireworks -- --ignored --nocapture` (with `FIREWORKS_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires FIREWORKS_API_KEY; skipped in CI"]
    async fn live_fireworks_call() {
        let Ok(key) = std::env::var("FIREWORKS_API_KEY") else {
            eprintln!("FIREWORKS_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("FIREWORKS_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::Fireworks,
            FIREWORKS_MODEL.to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, FIREWORKS_BASE_URL).expect("build adapter");

        let mut req = ChatRequest::new(
            FIREWORKS_MODEL,
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
            "live fireworks ok — text: {text:?}, usage: {:?}",
            resp.usage()
        );
    }
}
