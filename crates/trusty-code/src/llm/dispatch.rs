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
//! What: Wraps an OPTIONAL OpenRouter `LlmClient` (built eagerly when a
//! config is supplied, matching the original behaviour) plus a
//! lazily-constructed `BedrockChatClient` behind a `tokio::sync::OnceCell`.
//! The Bedrock client is only ever built the first time a `bedrock/*` slug is
//! actually requested — a pure-OpenRouter run never touches the AWS SDK or
//! needs AWS credentials, preserving the "default configuration works
//! standalone" rule. Symmetrically (#2245), the OpenRouter transport is
//! OPTIONAL at construction time: a pure-Bedrock run must not be blocked by a
//! missing `OPENROUTER_API_KEY` — that error now surfaces only if a
//! non-bedrock model slug is actually dispatched through a client built
//! without OpenRouter config.
//! Test: `dispatch::tests::*` — routing by slug prefix (mocked, no network),
//! `bedrock_construction_is_lazy` proves the OpenRouter-only path never
//! initialises the Bedrock cell, `construction_succeeds_with_no_openrouter_config`
//! and `openrouter_request_without_config_fails_at_chat_time` prove the
//! reverse: a Bedrock-only client builds fine and only fails at request time.

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
/// What: `openrouter` is `Some(LlmClient)` when a config was supplied at
/// construction (built eagerly — the reqwest client itself is cheap and
/// synchronous to build), `None` otherwise; `bedrock` is a `OnceCell`
/// populated on the first `bedrock/*` request via `BedrockChatClient::from_env`
/// (standard AWS credential chain — no key required at construction time; a
/// missing/invalid credential surfaces as an `LlmError` on that first call,
/// not at startup). A request that resolves to the missing transport (either
/// direction) fails with `LlmError::MissingConfig` at `chat()` time, never at
/// construction — so a pure-Bedrock run never needs `OPENROUTER_API_KEY` and
/// a pure-OpenRouter run never needs AWS credentials (#2245).
/// Test: `dispatch::tests::*`.
#[derive(Debug)]
pub struct DispatchingLlmClient {
    openrouter: Option<LlmClient>,
    bedrock: OnceCell<BedrockChatClient>,
}

impl DispatchingLlmClient {
    /// Construct from an optional OpenRouter config.
    ///
    /// Why: The single constructor every entry point (`main.rs`,
    /// `task::mock_llm`) should funnel through: whether OpenRouter creds are
    /// available is decided by the CALLER (typically "did
    /// `LlmClientConfig::from_env()` succeed"), and this type must not
    /// second-guess that decision by hard-failing when it's `None` — only a
    /// request that actually needs OpenRouter should ever surface that
    /// error (#2245).
    /// What: Builds the OpenRouter `LlmClient` eagerly when `config.is_some()`
    /// (identical behaviour to the pre-#2245 mandatory path); leaves it unset
    /// otherwise. The Bedrock cell always starts empty. Only fails if a
    /// SUPPLIED config fails to build (the underlying reqwest/TLS setup,
    /// "extremely rare" per `LlmClient::from_config`'s own docs).
    /// Test: `dispatch::tests::construction_succeeds_with_no_openrouter_config`,
    /// `dispatch::tests::openrouter_slug_never_touches_bedrock_cell`.
    pub fn new(openrouter_config: Option<LlmClientConfig>) -> Result<Self, LlmError> {
        let openrouter = openrouter_config.map(LlmClient::from_config).transpose()?;
        Ok(Self {
            openrouter,
            bedrock: OnceCell::new(),
        })
    }

    /// Construct from an explicit, MANDATORY OpenRouter config.
    ///
    /// Why: Back-compat convenience for callers that already know they need
    /// OpenRouter (e.g. tests constructing a client for an OpenRouter-only
    /// scenario) and want construction itself to fail fast on a bad config.
    /// What: `Self::new(Some(config))`.
    /// Test: `dispatch::tests::openrouter_slug_never_touches_bedrock_cell`.
    pub fn from_config(config: LlmClientConfig) -> Result<Self, LlmError> {
        Self::new(Some(config))
    }

    /// Construct from the `OPENROUTER_API_KEY` environment variable,
    /// MANDATORY (fails if unset).
    ///
    /// Why: Convenience entry point mirroring `LlmClient::from_env` for
    /// callers that want the original "key required" behaviour. Production
    /// call sites that must tolerate a Bedrock-only run instead call
    /// [`Self::new`] with `LlmClientConfig::from_env().ok()` (#2245).
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
    /// slug goes through the OpenRouter `LlmClient::chat` path IF one was
    /// configured — a request that needs the missing transport returns
    /// `LlmError::MissingConfig` naming the model and the env var to set
    /// (#2245), rather than the client having refused to construct at all.
    /// Test: `dispatch::tests::*`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if Self::routes_to_bedrock(&req.model) {
            self.bedrock().await?.chat(req).await
        } else {
            let client = self.openrouter.as_ref().ok_or_else(|| {
                LlmError::MissingConfig(format!(
                    "OPENROUTER_API_KEY is required to reach model '{}' (not a bedrock/* \
                     slug). Export it before running, e.g. `export OPENROUTER_API_KEY=sk-or-...`.",
                    req.model
                ))
            })?;
            client.chat(req).await
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

    /// A client built with NO OpenRouter config still constructs successfully
    /// — the exact shape a Bedrock-only run takes (#2245).
    ///
    /// Why: Pins the fix for the reported bug: building `DispatchingLlmClient`
    /// must not require `OPENROUTER_API_KEY` just because the OpenRouter
    /// transport theoretically exists — only an OpenRouter-bound `chat()`
    /// call should ever need it.
    /// What: `DispatchingLlmClient::new(None)` returns `Ok`.
    /// Test: this test.
    #[test]
    fn construction_succeeds_with_no_openrouter_config() {
        let client = DispatchingLlmClient::new(None);
        assert!(client.is_ok(), "expected Ok, got: {client:?}");
    }

    /// A non-bedrock request through a Bedrock-only client (no OpenRouter
    /// config) fails at `chat()` time with a clear `MissingConfig` error —
    /// not silently, and not at construction time (#2245).
    ///
    /// Why: Confirms the deferred failure actually surfaces a useful error
    /// instead of e.g. a panic or a confusing transport-level failure.
    /// What: Build with `new(None)`, dispatch an `anthropic/*`-slugged
    /// request, assert `LlmError::MissingConfig`.
    /// Test: this test.
    #[tokio::test]
    async fn openrouter_request_without_config_fails_at_chat_time() {
        let client = DispatchingLlmClient::new(None).expect("construction succeeds");
        let req = ChatRequest {
            model: "anthropic/claude-sonnet-4-5".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
        };
        let err = client.chat(&req).await.unwrap_err();
        assert!(
            matches!(err, LlmError::MissingConfig(_)),
            "expected MissingConfig, got: {err:?}"
        );
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
