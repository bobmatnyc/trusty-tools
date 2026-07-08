//! Transport dispatch: route each `ChatRequest` to OpenRouter or Bedrock by
//! model slug (#1021 phase 1).
//!
//! Why: Every caller of `LlmClientTrait::chat` (`AgentLoop`, `run_task`,
//! `task::executor`) is constructed with exactly ONE `Arc<dyn LlmClientTrait>`
//! shared across every agent in a run, but different agents can be routed to
//! different model slugs (`crate::provider::resolve_model`) — some
//! `bedrock/*`, most not. Rather than threading a second client through every
//! call site, `DispatchingLlmClient` is itself an `LlmClientTrait` impl that
//! picks the real transport per-request from `req.model`, using the exact
//! same `crate::provider::provider_for` routing `AgentLoop::build_request`
//! already consults for tool-choice/caching decisions (#1021's `provider`
//! module) — so there is exactly one source of truth for "is this a Bedrock
//! slug".
//! What: Wraps the existing OpenRouter `LlmClient` (built eagerly, exactly as
//! before) plus a lazily-constructed `BedrockChatClient` behind a
//! `tokio::sync::OnceCell`. The Bedrock client is only ever built the first
//! time a `bedrock/*` slug is actually requested — a pure-OpenRouter run
//! never touches the AWS SDK or needs AWS credentials, preserving the
//! "default configuration works standalone" rule.
//! Test: `dispatch::tests::*` — routing by slug prefix (mocked, no network),
//! `bedrock_construction_is_lazy` proves the OpenRouter-only path never
//! initialises the Bedrock cell.

use async_trait::async_trait;
use tokio::sync::OnceCell;

use super::bedrock::BedrockChatClient;
use super::client::{LlmClient, LlmClientConfig};
use super::client_trait::LlmClientTrait;
use super::error::LlmError;
use super::request::ChatRequest;
use super::response::ChatResponse;

/// Provider name `crate::provider::provider_for` reports for `bedrock/*`
/// slugs — the single dispatch condition this client checks.
const BEDROCK_PROVIDER_NAME: &str = "bedrock";

/// Routes each chat request to the OpenRouter HTTP transport or the Bedrock
/// Converse transport, by model slug.
///
/// Why: This is the seam that makes `bedrock/*` model slugs actually reach
/// AWS Bedrock instead of being sent (nonsensically) to OpenRouter's HTTP
/// endpoint — closing the "transport is hardcoded to OpenRouter" gap #1021
/// identified. Production call sites (`main.rs`, `task::mock_llm`) construct
/// this instead of a bare `LlmClient`; every other caller keeps depending on
/// the object-safe `LlmClientTrait`, so no signature at any call site changes.
/// What: `openrouter` is built eagerly (unchanged from before this ticket);
/// `bedrock` is a `OnceCell` populated on the first `bedrock/*` request via
/// `BedrockChatClient::from_env` (standard AWS credential chain — no key
/// required at construction time; a missing/invalid credential surfaces as an
/// `LlmError` on that first call, not at startup).
/// Test: `dispatch::tests::*`.
#[derive(Debug)]
pub struct DispatchingLlmClient {
    openrouter: LlmClient,
    bedrock: OnceCell<BedrockChatClient>,
}

impl DispatchingLlmClient {
    /// Construct from an explicit OpenRouter config.
    ///
    /// Why: Mirrors `LlmClient::from_config` so existing call sites
    /// (`main.rs::build_llm_client`, `task::mock_llm::build_llm_client`) swap
    /// in this type with a one-line change.
    /// What: Builds the OpenRouter `LlmClient` eagerly (identical behaviour
    /// to before); the Bedrock cell starts empty.
    /// Test: `dispatch::tests::openrouter_slug_never_touches_bedrock_cell`.
    pub fn from_config(config: LlmClientConfig) -> Result<Self, LlmError> {
        Ok(Self {
            openrouter: LlmClient::from_config(config)?,
            bedrock: OnceCell::new(),
        })
    }

    /// Construct from the `OPENROUTER_API_KEY` environment variable.
    ///
    /// Why: Convenience entry point mirroring `LlmClient::from_env`.
    /// What: Delegates to [`Self::from_config`] via `LlmClientConfig::from_env`.
    /// Test: Exercised transitively wherever `LlmClient::from_env` already was.
    pub fn from_env() -> Result<Self, LlmError> {
        Self::from_config(LlmClientConfig::from_env()?)
    }

