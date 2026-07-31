//! [`InferenceAdapter`] — the object-safe async inference surface.
//!
//! Why: the whole epic exists to replace six bespoke LLM clients with ONE trait
//! every consumer depends on. It unions tcode's `LlmClientTrait` (the async
//! `chat` seam) and `Provider` (name, tool-choice mapping, capability
//! introspection) plus trusty-review's `supports_structured_output`, so a
//! consumer can drive any provider through `Box<dyn InferenceAdapter>` without
//! branching on the backend. Concrete HTTP adapters (OpenRouter/Fireworks/
//! Bedrock) land in #2403/#2407 — this ticket defines only the surface and a
//! test-only implementation.
//! What: [`InferenceAdapter`] — `async fn chat`, `name`, `capabilities`, the
//! model-aware `capabilities_for` (#4425, for adapters that route per request),
//! a `map_tool_choice` hook, and capability-introspection defaults derived from
//! the adapter's [`ProviderCapabilities`]. Object-safe (async via
//! `async_trait`), so it is boxable/`Arc`-able for the configurator.
//! Test: exercised by `test_support::ScriptedAdapter` and the configurator
//! round-trip in `crates/trusty-common/tests/inference_foundation.rs`; the
//! `adapter::tests` module pins the `capabilities_for` default and the
//! `context_window` derivation from it.

use async_trait::async_trait;
use serde_json::Value;

use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, context_window};
use crate::inference::streaming::{ChatStream, buffered_stream};
use crate::inference::types::{ChatRequest, ChatResponse, ToolChoice, openai_tool_choice};

/// The async inference surface every provider implements.
///
/// Why: one seam lets the agent loop, trusty-review, and tga issue a chat call
/// against any provider identically; the concrete HTTP mechanics live in each
/// adapter, not in the caller. `Send + Sync` are required because callers share
/// adapters across `tokio` tasks.
/// What: implementors MUST provide [`Self::name`], [`Self::capabilities`], and
/// [`Self::chat`]. The remaining methods have capability-derived defaults an
/// implementor overrides only when its wire behaviour differs (e.g. an
/// Anthropic-dialect adapter overriding [`Self::map_tool_choice`]). Object-safe:
/// `async_trait` boxes the `chat` future so `Box<dyn InferenceAdapter>` works.
/// Test: `ScriptedAdapter` (test_support) implements it; the configurator drives
/// it as a trait object.
#[async_trait]
pub trait InferenceAdapter: Send + Sync {
    /// Stable provider name for logging and diagnostics.
    ///
    /// Why: telemetry and error messages need a human-readable backend label.
    /// What: a short identifier such as `"openrouter"` or `"scripted"`.
    /// Test: configurator round-trip asserts the built adapter's name.
    fn name(&self) -> &str;

    /// The capability descriptor for this adapter's provider.
    ///
    /// Why: the introspection defaults below read from it, and consumers query
    /// it directly to decide per-model behaviour before issuing a call.
    /// What: returns a borrowed [`ProviderCapabilities`] (typically a `&'static`
    /// from [`crate::inference::registry::capabilities`], but an adapter may own
    /// a customised copy).
    /// Test: `supports_*` defaults are derived from it.
    fn capabilities(&self) -> &ProviderCapabilities;

    /// The capability descriptor for the provider that would actually serve
    /// `model`.
    ///
    /// Why (#4425): [`Self::capabilities`] assumes one adapter serves one
    /// provider, but a ROUTING adapter picks its backend per request from the
    /// model slug (trusty-code's `OpenAiCompatClient` spans OpenRouter /
    /// Fireworks / Together / AtlasCloud; its `DispatchingLlmClient` adds
    /// Bedrock). Answering such an adapter's capability question with one fixed
    /// provider's profile is silently wrong for every other backend it serves —
    /// e.g. reporting OpenRouter's `detailed_usage_accounting: true` for a
    /// Fireworks turn, which no other provider accepts on the wire. This is the
    /// model-aware form callers should prefer whenever a slug is in hand;
    /// [`Self::context_window`] already established that shape.
    /// What: defaults to [`Self::capabilities`] — correct for every
    /// single-provider adapter, which is most of them. A routing adapter
    /// overrides it to resolve `model` through the same routing gate its
    /// `chat`/`chat_stream` use, so capabilities can never disagree with where
    /// the request is actually sent.
    /// Test: `adapter_capabilities_for_defaults_to_capabilities`;
    /// trusty-code's `client::tests::capabilities_for_follows_slug_routing` and
    /// `dispatch::tests::capabilities_for_follows_slug_routing`.
    fn capabilities_for(&self, model: &str) -> &ProviderCapabilities {
        let _ = model;
        self.capabilities()
    }

