//! AWS Bedrock Converse transport — a thin bridge onto `trusty_common::inference`
//! (#2407 migration).
//!
//! Why: the entire Bedrock Converse wire integration (region resolution, the AWS
//! credential chain, the `ChatRequest`/`ChatResponse` <-> Converse conversion,
//! and the `cachePoint` prompt-cache translation) moved into the shared
//! `trusty_common::inference::bedrock` adapter in #2407, so every consumer shares
//! ONE implementation instead of tcode owning a private ~1000-SLOC copy. This
//! module is what remains in tcode: a thin wrapper that delegates every method
//! to the shared adapter — zero local Bedrock/Converse logic.
//! What: [`BedrockChatClient`] wraps a
//! [`trusty_common::inference::BedrockAdapter`] and implements
//! [`InferenceAdapter`] by delegation. Since #4425 unified trusty-code's wire
//! types with `trusty_common::inference`'s, the wrapper no longer converts
//! anything: the request, the response, and the error type are already the
//! shared ones, so the former `super::convert` bridge is gone. The shared
//! adapter constructs its AWS client lazily on first `chat`, so
//! [`Self::from_env`]/[`Self::new`] touch no AWS credentials.
//! [`Self::new`]/[`Self::region`] are thin passthroughs kept for public-API
//! compatibility with the pre-#2407 local transport (no in-tree caller uses
//! them post-migration, but this is a published rlib crate and the surface
//! removal would otherwise be undocumented/silent).
//!
//! Streaming (#4425/#4426): [`Self::chat_stream`] forwards to the shared
//! adapter's, which since #4426 is a NATIVE `ConverseStream` transport — a
//! Bedrock turn arrives token-by-token, not as one buffered delta. This
//! wrapper needed no edit for that (only this doc line): because it delegates
//! rather than reimplements, the whole change landed inside
//! `trusty_common::inference::bedrock`.
//! Test: `bedrock::tests::*` (offline construction) plus the shared adapter's own
//! conversion + `#[ignore]`-gated live coverage in
//! `trusty_common::inference::bedrock`.

use async_trait::async_trait;
use trusty_common::inference::{
    BedrockAdapter, ChatRequest, ChatResponse, ChatStream, InferenceAdapter, InferenceError,
    ProviderCapabilities, ToolChoice,
};

/// AWS Bedrock Converse transport, backed by the shared inference adapter.
///
/// Why: satisfies the same `chat(&ChatRequest) -> Result<ChatResponse, ..>`
/// contract every other tcode transport exposes, so `DispatchingLlmClient`
/// (`super::dispatch`) can route `bedrock/*` slugs here without any caller
/// knowing the backend differs — while the Converse mechanics live in
/// `trusty_common::inference::bedrock`, not in this crate.
/// What: holds a shared [`BedrockAdapter`] (which owns the resolved region and a
/// lazily-built AWS client). `Debug` so `DispatchingLlmClient`'s
/// `OnceCell<BedrockChatClient>` can derive it.
/// Test: `bedrock::tests::from_env_constructs_offline`.
#[derive(Debug)]
pub struct BedrockChatClient {
    inner: BedrockAdapter,
}

impl BedrockChatClient {
    /// Construct a `BedrockChatClient` for an explicit (or default) region.
    ///
    /// Why: public-API compatibility passthrough — the pre-#2407 local transport
    /// exposed this constructor; no in-tree caller uses it today (everything goes
    /// through [`Self::from_env`] via `DispatchingLlmClient`), but this is a
    /// published rlib crate, so the constructor is kept rather than silently
    /// dropped.
    /// What: wraps [`BedrockAdapter::new`] with `region`. Async + `Result` to
    /// match the pre-migration signature; never actually fails.
    /// Test: `bedrock::tests::new_constructs_offline`.
    pub async fn new(region: Option<&str>) -> Result<Self, InferenceError> {
        Ok(Self {
            inner: BedrockAdapter::new(region),
        })
    }

