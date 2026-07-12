//! OpenAI-compatible inference transport, backed by the shared
//! `trusty_common::inference` adapter layer (#2406, epic #2400).
//!
//! Why: trusty-code used to hand-roll its own OpenRouter `reqwest` client. Epic
//! #2400 centralises the OpenAI-compatible HTTP mechanics (auth, the OpenRouter
//! detailed-usage directive, HTTP→error classification, response parsing) in
//! ONE shared core so every consumer shares it instead of six near-identical
//! copies. This module is tcode's thin consumer of that core: it selects the
//! provider (OpenRouter or Fireworks) by model slug, resolves the credential via
//! the shared 3-tier resolver (process env > `.env.local` > secure store),
//! builds the matching `trusty_common::inference` adapter once (caching it so
//! the underlying `reqwest` connection pool is reused across a run's many
//! turns — a bake-off latency concern), and bridges tcode's request/response
//! types across the seam (`super::convert`). Bedrock stays on its own Converse
//! transport (`super::bedrock`, routed by `super::dispatch`) — its migration
//! into commons is #2407.
//! What: [`OpenAiCompatClient`] implements [`LlmClientTrait`]. `fireworks/*`
//! slugs route to the Fireworks adapter (stripping the `fireworks/` routing
//! prefix to the provider-native model id and requiring `FIREWORKS_API_KEY`);
//! everything else routes to OpenRouter, sending the slug unchanged (identical
//! to the pre-#2406 behaviour). A missing credential surfaces at `chat()` time
//! (not construction), preserving the #2245 deferred-failure contract. The base
//! URLs are overridable (`TCODE_OPENROUTER_BASE_URL` / `TCODE_FIREWORKS_BASE_URL`
//! or [`OpenAiCompatClient::with_config`]) so an offline mock server can be
//! targeted end-to-end.
//! Test: `client::tests::*` (provider selection, prefix stripping, hermetic
//! missing-credential path against an injected empty store) and the black-box
//! HTTP round-trip in `tests/inference_shared_adapter_e2e.rs`; the `#[ignore]`
//! `live_openrouter_call` / `live_fireworks_call` smokes hit the real APIs.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use trusty_common::inference::{
    InferenceAdapter,
    credentials::{KeyStore, default_store, resolve_key_with},
    provider_for,
    providers::{fireworks, openrouter},
    registry::ProviderId,
};

use super::client_trait::LlmClientTrait;
use super::convert;
use super::error::LlmError;
use super::request::ChatRequest;
use super::response::ChatResponse;

/// Env var overriding the OpenRouter API base URL (for offline mock testing /
/// self-hosted gateways). Defaults to [`openrouter::OPENROUTER_BASE_URL`].
const OPENROUTER_BASE_URL_ENV: &str = "TCODE_OPENROUTER_BASE_URL";

/// Env var overriding the Fireworks API base URL. Defaults to
/// [`fireworks::FIREWORKS_BASE_URL`].
const FIREWORKS_BASE_URL_ENV: &str = "TCODE_FIREWORKS_BASE_URL";

/// The provider name `crate::provider::provider_for` reports for `fireworks/*`
/// slugs — the single routing condition this client checks (mirroring how
/// `super::dispatch` keys Bedrock routing off the same factory).
const FIREWORKS_PROVIDER_NAME: &str = "fireworks";

/// OpenAI-compatible transport over the shared inference adapter, routing
/// OpenRouter and Fireworks by model slug.
///
/// Why: one place that owns "which OpenAI-dialect provider, whose key, which
/// base URL" so the dispatch layer and the agent loop stay backend-agnostic.
/// What: holds the shared credential [`KeyStore`], the (overridable) per-provider
/// base URLs, and a lazily-populated cache of built adapters keyed by
/// [`ProviderId`] so each provider's `reqwest` client (and its connection pool)
/// is constructed at most once per process.
/// Test: `client::tests::*`.
pub struct OpenAiCompatClient {
    store: Box<dyn KeyStore>,
    openrouter_base: String,
    fireworks_base: String,
    adapters: Mutex<HashMap<ProviderId, Arc<dyn InferenceAdapter>>>,
}

