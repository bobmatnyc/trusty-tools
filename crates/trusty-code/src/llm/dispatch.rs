//! Transport dispatch: route each `ChatRequest` to Bedrock or the shared
//! OpenAI-compatible transport (OpenRouter / Fireworks) by model slug
//! (#1021 phase 1; #2406 migration).
//!
//! Why: Every caller of `InferenceAdapter::chat` (`AgentLoop`, `run_task`,
//! `task::executor`) is constructed with exactly ONE `Arc<dyn InferenceAdapter>`
//! shared across every agent in a run, but different agents can be routed to
//! different model slugs (`crate::provider::resolve_model`) — some `bedrock/*`,
//! some `fireworks/*`, most plain OpenRouter. Rather than threading multiple
//! clients through every call site, `DispatchingLlmClient` is itself an
//! `InferenceAdapter` impl that picks the real transport per-request from
//! `req.model`, using the exact same `crate::provider::provider_for` routing
//! `AgentLoop::build_request` already consults for tool-choice/caching/usage
//! decisions — so there is exactly one source of truth for "which backend".
//! What: `bedrock/*` slugs go to a lazily-constructed `BedrockChatClient`
//! (behind a `tokio::sync::OnceCell`, so a pure-OpenRouter/Fireworks run never
//! touches the AWS SDK). Every other slug goes to the shared-adapter-backed
//! `OpenAiCompatClient` (#2406), which itself routes OpenRouter vs Fireworks by
//! prefix and resolves credentials via the shared env > `.env.local` > store
//! chain — so no key is required at construction (#2245); a missing key only
//! surfaces if a slug that needs it is actually dispatched.
//! Test: `dispatch::tests::*` — routing by slug prefix (no network),
//! `bedrock_construction_is_lazy` proves the OpenAI-compat path never
//! initialises the Bedrock cell.

use async_trait::async_trait;
use tokio::sync::OnceCell;

use trusty_common::inference::{
    ChatRequest, ChatResponse, ChatStream, InferenceAdapter, InferenceError, ProviderCapabilities,
    ProviderId, capabilities,
};

use super::bedrock::BedrockChatClient;
use super::client::OpenAiCompatClient;

/// Provider name `crate::provider::provider_for` reports for `bedrock/*`
/// slugs — the single dispatch condition this client checks.
const BEDROCK_PROVIDER_NAME: &str = "bedrock";

/// Routes each chat request to the Bedrock Converse transport or the shared
/// OpenAI-compatible transport (OpenRouter / Fireworks), by model slug.
///
/// Why: this is the seam that makes `bedrock/*` model slugs reach AWS Bedrock
/// while `fireworks/*` and plain OpenRouter slugs reach the shared adapter —
/// keeping every call site depending only on the object-safe `InferenceAdapter`
/// so no signature changes when routing evolves. Production call sites
/// (`main.rs`, `task::mock_llm`) construct this instead of a bare transport.
/// What: `openai_compat` is the shared-adapter-backed transport (built eagerly —
/// it holds no credentials until first use); `bedrock` is a `OnceCell`
/// populated on the first `bedrock/*` request via `BedrockChatClient::from_env`
/// (standard AWS credential chain — no key required at construction). A request
/// that needs a missing credential fails at `chat()` time, never at
/// construction (#2245) — so a pure-Bedrock run never needs `OPENROUTER_API_KEY`
/// and a pure-OpenRouter run never needs AWS credentials.
/// Test: `dispatch::tests::*`.
#[derive(Debug)]
pub struct DispatchingLlmClient {
    openai_compat: OpenAiCompatClient,
    bedrock: OnceCell<BedrockChatClient>,
}

impl DispatchingLlmClient {
    /// Construct the dispatcher.
    ///
    /// Why: the single constructor every entry point (`main.rs`,
    /// `task::mock_llm`) funnels through. Since the shared OpenAI-compat
    /// transport resolves credentials lazily (env > `.env.local` > store) and
    /// the Bedrock cell starts empty, construction touches NO credentials and
    /// cannot fail — a missing key only surfaces when a request that needs it is
    /// dispatched (#2245).
    /// What: builds the `OpenAiCompatClient` with the process-default store and
    /// an empty Bedrock cell.
    /// Test: `dispatch::tests::construction_touches_no_credentials`.
    pub fn new() -> Self {
        Self {
            openai_compat: OpenAiCompatClient::new(),
            bedrock: OnceCell::new(),
        }
    }

    /// Get (or lazily build) the Bedrock Converse transport.
    ///
    /// Why: AWS credential resolution is async and must never run for a request
    /// that doesn't need it; `OnceCell::get_or_try_init` guarantees at most one
    /// build attempt, and a failed attempt can be retried on the next
    /// `bedrock/*` request rather than poisoning the client forever.
    /// What: returns the cached client, or builds one via
    /// `BedrockChatClient::from_env` on first use.
    /// Test: `dispatch::tests::bedrock_construction_is_lazy`.
    async fn bedrock(&self) -> Result<&BedrockChatClient, InferenceError> {
        self.bedrock
            .get_or_try_init(BedrockChatClient::from_env)
            .await
    }