    /// Issue one chat/completions call.
    ///
    /// Why: the single method the whole agent loop depends on; a one-call surface
    /// keeps test doubles trivial and the caller's dependency minimal.
    /// What: sends `request` to the provider and yields a [`ChatResponse`]
    /// (which carries the normalized [`crate::inference::Usage`]) or an
    /// [`InferenceError`] the caller classifies via
    /// [`InferenceError::is_retryable`]/[`InferenceError::is_alarm`].
    /// Test: `ScriptedAdapter::chat` returns queued scripted responses.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError>;

    /// Issue one chat/completions call as an incremental token stream.
    ///
    /// Why: consumers that render tokens as they arrive (the trusty-agents chat
    /// UI first) need deltas incrementally, not one buffered [`ChatResponse`].
    /// Making this a trait method with a working default means every adapter is
    /// stream-callable immediately: OpenAI-dialect providers override it with a
    /// real SSE transport (#3696 Gap B), while adapters without native streaming
    /// keep the default. Returning `Result<ChatStream, _>` (rather than a stream
    /// whose first item is the error) lets a `stream=true` handshake failure
    /// surface synchronously so the caller can choose to retry non-streaming —
    /// the adapter never silently degrades to buffered on its own.
    /// What: yields ordered [`crate::inference::ChatStreamEvent`]s — text/tool
    /// deltas followed by exactly one terminal `Done` carrying the finish reason
    /// and usage. The default buffers via [`Self::chat`] and replays the finished
    /// response through [`buffered_stream`]; dropping the returned stream cancels
    /// the underlying request in a real streaming override.
    /// Test: `crates/trusty-common/tests/inference_adapters.rs` streaming
    /// round-trip; `super::streaming::tests::buffered_stream_replays_response`
    /// covers the default's replay shape.
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream, InferenceError> {
        let response = self.chat(request).await?;
        Ok(buffered_stream(response))
    }