impl std::fmt::Debug for OpenAiCompatClient {
    /// Redacting `Debug`: the [`KeyStore`] and built adapters hold credential
    /// material, so this prints only the opaque type name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatClient").finish_non_exhaustive()
    }
}

impl OpenAiCompatClient {
    /// Construct with the process-default secure store and base URLs.
    ///
    /// Why: the production entry point — binary code funnels here so credential
    /// resolution uses the same env > `.env.local` > store chain everywhere.
    /// What: uses [`default_store`] and reads the base-URL env overrides once
    /// (falling back to the real provider URLs). Never touches credentials, so
    /// construction cannot fail on a missing key (#2245): that surfaces at
    /// `chat()` time.
    /// Test: `client::tests::selects_openrouter_for_plain_slug`.
    pub fn new() -> Self {
        Self::with_store(default_store())
    }

    /// Construct with an explicit [`KeyStore`], reading base URLs from the env
    /// overrides (or the real provider defaults).
    ///
    /// Why: lets a hermetic test inject a `MemoryKeyStore` so credential
    /// resolution never depends on the machine's real environment or `$HOME`.
    /// What: as [`Self::new`] but with the caller's store.
    /// Test: `client::tests::missing_openrouter_key_errors_at_chat_time`.
    pub fn with_store(store: Box<dyn KeyStore>) -> Self {
        let openrouter_base = std::env::var(OPENROUTER_BASE_URL_ENV)
            .unwrap_or_else(|_| openrouter::OPENROUTER_BASE_URL.to_string());
        let fireworks_base = std::env::var(FIREWORKS_BASE_URL_ENV)
            .unwrap_or_else(|_| fireworks::FIREWORKS_BASE_URL.to_string());
        Self::with_config(store, openrouter_base, fireworks_base)
    }

    /// Construct with an explicit store AND explicit base URLs.
    ///
    /// Why: the offline black-box e2e points both providers at a
    /// [`trusty_common::inference::test_support::MockInferenceServer`] without
    /// mutating process env (so the test stays parallel-safe).
    /// What: stores all three inputs and an empty adapter cache.
    /// Test: `tests/inference_shared_adapter_e2e.rs`.
    pub fn with_config(
        store: Box<dyn KeyStore>,
        openrouter_base: String,
        fireworks_base: String,
    ) -> Self {
        Self {
            store,
            openrouter_base,
            fireworks_base,
            adapters: Mutex::new(HashMap::new()),
        }
    }

    /// Select the OpenAI-dialect provider for a model slug.
    ///
    /// Why: single source of truth — reuses `crate::provider::provider_for`, the
    /// same factory `super::dispatch` consults for Bedrock routing and
    /// `AgentLoop::build_request` consults for usage/caching decisions, so
    /// routing can never disagree with normalisation.
    /// What: [`ProviderId::Fireworks`] iff that factory reports `"fireworks"`,
    /// else [`ProviderId::OpenRouter`] (the default for every other slug).
    /// Test: `client::tests::selects_fireworks_for_prefixed_slug`,
    /// `client::tests::selects_openrouter_for_plain_slug`.
    fn provider_for_slug(model: &str) -> ProviderId {
        if crate::provider::provider_for(model).name() == FIREWORKS_PROVIDER_NAME {
            ProviderId::Fireworks
        } else {
            ProviderId::OpenRouter
        }
    }