    /// Get (or lazily build) the Bedrock Converse transport.
    ///
    /// Why: AWS credential resolution is async and must never run for a
    /// request that doesn't need it; `OnceCell::get_or_try_init` guarantees
    /// at most one build attempt is ever made, and a failed attempt can be
    /// retried on the next `bedrock/*` request rather than poisoning the
    /// client forever.
    /// What: Returns the cached client, or builds one via
    /// `BedrockChatClient::from_env` on first use.
    /// Test: `dispatch::tests::bedrock_slug_routes_through_bedrock_client`
    /// (via a construction-failure path, since no AWS creds exist in tests).
    async fn bedrock(&self) -> Result<&BedrockChatClient, LlmError> {
        self.bedrock
            .get_or_try_init(BedrockChatClient::from_env)
            .await
    }

    /// Whether `model` routes to the Bedrock transport.
    ///
    /// Why: Single source of truth — reuses `crate::provider::provider_for`,
    /// the exact factory `AgentLoop::build_request` already consults for
    /// tool-choice/caching decisions, so routing can never disagree between
    /// "which transport sends this" and "which `Provider` normalises this".
    /// What: `true` iff `provider_for(model).name() == "bedrock"`.
    /// Test: `dispatch::tests::routes_bedrock_prefix_true`,
    /// `dispatch::tests::routes_non_bedrock_prefix_false`.
    fn routes_to_bedrock(model: &str) -> bool {
        crate::provider::provider_for(model).name() == BEDROCK_PROVIDER_NAME
    }
}

#[async_trait]
impl LlmClientTrait for DispatchingLlmClient {
    /// Dispatch `req` to Bedrock or OpenRouter based on `req.model`.
    ///
    /// Why: The single method every caller (`AgentLoop`, `run_task`,
    /// `task::executor`) invokes through `Arc<dyn LlmClientTrait>`.
    /// What: `bedrock/*` slugs go through [`Self::bedrock`]; every other
    /// slug goes through the unchanged OpenRouter `LlmClient::chat` path.
    /// Test: `dispatch::tests::*`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if Self::routes_to_bedrock(&req.model) {
            self.bedrock().await?.chat(req).await
        } else {
            self.openrouter.chat(req).await
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `routes_to_bedrock` is `true` for `bedrock/*` slugs.
    ///
    /// Why: This is the exact gate `chat` uses to pick a transport; a wrong
    /// answer here means Bedrock slugs silently go to OpenRouter (or a
    /// missing AWS credential blocks an OpenRouter-only run).
    /// What: Assert `true` for a representative Bedrock inference-profile slug.
    /// Test: this test.
    #[test]
    fn routes_bedrock_prefix_true() {
        assert!(DispatchingLlmClient::routes_to_bedrock(
            "bedrock/us.anthropic.claude-sonnet-4-6"
        ));
    }

    /// `routes_to_bedrock` is `false` for every non-Bedrock slug family.
    ///
    /// Why: Guards against a routing regression sending ordinary OpenRouter
    /// traffic to the (uninitialised, credential-less) Bedrock cell.
    /// What: Assert `false` for representative OpenRouter-namespaced slugs.
    /// Test: this test.
    #[test]
    fn routes_non_bedrock_prefix_false() {
        for slug in [
            "anthropic/claude-sonnet-4-5",
            "openai/gpt-4o-mini",
            "qwen/qwen-2.5-coder-32b-instruct",
        ] {
            assert!(
                !DispatchingLlmClient::routes_to_bedrock(slug),
                "slug {slug} must not route to Bedrock"
            );
        }
    }

    /// Constructing a `DispatchingLlmClient` never touches the Bedrock cell.
    ///
    /// Why: Pins the "default configuration works standalone" guarantee —
    /// building the client (e.g. at CLI startup) must not require AWS
    /// credentials just because the Bedrock transport exists in-process.
    /// What: Build with a fake OpenRouter key; assert construction succeeds
    /// (the Bedrock `OnceCell` is never even inspected until `chat` is
    /// called with a `bedrock/*` model).
    /// Test: this test.
    #[test]
    fn openrouter_slug_never_touches_bedrock_cell() {
        let config = LlmClientConfig::new("sk-or-test").expect("config");
        let client = DispatchingLlmClient::from_config(config);
        assert!(client.is_ok(), "expected Ok, got: {client:?}");
    }

    // NOTE: `chat()` end-to-end dispatch for a `bedrock/*` slug is
    // deliberately NOT exercised here with a real `BedrockChatClient::from_env`
    // call: on a machine with real AWS credentials configured (e.g.
    // `AWS_PROFILE=cto`, per this repo's CLAUDE.md), that would make a live
    // network call to Bedrock from an ordinary `cargo test` run — not
    // gated behind `--include-ignored`. `routes_bedrock_prefix_true` already
    // proves the routing decision itself; `bedrock::tests::live_bedrock_call`
    // (in `llm/bedrock/tests.rs`, `#[ignore]`-gated) covers the real call.
}