    /// Translate a neutral [`ToolChoice`] into this provider's wire JSON.
    ///
    /// Why: OpenAI-dialect and Anthropic-dialect providers spell tool-choice
    /// differently; centralising the mapping keeps the caller dialect-agnostic.
    /// What: defaults to the OpenAI spelling ([`openai_tool_choice`]); an
    /// Anthropic-dialect adapter overrides this.
    /// Test: default asserted via the OpenAI mapper's own tests.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        openai_tool_choice(choice)
    }

    /// Whether the provider supports native function-calling.
    ///
    /// Why: models without native tool support need prompt-emulated guidance
    /// instead of a `tools` array; this flag drives that selection.
    /// What: defaults to `capabilities().native_tool_calling`.
    /// Test: registry seed assertions.
    fn supports_native_tools(&self) -> bool {
        self.capabilities().native_tool_calling
    }

    /// Whether the provider honours Anthropic-style prompt-cache breakpoints.
    ///
    /// Why: gating the `cache_control` marker to caching-capable providers keeps
    /// the wire payload minimal for models that can never benefit.
    /// What: defaults to `capabilities().prompt_caching`.
    /// Test: registry seed assertions.
    fn supports_prompt_caching(&self) -> bool {
        self.capabilities().prompt_caching
    }

    /// Whether the provider supports structured / JSON-schema output.
    ///
    /// Why: the capability trusty-review needs to request schema-constrained
    /// responses; folded into the trait so every consumer shares one answer.
    /// What: defaults to `capabilities().structured_output`.
    /// Test: registry seed assertions.
    fn supports_structured_output(&self) -> bool {
        self.capabilities().structured_output
    }

    /// Whether to request detailed usage accounting (OpenRouter's
    /// `usage: {"include": true}` directive).
    ///
    /// Why: only OpenRouter returns its authoritative cache-aware cost when asked;
    /// other providers must never see the directive.
    /// What: defaults to `capabilities().detailed_usage_accounting`.
    /// Test: registry seed assertions.
    fn wants_detailed_usage(&self) -> bool {
        self.capabilities().detailed_usage_accounting
    }

    /// The context window (in tokens) for a model slug served by this provider.
    ///
    /// Why: compaction/budget code asks the adapter for the real window rather
    /// than hard-coding one; the answer is model-specific (incl. the #2330 haiku
    /// fix) with the provider default as a fallback.
    /// What: defaults to [`context_window`] with the capabilities of the
    /// provider that would actually serve `model` ([`Self::capabilities_for`])
    /// as the provider-default tier — #4425: on a routing adapter this is the
    /// only way the fallback tier can match the backend the slug reaches.
    /// Test: `context` submodule tests (`haiku_resolves_to_200k`);
    /// `context_window_default_follows_capabilities_for`.
    fn context_window(&self, model: &str) -> usize {
        context_window(model, Some(self.capabilities_for(model)))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::registry::{ProviderId, capabilities};

    /// A single-provider adapter: never overrides [`InferenceAdapter::capabilities_for`].
    struct SingleProvider;

    #[async_trait]
    impl InferenceAdapter for SingleProvider {
        fn name(&self) -> &str {
            "single"
        }

        fn capabilities(&self) -> &ProviderCapabilities {
            capabilities(ProviderId::OpenRouter)
        }

        async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
            Err(InferenceError::Unsupported("test double".into()))
        }
    }

    /// A ROUTING adapter, in miniature: the shape trusty-code's
    /// `OpenAiCompatClient`/`DispatchingLlmClient` have — one adapter, a backend
    /// chosen per request from the model slug.
    struct RoutingAdapter;

    #[async_trait]
    impl InferenceAdapter for RoutingAdapter {
        fn name(&self) -> &str {
            "routing"
        }

        fn capabilities(&self) -> &ProviderCapabilities {
            capabilities(ProviderId::OpenRouter)
        }

        fn capabilities_for(&self, model: &str) -> &ProviderCapabilities {
            if model.starts_with("fireworks/") {
                capabilities(ProviderId::Fireworks)
            } else {
                capabilities(ProviderId::OpenRouter)
            }
        }

        async fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
            Err(InferenceError::Unsupported("test double".into()))
        }
    }

    /// The `capabilities_for` default is `capabilities()` for any slug (#4425).
    ///
    /// Why: adding a model-aware capability accessor must not change what a
    /// single-provider adapter — most implementors, and every one that existed
    /// before #4425 — answers. The default has to be a pure widening.
    /// What: assert both an arbitrary and an unrelated-provider slug return the
    /// adapter's own profile.
    /// Test: this test.
    #[test]
    fn adapter_capabilities_for_defaults_to_capabilities() {
        let adapter = SingleProvider;
        for slug in ["openai/gpt-4o-mini", "fireworks/accounts/x/models/y"] {
            assert_eq!(
                adapter.capabilities_for(slug).id,
                adapter.capabilities().id,
                "slug {slug} must fall back to the adapter's own capabilities"
            );
        }
    }

    /// A routing adapter's `capabilities_for` follows the slug, and
    /// `context_window`'s default tier follows it too (#4425).
    ///
    /// Why: this is the regression guard for the defect that made a multi-
    /// provider adapter answer capability questions for ONE hard-wired
    /// provider. `context_window` is the observable consequence: with the
    /// default deriving from `capabilities()` a `fireworks/*` slug the substring
    /// table does not recognise would report OpenRouter's 200K tier instead of
    /// Fireworks' 128K one.
    /// What: assert the Fireworks slug resolves to the Fireworks profile and to
    /// its 128K default tier, while an OpenRouter slug keeps 200K.
    /// Test: this test.
    #[test]
    fn context_window_default_follows_capabilities_for() {
        let adapter = RoutingAdapter;
        let fireworks_slug = "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct";
        assert_eq!(
            adapter.capabilities_for(fireworks_slug).id,
            ProviderId::Fireworks
        );
        assert_eq!(adapter.context_window(fireworks_slug), 128_000);
        assert_eq!(
            adapter
                .capabilities_for("qwen/qwen-2.5-coder-32b-instruct")
                .id,
            ProviderId::OpenRouter
        );
        assert_eq!(
            adapter.context_window("qwen/qwen-2.5-coder-32b-instruct"),
            200_000
        );
    }
}