    /// Whether `model` routes to the Bedrock transport.
    ///
    /// Why: single source of truth — reuses `crate::provider::provider_for`, the
    /// exact factory `AgentLoop::build_request` already consults, so routing can
    /// never disagree between "which transport sends this" and "which `Provider`
    /// normalises this".
    /// What: `true` iff `provider_for(model).name() == "bedrock"`.
    /// Test: `dispatch::tests::routes_bedrock_prefix_true`,
    /// `dispatch::tests::routes_non_bedrock_prefix_false`.
    fn routes_to_bedrock(model: &str) -> bool {
        crate::provider::provider_for(model).name() == BEDROCK_PROVIDER_NAME
    }
}

impl Default for DispatchingLlmClient {
    /// Delegates to [`DispatchingLlmClient::new`].
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceAdapter for DispatchingLlmClient {
    /// The dispatcher's own label.
    ///
    /// Why (#4425): [`InferenceAdapter::name`] assumes one adapter serves one
    /// provider, but this type exists precisely because trusty-code shares ONE
    /// client across agents routed to different backends — there is no single
    /// truthful provider name. Reporting `"dispatching"` says exactly that,
    /// rather than naming whichever backend happened to be built first and
    /// misattributing every log line for the other one.
    /// What: the constant `"dispatching"`.
    /// Test: `dispatch::tests::name_is_dispatching`.
    fn name(&self) -> &str {
        "dispatching"
    }

    /// Capabilities for the DEFAULT backend, not for a specific request.
    ///
    /// Why (#4425): this is the one genuine impedance mismatch between
    /// trusty-code and the shared trait. [`InferenceAdapter::capabilities`]
    /// takes no model argument, so a per-request-routed client cannot answer it
    /// precisely. trusty-code does not rely on this method for its per-model
    /// decisions — `agent_loop::build_request` asks `crate::provider::provider_for(slug)`,
    /// which resolves per model slug — so returning the OpenRouter profile (the
    /// routing default, and the backend the overwhelming majority of slugs take)
    /// is honest for diagnostics without any call site depending on it.
    /// Collapsing that per-slug resolution into the shared registry needs
    /// slug-level capabilities upstream and is tracked as epic #4429 follow-up
    /// work, not smuggled in here.
    /// What: `capabilities(ProviderId::OpenRouter)`.
    /// Test: `dispatch::tests::capabilities_report_openrouter_default`.
    fn capabilities(&self) -> &ProviderCapabilities {
        capabilities(ProviderId::OpenRouter)
    }

    /// Dispatch `req` to Bedrock or the shared OpenAI-compat transport based on
    /// `req.model`.
    ///
    /// Why: the single blocking method every caller (`AgentLoop`, `run_task`,
    /// `task::executor`) invokes through `Arc<dyn InferenceAdapter>`.
    /// What: `bedrock/*` slugs go through [`Self::bedrock`]; every other slug
    /// (OpenRouter default, `fireworks/*`, and `together/*`) goes through the
    /// `OpenAiCompatClient`, which handles the OpenRouter/Fireworks/Together split
    /// and surfaces a missing credential as `InferenceError::MissingConfig` at this
    /// point.
    /// Test: `dispatch::tests::*`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        if Self::routes_to_bedrock(&req.model) {
            self.bedrock().await?.chat(req).await
        } else {
            self.openai_compat.chat(req).await
        }
    }

    /// Stream `req` through the same routing decision [`Self::chat`] makes.
    ///
    /// Why (#4425): without this override the trait's default would buffer via
    /// [`Self::chat`] — every trusty-code turn would keep arriving as one paste
    /// even though the OpenAI-compat lane has had real SSE since #3696. Routing
    /// the streaming call through the IDENTICAL `routes_to_bedrock` gate (rather
    /// than a second copy of the slug logic) is what guarantees a model cannot
    /// stream from one backend and block on another.
    /// What: `bedrock/*` → the Bedrock transport's `chat_stream` (buffered
    /// today; real `ConverseStream` is #4426); everything else → the
    /// OpenAI-compat transport's, which is native SSE.
    /// Test: `dispatch::tests::stream_routes_like_chat`.
    async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        if Self::routes_to_bedrock(&req.model) {
            self.bedrock().await?.chat_stream(req).await
        } else {
            self.openai_compat.chat_stream(req).await
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `routes_to_bedrock` is `true` for `bedrock/*` slugs.
    ///
    /// Why: this is the exact gate `chat` uses to pick the Bedrock transport; a
    /// wrong answer means Bedrock slugs silently go to the OpenAI-compat path.
    /// What: assert `true` for a representative Bedrock inference-profile slug.
    /// Test: this test.
    #[test]
    fn routes_bedrock_prefix_true() {
        assert!(DispatchingLlmClient::routes_to_bedrock(
            "bedrock/us.anthropic.claude-sonnet-4-6"
        ));
    }

    /// `routes_to_bedrock` is `false` for every non-Bedrock slug family,
    /// including `fireworks/*` and `together/*` (which route through the
    /// OpenAI-compat transport, not Bedrock).
    ///
    /// Why: guards against a routing regression sending ordinary OpenRouter,
    /// Fireworks, or Together traffic to the (credential-less) Bedrock cell.
    /// What: assert `false` for representative OpenRouter, Fireworks, and
    /// Together slugs.
    /// Test: this test.
    #[test]
    fn routes_non_bedrock_prefix_false() {
        for slug in [
            "anthropic/claude-sonnet-4-5",
            "openai/gpt-4o-mini",
            "qwen/qwen-2.5-coder-32b-instruct",
            "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct",
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo",
        ] {
            assert!(
                !DispatchingLlmClient::routes_to_bedrock(slug),
                "slug {slug} must not route to Bedrock"
            );
        }
    }

    /// Constructing a `DispatchingLlmClient` never touches credentials and never
    /// initialises the Bedrock cell.
    ///
    /// Why: pins the "default configuration works standalone" guarantee (#2245) —
    /// building the client (e.g. at CLI startup) must not require AWS credentials
    /// or any API key just because the transports exist in-process.
    /// What: build the client; assert the Bedrock `OnceCell` is still empty (it
    /// is only inspected on a `bedrock/*` `chat`).
    /// Test: this test.
    #[test]
    fn construction_touches_no_credentials() {
        let client = DispatchingLlmClient::new();
        assert!(
            client.bedrock.get().is_none(),
            "Bedrock cell must be lazy — untouched at construction"
        );
    }

    /// A non-bedrock dispatch never initialises the Bedrock cell.
    ///
    /// Why: proves the OpenAI-compat path is fully independent of the AWS SDK —
    /// a pure-OpenRouter/Fireworks run must not construct the Bedrock client.
    /// What: build with an empty store so an OpenRouter chat fails fast on the
    /// missing key (when no ambient key exists), then assert the Bedrock cell is
    /// still empty. Skips the chat when a real key is present locally, but still
    /// asserts the cell stays empty.
    /// Test: this test.
    #[tokio::test]
    async fn bedrock_construction_is_lazy() {
        let client = DispatchingLlmClient::new();
        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
            stop: None,
        };
        // Best-effort: the call may error (missing key) or, with a real ambient
        // key, would attempt a live request — either way we only care that the
        // Bedrock cell was never touched. Guard the live case out.
        if !std::env::var("OPENROUTER_API_KEY").is_ok_and(|v| !v.is_empty()) {
            let _ = client.chat(&req).await;
        }
        assert!(
            client.bedrock.get().is_none(),
            "a non-bedrock dispatch must never initialise the Bedrock cell"
        );
    }

    /// The dispatcher reports its own label, not a backend's (#4425).
    ///
    /// Why: naming a backend here would misattribute every log line for the
    /// other backend, since one dispatcher serves both per request.
    /// What: assert `name() == "dispatching"`.
    /// Test: this test.
    #[test]
    fn name_is_dispatching() {
        assert_eq!(DispatchingLlmClient::new().name(), "dispatching");
    }

    /// Capabilities report the OpenRouter routing default (#4425).
    ///
    /// Why: pins the documented answer to the shared trait's model-free
    /// `capabilities()` question, so a future change to it is a deliberate
    /// decision rather than a silent drift in what a diagnostic reports.
    /// What: assert the returned profile's id is OpenRouter.
    /// Test: this test.
    #[test]
    fn capabilities_report_openrouter_default() {
        assert_eq!(
            DispatchingLlmClient::new().capabilities().id,
            ProviderId::OpenRouter
        );
    }

    /// A streaming dispatch routes by the SAME gate the blocking one uses
    /// (#4425).
    ///
    /// Why: if `chat` and `chat_stream` could disagree, a model would reach one
    /// backend when the TUI is attached and another when it is not — the worst
    /// class of bug, because it only appears in one of the two modes.
    /// What: assert `routes_to_bedrock` (the single gate both call) agrees with
    /// the provider factory for both families. The transports themselves are
    /// covered by their own tests; issuing a real stream here would need a
    /// network.
    /// Test: this test.
    #[test]
    fn stream_routes_like_chat() {
        for (slug, expect_bedrock) in [
            ("bedrock/us.anthropic.claude-sonnet-4-6", true),
            ("openai/gpt-4o-mini", false),
            ("fireworks/accounts/fireworks/models/x", false),
        ] {
            assert_eq!(
                DispatchingLlmClient::routes_to_bedrock(slug),
                expect_bedrock,
                "slug {slug} must route identically for chat and chat_stream"
            );
        }
    }
}
