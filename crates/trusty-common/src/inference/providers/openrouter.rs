//! OpenRouter adapter — a thin config over the OpenAI-compatible core.
//!
//! Why: OpenRouter fronts dozens of model families behind one OpenAI-compatible
//! `/chat/completions` schema and is the epic's default backend. Everything it
//! needs beyond the shared core is a base URL, `Bearer` auth (handled by the
//! core), attribution headers, and the `usage:{include:true}` detailed-usage
//! flag — the last of which is already driven by the capability registry's
//! `detailed_usage_accounting = true` for OpenRouter, so the core injects it
//! automatically. Anthropic-style `cache_control` breakpoints set by the caller
//! on `ChatMessage`/`FunctionDefinition` pass through unchanged (the core clones
//! and serialises the request verbatim).
//! What: [`build`] constructs an [`OpenAiCompatAdapter`] for a resolved
//! OpenRouter credential against a given base URL; [`factory`] is the
//! production factory (real base URL) registered into the [`Configurator`].
//! Test: inline `#[ignore]` `live_openrouter_call`; offline round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

use super::openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};
use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderId;

/// OpenRouter API root; the core appends `/chat/completions`.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// `HTTP-Referer` attribution header value (mirrors tcode's live client).
const OPENROUTER_REFERER: &str = "https://github.com/bobmatnyc/trusty-tools";

/// `X-Title` attribution header value shown in OpenRouter dashboards.
const OPENROUTER_TITLE: &str = "trusty-tools";

/// Build an OpenRouter adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests can point the
/// exact same adapter at [`crate::inference::test_support::MockInferenceServer`]
/// while production uses [`OPENROUTER_BASE_URL`].
/// What: requires the resolved key (OpenRouter is a keyed provider — a missing
/// key is [`InferenceError::MissingCredential`], never a silent anonymous call),
/// then constructs an [`OpenAiCompatAdapter`] with OpenRouter's attribution
/// headers and its registry capabilities (which carry `detailed_usage_accounting
/// = true`, so the core auto-injects `usage:{include:true}`).
/// Test: `crates/trusty-common/tests/inference_adapters.rs`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::OpenRouter,
        })?
        .clone();
    let config = OpenAiCompatConfig {
        name: ProviderId::OpenRouter.as_str().to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        extra_headers: vec![
            ("HTTP-Referer".to_string(), OPENROUTER_REFERER.to_string()),
            ("X-Title".to_string(), OPENROUTER_TITLE.to_string()),
        ],
        capabilities: *resolved.capabilities(),
    };
    Ok(Box::new(OpenAiCompatAdapter::new(config)?))
}

/// Production factory: build an OpenRouter adapter against the real base URL.
///
/// Why: this is what [`super::register_default_factories`] registers into the
/// [`crate::inference::Configurator`] so `build("<slug>", &store)` yields a live
/// OpenRouter adapter.
/// What: delegates to [`build`] with [`OPENROUTER_BASE_URL`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via a mock-URL
/// factory) and the `#[ignore]` live smoke test below.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, OPENROUTER_BASE_URL)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::registry::capabilities;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    /// Construct a `ResolvedProvider` with a fake OpenRouter key, without going
    /// through the env-sensitive resolver.
    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::OpenRouter,
            "openai/gpt-4o-mini".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named OpenRouter adapter whose capabilities
    /// request detailed usage accounting.
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("sk-or-test"), OPENROUTER_BASE_URL) // pragma: allowlist secret
            .expect("built");
        assert_eq!(adapter.name(), "openrouter");
        assert!(adapter.wants_detailed_usage());
        assert_eq!(adapter.capabilities().id, ProviderId::OpenRouter);
    }

    /// Why: a resolved provider with no key must be an explicit alarm, never a
    /// silent anonymous adapter.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved = ResolvedProvider::new(
            ProviderId::OpenRouter,
            "openai/gpt-4o-mini".to_string(),
            None,
        );
        let Err(err) = build(&resolved, OPENROUTER_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::OpenRouter
            }
        ));
    }

    /// Live smoke test: send a trivial prompt to a cheap OpenRouter model.
    ///
    /// Why: end-to-end validation against the real API that the OpenRouter
    /// config + core produce a non-empty response and non-zero usage. Ignored so
    /// CI stays offline; run locally with a real key.
    /// What: reads `OPENROUTER_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        openrouter -- --ignored --nocapture` (with `OPENROUTER_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY; skipped in CI"]
    async fn live_openrouter_call() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("OPENROUTER_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("OPENROUTER_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::OpenRouter,
            "openai/gpt-4o-mini".to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, OPENROUTER_BASE_URL).expect("build adapter");
        let _ = capabilities(ProviderId::OpenRouter); // ensure registry link

        let mut req = ChatRequest::new(
            "openai/gpt-4o-mini",
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
            "live openrouter ok — text: {text:?}, usage: {:?}",
            resp.usage()
        );
    }
}
