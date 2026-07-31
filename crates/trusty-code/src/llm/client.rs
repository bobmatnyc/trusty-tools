//! OpenAI-compatible inference transport, backed by the shared
//! `trusty_common::inference` adapter layer (#2406, epic #2400).
//!
//! Why: trusty-code used to hand-roll its own OpenRouter `reqwest` client. Epic
//! #2400 centralises the OpenAI-compatible HTTP mechanics (auth, the OpenRouter
//! detailed-usage directive, HTTP→error classification, response parsing) in
//! ONE shared core so every consumer shares it instead of six near-identical
//! copies. This module is tcode's thin consumer of that core: it selects the
//! provider (OpenRouter, Fireworks, Together, or AtlasCloud) by model slug, resolves the
//! credential via the shared 3-tier resolver (process env > `.env.local` >
//! secure store), builds the matching `trusty_common::inference` adapter once
//! (caching it so the underlying `reqwest` connection pool is reused across a
//! run's many turns — a bake-off latency concern), and calls it. Since #4425
//! unified trusty-code's wire types with the shared ones there is nothing left
//! to bridge — the former `super::convert` module is gone, and the only
//! per-request rewrite is the routing prefix (see [`OpenAiCompatClient::route`]).
//! Bedrock stays on its own Converse transport (`super::bedrock`, routed by
//! `super::dispatch`) — its migration into commons is #2407.
//! What: [`OpenAiCompatClient`] implements [`InferenceAdapter`] — including
//! [`InferenceAdapter::chat_stream`], which reaches the shared adapter's native
//! SSE transport and is what makes trusty-code stream (#4425). `fireworks/*`
//! slugs route to the Fireworks adapter (stripping the `fireworks/` routing
//! prefix to the provider-native model id and requiring `FIREWORKS_API_KEY`);
//! `together/*` slugs route to the Together adapter (#2494 — stripping the
//! `together/` routing prefix and requiring `TOGETHER_API_KEY`); `atlascloud/*`
//! slugs route to the AtlasCloud adapter (#2536 — stripping the `atlascloud/`
//! routing prefix and requiring `ATLASCLOUD_API_KEY`); everything else
//! routes to OpenRouter, sending the slug unchanged (identical to the pre-#2406
//! behaviour). A missing credential surfaces at `chat()` time (not
//! construction), preserving the #2245 deferred-failure contract. The base URLs
//! are overridable (`TCODE_OPENROUTER_BASE_URL` / `TCODE_FIREWORKS_BASE_URL` /
//! `TCODE_TOGETHER_BASE_URL` / `TCODE_ATLASCLOUD_BASE_URL` or
//! [`OpenAiCompatClient::with_config`]) so an offline mock server can be targeted
//! end-to-end.
//! Test: `client::tests::*` (provider selection, prefix stripping, hermetic
//! missing-credential path against an injected empty store) and the black-box
//! HTTP round-trip in `tests/inference_shared_adapter_e2e.rs`; the `#[ignore]`
//! `live_openrouter_call` / `live_fireworks_call` / `live_together_call` smokes
//! hit the real APIs.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use trusty_common::inference::{
    ChatRequest, ChatResponse, ChatStream, InferenceAdapter, InferenceError, ProviderCapabilities,
    capabilities,
    credentials::{KeyStore, default_store, resolve_key_with},
    provider_for,
    providers::{atlascloud, fireworks, openrouter, together},
    registry::ProviderId,
};

/// Env var overriding the OpenRouter API base URL (for offline mock testing /
/// self-hosted gateways). Defaults to [`openrouter::OPENROUTER_BASE_URL`].
const OPENROUTER_BASE_URL_ENV: &str = "TCODE_OPENROUTER_BASE_URL";

/// Env var overriding the Fireworks API base URL. Defaults to
/// [`fireworks::FIREWORKS_BASE_URL`].
const FIREWORKS_BASE_URL_ENV: &str = "TCODE_FIREWORKS_BASE_URL";

