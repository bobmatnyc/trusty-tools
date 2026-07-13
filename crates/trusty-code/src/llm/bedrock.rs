//! AWS Bedrock Converse transport — a thin bridge onto `trusty_common::inference`
//! (#2407 migration).
//!
//! Why: the entire Bedrock Converse wire integration (region resolution, the AWS
//! credential chain, the `ChatRequest`/`ChatResponse` <-> Converse conversion,
//! and the `cachePoint` prompt-cache translation) moved into the shared
//! `trusty_common::inference::bedrock` adapter in #2407, so every consumer shares
//! ONE implementation instead of tcode owning a private ~1000-SLOC copy. This
//! module is what remains in tcode: a thin `LlmClientTrait`-shaped wrapper that
//! bridges tcode's wire types to the shared adapter at exactly one seam (reusing
//! `super::convert`, the same field-by-field bridge #2406 established for the
//! OpenAI-compatible transport) — zero local Bedrock/Converse logic.
//! What: [`BedrockChatClient`] wraps a
//! [`trusty_common::inference::BedrockAdapter`]. `chat` converts the request via
//! [`super::convert::to_shared_request`] (passing the FULL `bedrock/`-prefixed
//! slug as the wire model — the shared adapter strips the prefix before the AWS
//! call and keeps it for telemetry), delegates to the shared adapter, maps its
//! [`trusty_common::inference::InferenceError`] back to [`LlmError`] via
//! [`super::convert::map_error`], and converts the response via
//! [`super::convert::from_shared_response`]. The shared adapter constructs its
//! AWS client lazily on first `chat`, so [`Self::from_env`]/[`Self::new`] touch
//! no AWS credentials. [`Self::new`]/[`Self::region`] are thin passthroughs kept
//! for public-API compatibility with the pre-#2407 local transport (no in-tree
//! caller uses them post-migration, but this is a published rlib crate and the
//! surface removal would otherwise be undocumented/silent).
//! Test: `bedrock::tests::*` (offline construction) plus the shared adapter's own
//! conversion + `#[ignore]`-gated live coverage in
//! `trusty_common::inference::bedrock`.

use trusty_common::inference::{BedrockAdapter, InferenceAdapter};

use super::convert::{from_shared_response, map_error, to_shared_request};
use super::error::LlmError;
use super::request::ChatRequest;
use super::response::ChatResponse;

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
    pub async fn new(region: Option<&str>) -> Result<Self, LlmError> {
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
    pub async fn from_env() -> Result<Self, LlmError> {
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

    /// Execute one Converse call and return the response.
    ///
    /// Why: the single method `DispatchingLlmClient` needs to satisfy
    /// `LlmClientTrait::chat` for `bedrock/*` slugs.
    /// What: converts `req` to the shared request (the FULL slug is passed as the
    /// wire model — the shared adapter strips the `bedrock/` prefix before the AWS
    /// call), delegates to the shared [`BedrockAdapter`], maps a shared
    /// [`trusty_common::inference::InferenceError`] to [`LlmError`], and converts
    /// the shared response back to tcode's [`ChatResponse`].
    /// Test: the shared adapter's conversion tests + the `#[ignore]`-gated live
    /// call cover the wire behaviour end-to-end.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let shared = to_shared_request(req, req.model.clone());
        let resp = self.inner.chat(&shared).await.map_err(map_error)?;
        Ok(from_shared_response(resp))
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