    /// The provider-native wire model id for a routing slug.
    ///
    /// Why: Fireworks serves bare `accounts/fireworks/models/*` ids, but tcode
    /// routes on a `fireworks/`-prefixed slug; the prefix is a routing artefact
    /// that must be stripped before the id is sent. OpenRouter takes the slug
    /// verbatim (identical to pre-#2406).
    /// What: strips a leading `fireworks/` for [`ProviderId::Fireworks`]; returns
    /// the slug unchanged otherwise.
    /// Test: `client::tests::fireworks_wire_model_strips_prefix`.
    fn wire_model(provider: ProviderId, model: &str) -> String {
        match provider {
            ProviderId::Fireworks => model
                .strip_prefix("fireworks/")
                .unwrap_or(model)
                .to_string(),
            _ => model.to_string(),
        }
    }

    /// Get (or lazily build + cache) the adapter for a provider.
    ///
    /// Why: building an adapter constructs a `reqwest` client; doing that once
    /// per provider and reusing it preserves connection pooling across a run's
    /// turns (a latency/cost baseline concern), mirroring the original client's
    /// build-once-at-startup behaviour.
    /// What: returns the cached `Arc` on a hit; on a miss builds via
    /// [`Self::build_adapter`], inserts, and returns it. A build failure is
    /// returned (not cached), so a later turn can retry once the credential is
    /// fixed.
    /// Test: exercised by every `chat` path.
    async fn adapter_for(
        &self,
        provider: ProviderId,
    ) -> Result<Arc<dyn InferenceAdapter>, LlmError> {
        {
            let cache = self.adapters.lock().await;
            if let Some(adapter) = cache.get(&provider) {
                return Ok(adapter.clone());
            }
        }
        let built: Arc<dyn InferenceAdapter> = Arc::from(self.build_adapter(provider)?);
        let mut cache = self.adapters.lock().await;
        Ok(cache.entry(provider).or_insert(built).clone())
    }

    /// Resolve the credential and build the shared adapter for a provider.
    ///
    /// Why: this is the seam that reuses the shared resolver + provider factory
    /// while keeping tcode's routing decision authoritative — we build ONLY the
    /// provider tcode selected, never letting the shared resolver's own
    /// prefix-based fallbacks silently re-home a request to OpenAI-direct or
    /// Anthropic-direct.
    /// What: for Fireworks, first resolves the `FIREWORKS_API_KEY` explicitly via
    /// the shared resolver so a missing key is ALWAYS a clear, Fireworks-specific
    /// [`LlmError::MissingConfig`] — never the shared two-stage resolver's silent
    /// fall-back to OpenRouter (which would be a wrong-provider misroute), and
    /// never an OpenRouter-flavoured error when both keys happen to be absent;
    /// only once the key is confirmed does it resolve + build the Fireworks
    /// adapter. For OpenRouter, resolves on an `openrouter/` routing slug (a
    /// missing key surfaces as the resolver's own `MissingCredential`, mapped to
    /// `MissingConfig`). The resolved model slug is irrelevant to the built
    /// adapter (it sends the per-request wire model), so a fixed routing slug is
    /// used.
    /// Test: `client::tests::missing_openrouter_key_errors_at_chat_time`,
    /// `client::tests::missing_fireworks_key_errors_not_falls_back`.
    fn build_adapter(&self, provider: ProviderId) -> Result<Box<dyn InferenceAdapter>, LlmError> {
        match provider {
            ProviderId::Fireworks => {
                if resolve_key_with("fireworks", self.store.as_ref()).is_none() {
                    return Err(LlmError::MissingConfig(
                        "FIREWORKS_API_KEY is required to reach a fireworks/* model. Export it \
                         (e.g. `export FIREWORKS_API_KEY=fw_...`), add it to .env.local, or set it \
                         via `tcode config keys set fireworks`."
                            .to_string(),
                    ));
                }
                // The key is present, so stage-1 of the resolver returns Fireworks.
                let resolved = provider_for("fireworks/route", self.store.as_ref())
                    .map_err(convert::map_error)?;
                fireworks::build(&resolved, &self.fireworks_base).map_err(convert::map_error)
            }
            _ => {
                let resolved = provider_for("openrouter/route", self.store.as_ref())
                    .map_err(convert::map_error)?;
                openrouter::build(&resolved, &self.openrouter_base).map_err(convert::map_error)
            }
        }
    }