/// Env var overriding the Together API base URL. Defaults to
/// [`together::TOGETHER_BASE_URL`].
const TOGETHER_BASE_URL_ENV: &str = "TCODE_TOGETHER_BASE_URL";

/// Env var overriding the AtlasCloud API base URL. Defaults to
/// [`atlascloud::ATLASCLOUD_BASE_URL`].
const ATLASCLOUD_BASE_URL_ENV: &str = "TCODE_ATLASCLOUD_BASE_URL";

/// The provider name `crate::provider::provider_for` reports for `fireworks/*`
/// slugs — the single routing condition this client checks (mirroring how
/// `super::dispatch` keys Bedrock routing off the same factory).
const FIREWORKS_PROVIDER_NAME: &str = "fireworks";

/// The provider name `crate::provider::provider_for` reports for `together/*`
/// slugs — the second OpenAI-dialect routing condition this client checks
/// (#2494), keyed off the same factory as Fireworks.
const TOGETHER_PROVIDER_NAME: &str = "together";

/// The provider name `crate::provider::provider_for` reports for `atlascloud/*`
/// slugs — the third OpenAI-dialect routing condition this client checks
/// (#2536), keyed off the same factory as Fireworks and Together.
const ATLASCLOUD_PROVIDER_NAME: &str = "atlascloud";

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
    together_base: String,
    atlascloud_base: String,
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
        let together_base = std::env::var(TOGETHER_BASE_URL_ENV)
            .unwrap_or_else(|_| together::TOGETHER_BASE_URL.to_string());
        let atlascloud_base = std::env::var(ATLASCLOUD_BASE_URL_ENV)
            .unwrap_or_else(|_| atlascloud::ATLASCLOUD_BASE_URL.to_string());
        Self::with_config(
            store,
            openrouter_base,
            fireworks_base,
            together_base,
            atlascloud_base,
        )
    }

    /// Construct with an explicit store AND explicit per-provider base URLs.
    ///
    /// Why: the offline black-box e2e points each provider at a
    /// [`trusty_common::inference::test_support::MockInferenceServer`] without
    /// mutating process env (so the test stays parallel-safe).
    /// What: stores all five inputs and an empty adapter cache.
    /// Test: `tests/inference_shared_adapter_e2e.rs`.
    pub fn with_config(
        store: Box<dyn KeyStore>,
        openrouter_base: String,
        fireworks_base: String,
        together_base: String,
        atlascloud_base: String,
    ) -> Self {
        Self {
            store,
            openrouter_base,
            fireworks_base,
            together_base,
            atlascloud_base,
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
    /// [`ProviderId::Together`] iff it reports `"together"` (#2494),
    /// [`ProviderId::AtlasCloud`] iff it reports `"atlascloud"` (#2536), else
    /// [`ProviderId::OpenRouter`] (the default for every other slug).
    /// Test: `client::tests::selects_fireworks_for_prefixed_slug`,
    /// `client::tests::selects_together_for_prefixed_slug`,
    /// `client::tests::selects_atlascloud_for_prefixed_slug`,
    /// `client::tests::selects_openrouter_for_plain_slug`.
    fn provider_for_slug(model: &str) -> ProviderId {
        match crate::provider::provider_for(model).name() {
            FIREWORKS_PROVIDER_NAME => ProviderId::Fireworks,
            TOGETHER_PROVIDER_NAME => ProviderId::Together,
            ATLASCLOUD_PROVIDER_NAME => ProviderId::AtlasCloud,
            _ => ProviderId::OpenRouter,
        }
    }

    /// The provider-native wire model id for a routing slug.
    ///
    /// Why: Fireworks serves bare `accounts/fireworks/models/*` ids and Together
    /// serves bare `meta-llama/*` (etc.) ids, but tcode routes on a
    /// `fireworks/`- or `together/`-prefixed slug; the prefix is a routing
    /// artefact that must be stripped before the id is sent. OpenRouter takes the
    /// slug verbatim (identical to pre-#2406).
    /// What: strips a leading `fireworks/` for [`ProviderId::Fireworks`], a
    /// leading `together/` for [`ProviderId::Together`] (#2494), and a leading
    /// `atlascloud/` for [`ProviderId::AtlasCloud`] (#2536 — the remainder, e.g.
    /// `openai/gpt-5.6-sol`, is the AtlasCloud-native wire id); returns the slug
    /// unchanged otherwise.
    /// Test: `client::tests::fireworks_wire_model_strips_prefix`,
    /// `client::tests::together_wire_model_strips_prefix`,
    /// `client::tests::atlascloud_wire_model_strips_prefix`.
    fn wire_model(provider: ProviderId, model: &str) -> String {
        match provider {
            ProviderId::Fireworks => model
                .strip_prefix("fireworks/")
                .unwrap_or(model)
                .to_string(),
            ProviderId::Together => model.strip_prefix("together/").unwrap_or(model).to_string(),
            ProviderId::AtlasCloud => model
                .strip_prefix("atlascloud/")
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
    ) -> Result<Arc<dyn InferenceAdapter>, InferenceError> {
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
    /// [`InferenceError::MissingConfig`] — never the shared two-stage resolver's silent
    /// fall-back to OpenRouter (which would be a wrong-provider misroute), and
    /// never an OpenRouter-flavoured error when both keys happen to be absent;
    /// only once the key is confirmed does it resolve + build the Fireworks
    /// adapter. Together (#2494) and AtlasCloud (#2536) follow the identical
    /// contract against `TOGETHER_API_KEY` / `ATLASCLOUD_API_KEY` respectively.
    /// For OpenRouter, resolves on an `openrouter/` routing
    /// slug (a missing key surfaces as the resolver's own `MissingCredential`,
    /// mapped to `MissingConfig`). The resolved model slug is irrelevant to the
    /// built adapter (it sends the per-request wire model), so a fixed routing
    /// slug is used.
    /// Test: `client::tests::missing_openrouter_key_errors_at_chat_time`,
    /// `client::tests::missing_fireworks_key_errors_not_falls_back`,
    /// `client::tests::missing_together_key_errors_not_falls_back`,
    /// `client::tests::missing_atlascloud_key_errors_not_falls_back`.
    fn build_adapter(
        &self,
        provider: ProviderId,
    ) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
        match provider {
            ProviderId::Fireworks => {
                if resolve_key_with("fireworks", self.store.as_ref()).is_none() {
                    return Err(InferenceError::MissingConfig(
                        "FIREWORKS_API_KEY is required to reach a fireworks/* model. Export it \
                         (e.g. `export FIREWORKS_API_KEY=fw_...`), add it to .env.local, or set it \
                         via `tcode config keys set fireworks`."
                            .to_string(),
                    ));
                }
                // The key is present, so stage-1 of the resolver returns Fireworks.
                let resolved = provider_for("fireworks/route", self.store.as_ref())?;
                fireworks::build(&resolved, &self.fireworks_base)
            }
            ProviderId::Together => {
                if resolve_key_with("together", self.store.as_ref()).is_none() {
                    return Err(InferenceError::MissingConfig(
                        "TOGETHER_API_KEY is required to reach a together/* model. Export it \
                         (e.g. `export TOGETHER_API_KEY=tgp_...`), add it to .env.local, or set \
                         it via `tcode config keys set together`."
                            .to_string(),
                    ));
                }
                // The key is present, so stage-1 of the resolver returns Together.
                let resolved = provider_for("together/route", self.store.as_ref())?;
                together::build(&resolved, &self.together_base)
            }
            ProviderId::AtlasCloud => {
                if resolve_key_with("atlascloud", self.store.as_ref()).is_none() {
                    return Err(InferenceError::MissingConfig(
                        "ATLASCLOUD_API_KEY is required to reach an atlascloud/* model. Export it \
                         (e.g. `export ATLASCLOUD_API_KEY=ac_...`), add it to .env.local, or set \
                         it via `tcode config keys set atlascloud`."
                            .to_string(),
                    ));
                }
                // The key is present, so stage-1 of the resolver returns AtlasCloud.
                let resolved = provider_for("atlascloud/route", self.store.as_ref())?;
                atlascloud::build(&resolved, &self.atlascloud_base)
            }
            _ => {
                let resolved = provider_for("openrouter/route", self.store.as_ref())?;
                openrouter::build(&resolved, &self.openrouter_base)
            }
        }
    }

    /// Resolve the adapter and the provider-native request for one call.
    ///
    /// Why (#4425): [`InferenceAdapter::chat`] and [`InferenceAdapter::chat_stream`]
    /// make the IDENTICAL routing, credential, and prefix-stripping decisions and
    /// differ only in which adapter method they finally invoke. Sharing one
    /// resolution step is what guarantees a model cannot be routed to one
    /// provider when blocking and another when streaming.
    /// What: selects the provider from `req.model`, gets (or builds) its cached
    /// adapter, and returns it alongside a copy of `req` whose `model` is the
    /// provider-native wire id (routing prefix stripped). The clone is one
    /// shallow copy per call — the same cost the pre-#4425 `to_shared_request`
    /// bridge paid.
    /// Test: `client::tests::*` and `tests/inference_shared_adapter_e2e.rs`.
    async fn route(
        &self,
        req: &ChatRequest,
    ) -> Result<(Arc<dyn InferenceAdapter>, ChatRequest), InferenceError> {
        let provider = Self::provider_for_slug(&req.model);
        let adapter = self.adapter_for(provider).await?;
        let mut routed = req.clone();
        routed.model = Self::wire_model(provider, &req.model);
        Ok((adapter, routed))
    }
}

impl Default for OpenAiCompatClient {
    /// Delegates to [`OpenAiCompatClient::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Delegating [`InferenceAdapter`] impl (#4425).
///
/// Why: this transport IS a shared-trait implementation, not a private
/// abstraction — `DispatchingLlmClient` and every trusty-code call site reach
/// it through `dyn InferenceAdapter`. It routes per REQUEST across four
/// OpenAI-dialect providers, so the trait's per-adapter identity methods are
/// answered for the routing default rather than for one fixed backend.
/// What: `chat` and `chat_stream` both go through [`Self::route`], so they can
/// never disagree about provider selection; `chat_stream` reaches the shared
/// OpenAI-compat adapter's NATIVE SSE transport (#3696 Gap B), which is what
/// makes trusty-code's OpenRouter path stream token-by-token.
/// Test: `client::tests::*`, `tests/inference_shared_adapter_e2e.rs`.
#[async_trait]
impl InferenceAdapter for OpenAiCompatClient {
    /// The routing default's name; see [`Self::capabilities`] for why this is
    /// not a per-request answer.
    fn name(&self) -> &str {
        ProviderId::OpenRouter.as_str()
    }

    /// Capabilities for the routing DEFAULT (OpenRouter).
    ///
    /// Why: the shared trait's `capabilities()` takes no model argument, but
    /// this transport serves four providers chosen per request, so there is no
    /// model-free answer that is right for all of them. OpenRouter is the
    /// default branch of [`Self::provider_for_slug`] — the provider a slug with
    /// no routing prefix actually reaches — so it is the honest answer to the
    /// model-free question. Callers holding a slug must ask
    /// [`Self::capabilities_for`] instead, which routes.
    /// What: `capabilities(ProviderId::OpenRouter)`.
    /// Test: `client::tests::capabilities_report_openrouter_default`.
    fn capabilities(&self) -> &ProviderCapabilities {
        capabilities(ProviderId::OpenRouter)
    }

    /// Capabilities for the provider `model` ACTUALLY routes to (#4425).
    ///
    /// Why: this transport is multi-provider by design, and OpenRouter's
    /// profile is wrong for the other three in ways that reach the wire —
    /// `detailed_usage_accounting` (the OpenRouter-only usage directive),
    /// `prompt_caching` (`cache_control` breakpoints), `vision`, and the
    /// context-window fallback tier all differ. Answering every capability
    /// question with OpenRouter's profile made "one adapter, many providers"
    /// silently wrong for `fireworks/*`, `together/*`, and `atlascloud/*`.
    /// What: resolves `model` through [`Self::provider_for_slug`] — the SAME
    /// gate [`Self::route`] uses to pick the transport — so capabilities can
    /// never disagree with where the request is sent.
    /// Test: `client::tests::capabilities_for_follows_slug_routing`.
    fn capabilities_for(&self, model: &str) -> &ProviderCapabilities {
        capabilities(Self::provider_for_slug(model))
    }

    /// Route + issue one blocking chat call through the shared adapter.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let (adapter, routed) = self.route(req).await?;
        adapter.chat(&routed).await
    }

    /// Route + issue one STREAMING chat call through the shared adapter.
    ///
    /// Why (#4425): this override is the whole reason trusty-code can stream —
    /// the shared `OpenAiCompatAdapter` implements native SSE, and inheriting
    /// the trait's buffered default here would have silently thrown that away.
    /// What: same routing as [`Self::chat`], then the adapter's `chat_stream`.
    /// A `stream=true` handshake failure surfaces synchronously as `Err`, so a
    /// caller may choose to retry non-streaming rather than see a half-open
    /// stream.
    /// Test: `tests/inference_shared_adapter_e2e.rs` streaming round-trip.
    async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        let (adapter, routed) = self.route(req).await?;
        adapter.chat_stream(&routed).await
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

    /// A `together/*` slug selects Together.
    ///
    /// Why: this is the routing decision that makes Together reachable (#2494).
    /// What: assert the provider selection for a together slug.
    /// Test: this test.
    #[test]
    fn selects_together_for_prefixed_slug() {
        assert_eq!(
            OpenAiCompatClient::provider_for_slug(
                "together/meta-llama/Llama-3.3-70B-Instruct-Turbo"
            ),
            ProviderId::Together
        );
    }

    /// An `atlascloud/*` slug selects AtlasCloud, including the nested
    /// `atlascloud/openai/gpt-5.6-sol` form.
    ///
    /// Why: this is the routing decision that makes AtlasCloud reachable (#2536),
    /// and the nested `openai/`-shaped model id must not re-home to OpenRouter.
    /// What: assert the provider selection for the nested and a bare atlascloud slug.
    /// Test: this test.
    #[test]
    fn selects_atlascloud_for_prefixed_slug() {
        assert_eq!(
            OpenAiCompatClient::provider_for_slug("atlascloud/openai/gpt-5.6-sol"),
            ProviderId::AtlasCloud
        );
        assert_eq!(
            OpenAiCompatClient::provider_for_slug("atlascloud/deepseek-v3"),
            ProviderId::AtlasCloud
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

    /// The Together wire model strips the `together/` routing prefix down to the
    /// bare Together catalog slug.
    ///
    /// Why: Together 404s on the prefixed id; the prefix is a tcode routing
    /// artefact only (#2494).
    /// What: assert the stripped slug.
    /// Test: this test.
    #[test]
    fn together_wire_model_strips_prefix() {
        assert_eq!(
            OpenAiCompatClient::wire_model(
                ProviderId::Together,
                "together/meta-llama/Llama-3.3-70B-Instruct-Turbo"
            ),
            "meta-llama/Llama-3.3-70B-Instruct-Turbo"
        );
    }

    /// The AtlasCloud wire model strips the `atlascloud/` routing prefix down to
    /// the AtlasCloud-native slug — including the nested `openai/gpt-5.6-sol`
    /// remainder (the `atlascloud/` segment is the ONLY routing artefact stripped)
    /// and the bare `deepseek-v3` remainder.
    ///
    /// Why: AtlasCloud 404s on the `atlascloud/`-prefixed id; the prefix is a
    /// tcode routing artefact only, but the model id may itself be `vendor/model`
    /// shaped, so only the leading `atlascloud/` segment must be removed (#2536).
    /// What: assert the stripped slug for both the nested and bare forms.
    /// Test: this test.
    #[test]
    fn atlascloud_wire_model_strips_prefix() {
        assert_eq!(
            OpenAiCompatClient::wire_model(ProviderId::AtlasCloud, "atlascloud/openai/gpt-5.6-sol"),
            "openai/gpt-5.6-sol"
        );
        assert_eq!(
            OpenAiCompatClient::wire_model(ProviderId::AtlasCloud, "atlascloud/deepseek-v3"),
            "deepseek-v3"
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
            stop: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        assert!(
            matches!(err, InferenceError::MissingConfig(_)),
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
            stop: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        match err {
            InferenceError::MissingConfig(msg) => {
                assert!(
                    msg.contains("FIREWORKS_API_KEY"),
                    "unhelpful message: {msg}"
                )
            }
            other => panic!("expected MissingConfig naming FIREWORKS_API_KEY, got: {other:?}"),
        }
    }

    /// A `together/*` request with no `TOGETHER_API_KEY` fails with an explicit
    /// `MissingConfig` — it must NOT silently fall back to OpenRouter (#2494).
    ///
    /// Why: the shared resolver's stage-1 falls through to OpenRouter when a
    /// family key is absent; for an explicit tcode together route that would be a
    /// wrong-provider misroute, so `build_adapter` turns it into a clear error —
    /// the same contract as Fireworks.
    /// What: with an empty store (and, when present locally, a real
    /// `TOGETHER_API_KEY` shadowing it, in which case skip), assert
    /// `MissingConfig` naming `TOGETHER_API_KEY`.
    /// Test: this test.
    #[tokio::test]
    async fn missing_together_key_errors_not_falls_back() {
        if std::env::var("TOGETHER_API_KEY").is_ok_and(|v| !v.is_empty()) {
            return; // real key present locally — the fall-back guard isn't exercised
        }
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new()));
        let req = ChatRequest {
            model: "together/meta-llama/Llama-3.3-70B-Instruct-Turbo".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
            stop: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        match err {
            InferenceError::MissingConfig(msg) => {
                assert!(msg.contains("TOGETHER_API_KEY"), "unhelpful message: {msg}")
            }
            other => panic!("expected MissingConfig naming TOGETHER_API_KEY, got: {other:?}"),
        }
    }

    /// An `atlascloud/*` request with no `ATLASCLOUD_API_KEY` fails with an
    /// explicit `MissingConfig` — it must NOT silently fall back to OpenRouter
    /// (#2536).
    ///
    /// Why: the shared resolver's stage-1 falls through to OpenRouter when a
    /// family key is absent; for an explicit tcode atlascloud route that would be
    /// a wrong-provider misroute, so `build_adapter` turns it into a clear error —
    /// the same contract as Fireworks and Together.
    /// What: with an empty store (and, when present locally, a real
    /// `ATLASCLOUD_API_KEY` shadowing it, in which case skip), assert
    /// `MissingConfig` naming `ATLASCLOUD_API_KEY`.
    /// Test: this test.
    #[tokio::test]
    async fn missing_atlascloud_key_errors_not_falls_back() {
        if std::env::var("ATLASCLOUD_API_KEY").is_ok_and(|v| !v.is_empty()) {
            return; // real key present locally — the fall-back guard isn't exercised
        }
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new()));
        let req = ChatRequest {
            model: "atlascloud/openai/gpt-5.6-sol".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
            stop: None,
        };
        let err = client
            .chat(&req)
            .await
            .expect_err("must fail without a key");
        match err {
            InferenceError::MissingConfig(msg) => {
                assert!(
                    msg.contains("ATLASCLOUD_API_KEY"),
                    "unhelpful message: {msg}"
                )
            }
            other => panic!("expected MissingConfig naming ATLASCLOUD_API_KEY, got: {other:?}"),
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
            stop: None,
        };
        let resp = client.chat(&req).await.expect("chat call succeeded");
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            crate::llm::token_usage(&resp).prompt_tokens > 0,
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
            stop: None,
        };
        let resp = client.chat(&req).await.expect("chat call succeeded");
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            crate::llm::token_usage(&resp).prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!("live fireworks ok — text: {text:?}");
    }

    /// Live integration: a real Together round-trip via the shared adapter
    /// (the concrete payoff of #2494).
    ///
    /// Why: proves `together/*` routing + prefix stripping + `TOGETHER_API_KEY`
    /// resolution work against the real API.
    /// What: reads `TOGETHER_API_KEY`; SKIPS when absent.
    /// Test: `cargo test -p trusty-code -- --include-ignored live_together`.
    #[tokio::test]
    #[ignore = "requires TOGETHER_API_KEY; skipped in CI"]
    async fn live_together_call() {
        let Ok(key) = std::env::var("TOGETHER_API_KEY") else {
            eprintln!("TOGETHER_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("TOGETHER_API_KEY is empty — skipping live test");
            return;
        }
        let client = OpenAiCompatClient::new();
        let req = ChatRequest {
            model: "together/meta-llama/Llama-3.3-70B-Instruct-Turbo".into(),
            messages: vec![
                super::super::ChatMessage::system("You are a concise assistant."),
                super::super::ChatMessage::user("Reply with exactly the word: pong"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(16),
            tools: None,
            tool_choice: None,
            usage: None,
            stop: None,
        };
        let resp = client.chat(&req).await.expect("chat call succeeded");
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            crate::llm::token_usage(&resp).prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!("live together ok — text: {text:?}");
    }

    /// Capabilities report the OpenRouter routing default (#4425).
    ///
    /// Why: pins the documented answer to the shared trait's model-free
    /// `capabilities()` question for a transport that routes per request, so a
    /// change to it is deliberate rather than silent drift.
    /// What: assert the returned profile's id is OpenRouter.
    /// Test: this test.
    #[test]
    fn capabilities_report_openrouter_default() {
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new()));
        assert_eq!(client.capabilities().id, ProviderId::OpenRouter);
        assert_eq!(client.name(), "openrouter");
    }

    /// `capabilities_for` follows the SAME slug routing `route()` does (#4425).
    ///
    /// Why: this transport serves four providers picked per request. Answering
    /// every capability question with OpenRouter's profile — as it did before
    /// #4425 — reports `detailed_usage_accounting: true` and
    /// `prompt_caching: true` for a Fireworks turn, neither of which Fireworks
    /// honours, and hands the compaction budget OpenRouter's 200K tier for a
    /// 128K backend. #4426 builds Bedrock on this exact surface.
    /// What: for one representative slug per routed provider, assert
    /// `capabilities_for(slug).id` equals the provider `provider_for_slug`
    /// selects — i.e. the two can never disagree — and spot-check that the
    /// answer actually DIFFERS from OpenRouter's for a non-OpenRouter slug.
    /// Test: this test.
    #[test]
    fn capabilities_for_follows_slug_routing() {
        let client = OpenAiCompatClient::with_store(Box::new(MemoryKeyStore::new()));
        for (slug, expected) in [
            ("anthropic/claude-sonnet-4-5", ProviderId::OpenRouter),
            ("openai/gpt-4o-mini", ProviderId::OpenRouter),
            (
                "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct",
                ProviderId::Fireworks,
            ),
            (
                "together/meta-llama/Llama-3.3-70B-Instruct-Turbo",
                ProviderId::Together,
            ),
            ("atlascloud/openai/gpt-5.6-sol", ProviderId::AtlasCloud),
        ] {
            assert_eq!(
                client.capabilities_for(slug).id,
                expected,
                "capabilities for {slug} must follow its routing target"
            );
            assert_eq!(
                client.capabilities_for(slug).id,
                OpenAiCompatClient::provider_for_slug(slug),
                "capabilities and transport routing must agree for {slug}"
            );
        }

        // The behavioural teeth: a Fireworks slug must NOT inherit OpenRouter's
        // usage-directive / caching / context-window profile.
        let fireworks = client.capabilities_for("fireworks/accounts/x/models/y");
        assert!(!fireworks.detailed_usage_accounting);
        assert!(!fireworks.prompt_caching);
        assert_eq!(fireworks.max_context_window, 128_000);
        assert_eq!(
            client.context_window("fireworks/accounts/x/models/y"),
            128_000
        );
    }
}