    /// Construct a `BedrockChatClient` from the environment.
    ///
    /// Why: the convenience entry point `DispatchingLlmClient` lazily calls on
    /// the first `bedrock/*` request. Region resolves from
    /// `TRUSTY_AWS_REGION`/`AWS_REGION`/default; credentials come from the AWS
    /// chain, resolved lazily on the first `chat`, so this never touches AWS.
    /// What: delegates to [`Self::new`] with `region: None`.
    /// Test: `bedrock::tests::from_env_constructs_offline`.
    pub async fn from_env() -> Result<Self, InferenceError> {
        Self::new(None).await
    }

    /// The AWS region this client is configured for.
    ///
    /// Why: public-API compatibility passthrough (see [`Self::new`]) — exposed
    /// for diagnostics and telemetry by any caller that held the pre-#2407 type.
    /// What: delegates to the shared adapter's `region()`.
    /// Test: `bedrock::tests::region_reports_resolved_value`.
    pub fn region(&self) -> &str {
        self.inner.region()
    }
}

/// Delegating [`InferenceAdapter`] impl (#4425).
///
/// Why: `DispatchingLlmClient` routes `bedrock/*` slugs here through the SHARED
/// trait, so this wrapper must satisfy that trait rather than expose a
/// look-alike inherent `chat`. Every method delegates — including the
/// capability and tool-choice hooks, which Bedrock overrides with the
/// Anthropic dialect: forwarding them (instead of inheriting the trait's
/// OpenAI-dialect defaults) is what keeps a `bedrock/*` turn wire-identical to
/// calling the shared adapter directly.
/// What: name/capabilities/chat/chat_stream/map_tool_choice all forward to
/// [`BedrockAdapter`]. The capability-derived `supports_*` defaults need no
/// override because they read `capabilities()`, which is forwarded.
/// Test: `bedrock::tests::*`; wire behaviour is covered by the shared adapter's
/// own tests.
#[async_trait]
impl InferenceAdapter for BedrockChatClient {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        self.inner.capabilities()
    }

    // #4425: forward the model-aware form too — a decorator that answered it
    // from the trait default would silently drop back to `capabilities()` and
    // stop reflecting the wrapped adapter.
    fn capabilities_for(&self, model: &str) -> &ProviderCapabilities {
        self.inner.capabilities_for(model)
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.inner.chat(request).await
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream, InferenceError> {
        self.inner.chat_stream(request).await
    }

    fn map_tool_choice(&self, choice: ToolChoice) -> serde_json::Value {
        self.inner.map_tool_choice(choice)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_env` builds a client without touching AWS (the shared adapter's
    /// client is lazy).
    ///
    /// Why: pins that constructing the Bedrock transport — e.g. on the first
    /// `bedrock/*` dispatch — requires no AWS credentials, matching the
    /// pre-migration lazy-construction guarantee (#2245).
    /// What: call `from_env`; assert it succeeds.
    /// Test: this test.
    #[tokio::test]
    async fn from_env_constructs_offline() {
        assert!(BedrockChatClient::from_env().await.is_ok());
    }

    /// `new` with an explicit region builds a client without touching AWS.
    ///
    /// Why: pins the public-API-compatibility constructor's lazy-construction
    /// guarantee, matching [`from_env_constructs_offline`].
    /// What: call `new(Some("us-west-2"))`; assert it succeeds.
    /// Test: this test.
    #[tokio::test]
    async fn new_constructs_offline() {
        assert!(BedrockChatClient::new(Some("us-west-2")).await.is_ok());
    }

    /// `region()` reports the resolved region, explicit value preferred.
    ///
    /// Why: pins the public-API-compatibility accessor actually reflects what
    /// the shared adapter resolved, not a stale/default value.
    /// What: build with an explicit region; assert `region()` returns it.
    /// Test: this test.
    #[tokio::test]
    async fn region_reports_resolved_value() {
        let client = BedrockChatClient::new(Some("eu-west-1"))
            .await
            .expect("build");
        assert_eq!(client.region(), "eu-west-1");
    }
}