    /// Route + issue one chat call through the shared adapter.
    ///
    /// Why: the single method [`LlmClientTrait`] exposes; all provider selection,
    /// prefix stripping, credential resolution, and type bridging live behind it.
    /// What: selects the provider from `req.model`, gets its cached adapter,
    /// converts the request (with the provider-native wire model), calls the
    /// shared adapter, maps any error, and converts the response back.
    /// Test: `client::tests::*` and `tests/inference_shared_adapter_e2e.rs`.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let provider = Self::provider_for_slug(&req.model);
        let adapter = self.adapter_for(provider).await?;
        let wire_model = Self::wire_model(provider, &req.model);
        let shared_req = convert::to_shared_request(req, wire_model);
        let shared_resp = adapter
            .chat(&shared_req)
            .await
            .map_err(convert::map_error)?;
        Ok(convert::from_shared_response(shared_resp))
    }
}

impl Default for OpenAiCompatClient {
    /// Delegates to [`OpenAiCompatClient::new`].
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for OpenAiCompatClient {
    /// Forward to the inherent [`OpenAiCompatClient::chat`].
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        OpenAiCompatClient::chat(self, req).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::inference::credentials::MemoryKeyStore;

    /// A plain (non-fireworks) slug selects OpenRouter.
    ///
    /// Why: preserves the pre-#2406 default — every non-fireworks, non-bedrock
    /// slug goes to OpenRouter unchanged.
    /// What: assert the provider selection for representative OpenRouter slugs.
    /// Test: this test.
    #[test]
    fn selects_openrouter_for_plain_slug() {
        for slug in [
            "openai/gpt-4o-mini",
            "anthropic/claude-sonnet-4-5",
            "qwen/qwen-2.5-coder-32b-instruct",
        ] {
            assert_eq!(
                OpenAiCompatClient::provider_for_slug(slug),
                ProviderId::OpenRouter,
                "{slug} must select OpenRouter"
            );
        }
    }

    /// A `fireworks/*` slug selects Fireworks.
    ///
    /// Why: this is the routing decision that makes Fireworks reachable (#2406).
    /// What: assert the provider selection for a fireworks slug.
    /// Test: this test.
    #[test]
    fn selects_fireworks_for_prefixed_slug() {
        assert_eq!(
            OpenAiCompatClient::provider_for_slug(
                "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct"
            ),
            ProviderId::Fireworks
        );
    }

    /// The Fireworks wire model strips the `fireworks/` routing prefix; the
    /// OpenRouter wire model is the slug verbatim.
    ///
    /// Why: Fireworks 404s on the prefixed id; OpenRouter needs the slug as-is.
    /// What: assert both directions.
    /// Test: this test.
    #[test]
    fn fireworks_wire_model_strips_prefix() {
        assert_eq!(
            OpenAiCompatClient::wire_model(
                ProviderId::Fireworks,
                "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct"
            ),
            "accounts/fireworks/models/llama-v3p1-70b-instruct"
        );
        assert_eq!(
            OpenAiCompatClient::wire_model(ProviderId::OpenRouter, "openai/gpt-4o-mini"),
            "openai/gpt-4o-mini"
        );
    }

    /// With an empty store and (assumed) no `OPENROUTER_API_KEY` env, an
    /// OpenRouter chat fails at `chat()` time with a `MissingConfig` — never at
    /// construction (#2245 deferred-failure contract).
    ///
    /// Why: pins that construction is credential-free and that the missing-key
    /// error is actionable and surfaces only when a request needs it.
    /// What: build with an empty `MemoryKeyStore`; if the ambient env happens to
    /// carry a real key (dev machines), skip the assertion rather than make a
    /// live call — the hermetic case (no env key) is what CI exercises.
    /// Test: this test.
    #[tokio::test]
    async fn missing_openrouter_key_errors_at_chat_time() {
        if std::env::var("OPENROUTER_API_KEY").is_ok_and(|v| !v.is_empty()) {
            return; // real key present locally — don't issue a live call
        }
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new())); // empty store
        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        assert!(
            matches!(err, LlmError::MissingConfig(_)),
            "expected MissingConfig, got: {err:?}"
        );
    }

    /// A `fireworks/*` request with no `FIREWORKS_API_KEY` fails with an
    /// explicit `MissingConfig` — it must NOT silently fall back to OpenRouter.
    ///
    /// Why: the shared resolver's stage-1 falls through to OpenRouter when a
    /// family key is absent; for an explicit tcode fireworks route that would be
    /// a wrong-provider misroute, so `build_adapter` turns it into a clear error.
    /// What: with an empty store (and, when present locally, a real
    /// `FIREWORKS_API_KEY` shadowing it, in which case skip), assert
    /// `MissingConfig`.
    /// Test: this test.
    #[tokio::test]
    async fn missing_fireworks_key_errors_not_falls_back() {
        if std::env::var("FIREWORKS_API_KEY").is_ok_and(|v| !v.is_empty()) {
            return; // real key present locally — the fall-back guard isn't exercised
        }
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new()));
        let req = ChatRequest {
            model: "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        match err {
            LlmError::MissingConfig(msg) => {
                assert!(
                    msg.contains("FIREWORKS_API_KEY"),
                    "unhelpful message: {msg}"
                )
            }
            other => panic!("expected MissingConfig naming FIREWORKS_API_KEY, got: {other:?}"),
        }
    }

    /// Live integration: a real OpenRouter round-trip via the shared adapter.
    ///
    /// Why: end-to-end validation (ported from the pre-#2406 `live_openrouter_call`)
    /// that tcode's transport, now on the shared adapter, still produces a
    /// non-empty reply and non-zero usage.
    /// What: reads `OPENROUTER_API_KEY`; SKIPS (does not fail) when absent.
    /// Test: `cargo test -p trusty-code -- --include-ignored live_openrouter`.
    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY; skipped in CI"]
    async fn live_openrouter_call() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("OPENROUTER_API_KEY not set — skipping live test");
            return;
        };
        if key.is_empty() {
            eprintln!("OPENROUTER_API_KEY is empty — skipping live test");
            return;
        }
        let client = OpenAiCompatClient::new();
        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![
                super::super::ChatMessage::system("You are a concise assistant."),
                super::super::ChatMessage::user("Reply with exactly the word: pong"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(16),
            tools: None,
            tool_choice: None,
            usage: None,
        };
        let resp = client.chat(&req).await.expect("chat call succeeded");
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            resp.token_usage().prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!("live openrouter ok — text: {text:?}");
    }

    /// Live integration: a real Fireworks round-trip via the shared adapter
    /// (the concrete payoff of #2406).
    ///
    /// Why: proves `fireworks/*` routing + prefix stripping + `FIREWORKS_API_KEY`
    /// resolution work against the real API.
    /// What: reads `FIREWORKS_API_KEY`; SKIPS when absent.
    /// Test: `cargo test -p trusty-code -- --include-ignored live_fireworks`.
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
        let client = OpenAiCompatClient::new();
        let req = ChatRequest {
            model: "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct".into(),
            messages: vec![
                super::super::ChatMessage::system("You are a concise assistant."),
                super::super::ChatMessage::user("Reply with exactly the word: pong"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(16),
            tools: None,
            tool_choice: None,
            usage: None,
        };
        let resp = client.chat(&req).await.expect("chat call succeeded");
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            resp.token_usage().prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!("live fireworks ok — text: {text:?}");
    }
}
