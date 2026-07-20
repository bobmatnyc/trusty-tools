//! Shared-inference client for trusty-agents' OpenRouter traffic (#2410,
//! epic #2400, Step 2 — flag-gated, default OFF).
//!
//! Why: Step 1 (`crate::llm::inference_bridge`) built the pure conversion
//! seam; this module is the thin consumer that turns it into a real HTTP
//! transport, mirroring `trusty-code`'s `OpenAiCompatClient`
//! (`crates/trusty-code/src/llm/client.rs`) so the two crates share the
//! same shape of adoption. Construction never touches credentials — the
//! #2245 deferred-failure contract — so a caller can build an
//! [`InferenceClient`] unconditionally and only pay the "is a key
//! configured" cost inside [`InferenceClient::chat`].
//!
//! What: [`InferenceClient`] owns a `Configurator` (populated via
//! `register_default_factories`, `crates/trusty-common/src/inference/providers/mod.rs:45`),
//! a credential `KeyStore`, and a `ProviderId`-keyed adapter cache
//! (`Arc<dyn InferenceAdapter>`) so the underlying `reqwest` connection pool
//! is reused across turns rather than rebuilt per call.
//!
//! 🔴 Step 1+2 scope is OpenRouter ONLY. [`InferenceClient::adapter`] always
//! resolves against the FIXED routing slug [`OPENROUTER_ROUTING_SLUG`] —
//! **never** the caller's real model slug. This is deliberate, not an
//! oversight: `register_default_factories` also registers the Anthropic
//! **direct** factory, and `trusty_common::inference::provider_for` resolves
//! an `"anthropic/…"`-prefixed slug to `ProviderId::Anthropic` whenever an
//! `ANTHROPIC_API_KEY` happens to be configured (a very common local-dev
//! setup per this crate's own `llm::credentials` priority order). If this
//! client ever resolved on the real (already-OpenRouter-qualified, per
//! `llm::credentials::qualify_openrouter_model`) model slug instead of the
//! fixed routing slug, an anthropic-prefixed model would silently defect
//! from OpenRouter onto the Anthropic-direct adapter — exactly the migration
//! the owner explicitly ruled out of scope for #2410 (see the epic's SCOPE
//! BOUNDARIES: `llm::anthropic_native` is untouched). The fixed routing slug
//! makes that misroute structurally impossible: `provider_for` resolves
//! `"openrouter/route"`'s prefix to `ProviderId::OpenRouter` in BOTH
//! resolution stages, regardless of which other credentials are configured
//! (see `openrouter_route_never_resolves_to_anthropic_even_when_anthropic_key_present`
//! in `tests.rs`). Per-slug provider routing (Fireworks) is epic Step 3, a
//! later PR — it will need a different resolution strategy than this one.
//!
//! [`shared_inference_enabled`] is the single source of truth for the
//! `TAGENT_INFERENCE_SHARED` env flag `tool_loop::turn::dispatch_turn` gates
//! on; default OFF, so the legacy `async-openai` typed path is unchanged
//! unless a caller opts in.
//!
//! Test: `inference_client::tests::*`; the offline black-box wire-shape proof
//! lives in `crates/trusty-agents/tests/inference_shared_adapter_e2e.rs`.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tokio::sync::Mutex;
use trusty_common::inference::{
    ChatRequest, ChatResponse, Configurator, InferenceAdapter, InferenceError, ResolvedProvider,
    credentials::{KeyStore, default_store},
    providers::openrouter,
    registry::ProviderId,
};

/// Env var overriding the OpenRouter base URL. Reuses the SAME variable the
/// legacy path already honours (`llm::adapter::openrouter_endpoint`,
/// `llm::helpers::create_client`) so a single override affects both paths
/// identically — the byte-parity property the offline e2e test relies on.
/// Defaults to [`openrouter::OPENROUTER_BASE_URL`] when unset.
const OPENROUTER_BASE_URL_ENV: &str = "OPENROUTER_BASE_URL";

/// Env var gating the shared-inference OpenRouter path. Default OFF — unset
/// or any value other than `"1"` keeps the legacy `async-openai` typed path.
pub const SHARED_INFERENCE_ENV: &str = "TAGENT_INFERENCE_SHARED";

/// Fixed routing slug forcing `provider_for` to resolve `ProviderId::OpenRouter`
/// regardless of the caller's real model slug. See the module doc for why
/// this must never be the request's own model.
const OPENROUTER_ROUTING_SLUG: &str = "openrouter/route";

/// Whether the shared-inference seam is enabled for this process.
///
/// Why: single source of truth so `dispatch_turn` and any future call site
/// agree on the flag name and the exact value that turns it on.
/// What: `true` iff `TAGENT_INFERENCE_SHARED` is exactly `"1"`.
/// Test: `shared_inference_enabled_reads_flag`.
pub fn shared_inference_enabled() -> bool {
    std::env::var(SHARED_INFERENCE_ENV).is_ok_and(|v| v == "1")
}

/// OpenRouter transport backed by the shared `trusty_common::inference`
/// adapter layer.
///
/// Why: one place that owns "build the OpenRouter adapter once, cache it,
/// reuse its connection pool" so `dispatch_turn`'s shared branch stays a
/// one-line call.
/// What: holds the `Configurator` (factory registry), the `KeyStore`, and a
/// lazily-populated adapter cache keyed by `ProviderId` (mirroring
/// `trusty-code`'s cache shape even though Step 1+2 only ever populates the
/// `OpenRouter` entry).
/// Test: `tests::*`.
pub struct InferenceClient {
    configurator: Configurator,
    store: Box<dyn KeyStore>,
    adapters: Mutex<HashMap<ProviderId, Arc<dyn InferenceAdapter>>>,
}

impl std::fmt::Debug for InferenceClient {
    /// Redacting `Debug`: the `KeyStore` and built adapters hold credential
    /// material, so this prints only the opaque type name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceClient").finish_non_exhaustive()
    }
}

impl InferenceClient {
    /// Construct with the process-default secure store and the real
    /// (production-URL) provider factories.
    ///
    /// Why: the production entry point `dispatch_turn`'s shared branch calls.
    /// What: `register_default_factories` into a fresh `Configurator`, paired
    /// with `default_store()`. Never touches credentials — construction
    /// cannot fail on a missing key.
    /// Test: `constructs_without_touching_credentials`.
    pub fn new() -> Self {
        Self::with_store(default_store())
    }

    /// Construct with an explicit `KeyStore`, using the real
    /// `register_default_factories` with the OpenRouter entry's base URL
    /// re-pointed at the `OPENROUTER_BASE_URL` override when set.
    ///
    /// Why: lets a caller inject a hermetic store (e.g. `MemoryKeyStore`)
    /// while still exercising the real provider factories, AND — by honouring
    /// the same env override the legacy `async-openai` client already reads —
    /// lets the offline e2e test point BOTH the legacy and shared paths at one
    /// mock server with a single env var, without mutating the production
    /// `openrouter::factory` registration itself.
    /// What: registers every default factory, then `Configurator::register`
    /// replaces just the `OpenRouter` entry with a closure using
    /// [`openrouter::build`] against the resolved base URL (env override or
    /// [`openrouter::OPENROUTER_BASE_URL`]).
    /// Test: `missing_openrouter_key_errors_at_chat_time`.
    pub fn with_store(store: Box<dyn KeyStore>) -> Self {
        let mut configurator = Configurator::new();
        trusty_common::inference::register_default_factories(&mut configurator);
        let base_url = std::env::var(OPENROUTER_BASE_URL_ENV)
            .unwrap_or_else(|_| openrouter::OPENROUTER_BASE_URL.to_string());
        configurator.register(
            ProviderId::OpenRouter,
            Box::new(move |resolved: &ResolvedProvider| openrouter::build(resolved, &base_url)),
        );
        Self::with_configurator(configurator, store)
    }

    /// Construct with a caller-supplied `Configurator` AND `KeyStore`.
    ///
    /// Why: the offline e2e test substitutes a `Configurator` whose
    /// `OpenRouter` factory points at a loopback mock instead of the real
    /// `https://openrouter.ai/api/v1`, without mutating process env or
    /// touching the production registry.
    /// What: stores both and starts with an empty adapter cache.
    /// Test: `crates/trusty-agents/tests/inference_shared_adapter_e2e.rs`.
    pub fn with_configurator(configurator: Configurator, store: Box<dyn KeyStore>) -> Self {
        Self {
            configurator,
            store,
            adapters: Mutex::new(HashMap::new()),
        }
    }

    /// Get (or lazily build + cache) the OpenRouter adapter.
    ///
    /// Why: building an adapter constructs a `reqwest::Client`; doing that
    /// once and reusing it preserves connection pooling across a run's many
    /// turns, mirroring `trusty-code`'s `OpenAiCompatClient::adapter_for`.
    /// What: resolves + builds against the FIXED [`OPENROUTER_ROUTING_SLUG`]
    /// (see module doc) on a cache miss; a build failure is returned, not
    /// cached, so a later turn can retry once the credential is fixed.
    /// Test: `adapter_is_cached_across_calls`,
    /// `openrouter_route_never_resolves_to_anthropic_even_when_anthropic_key_present`.
    async fn adapter(&self) -> Result<Arc<dyn InferenceAdapter>, InferenceError> {
        {
            let cache = self.adapters.lock().await;
            if let Some(adapter) = cache.get(&ProviderId::OpenRouter) {
                return Ok(adapter.clone());
            }
        }
        let built: Arc<dyn InferenceAdapter> = Arc::from(
            self.configurator
                .build(OPENROUTER_ROUTING_SLUG, self.store.as_ref())?,
        );
        let mut cache = self.adapters.lock().await;
        Ok(cache.entry(ProviderId::OpenRouter).or_insert(built).clone())
    }

    /// Issue one chat call through the shared OpenRouter adapter.
    ///
    /// Why: the single method `dispatch_turn`'s shared branch calls; all
    /// adapter selection/caching/credential resolution lives behind it.
    /// What: gets the cached (or freshly-built) OpenRouter adapter and
    /// forwards `req` to it verbatim — the caller has already applied
    /// `llm::credentials::qualify_openrouter_model` and the
    /// `inference_bridge` conversions.
    /// Test: `crates/trusty-agents/tests/inference_shared_adapter_e2e.rs`.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let adapter = self.adapter().await?;
        adapter.chat(req).await
    }
}

impl Default for InferenceClient {
    /// Delegates to [`InferenceClient::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Process-wide shared client, mirroring `http::http_client()`'s
/// once-per-process reuse pattern (MIN-2 / #98) so the shared branch's
/// `reqwest` connection pool is reused across every turn of every agent in
/// this process, not rebuilt per call.
static SHARED_CLIENT: OnceLock<InferenceClient> = OnceLock::new();

/// Hand out the process-wide [`InferenceClient`], initializing on first call.
///
/// Why: `tool_loop::turn::dispatch_turn` calls this once per turn when the
/// flag is on; a fresh client per call would rebuild the `reqwest::Client`
/// (and its connection pool) every turn, defeating keep-alive.
/// What: returns a `'static` reference to the lazily-built default client.
/// Test: `shared_client_returns_same_instance`.
pub fn shared_client() -> &'static InferenceClient {
    SHARED_CLIENT.get_or_init(InferenceClient::new)
}
