//! `GET /api/models` — inference provider catalog (#3243, epic #3052).
//!
//! Why: The Assistant-MVP model/provider picker (Brief 5 item I2) needs to
//! populate its UI from live data instead of a hardcoded model list. The
//! unified `trusty_common::inference::registry` is already compiled into
//! this binary (transitively, via the `config-cli` feature on the
//! `trusty-common` dependency) but was never exposed over HTTP — this route
//! is the seam. Credential presence must never leak the key itself: the
//! response only ever carries a boolean, never a value or a source path.
//! What: `get_models` walks `registry::all()` and, for each provider, reports
//! its id, default model, context window, whether a credential resolves for
//! it (`credential_configured`), and whether the *legacy* chat dispatch
//! (`crate::llm::adapter::adapter_for_model`) can reach it today
//! (`reachable_today`) — see [`reachable_today`] for the exact set and the
//! #2410 migration this field tracks. A synthetic `local` entry reports
//! Ollama's cached availability probe alongside its default model.
//! Test: `super::tests::models` — response shape, the ollama-absent path,
//! the zero-credentials-configured path, and (the safety-critical one) that
//! no credential VALUE ever appears in the serialized body even when a key
//! is configured via env var.

use axum::Json;
use serde::Serialize;
use trusty_common::inference::credentials::resolve_key;
use trusty_common::inference::registry::{self, ProviderId};

/// One provider entry in the `/api/models` catalog.
///
/// Why: The UI's picker renders one row per provider; this is exactly the
/// shape it needs, no more.
/// What: `provider_id` is [`ProviderId::as_str`]; `default_model` /
/// `context_window` come straight from the registry's seed table;
/// `credential_configured` and `reachable_today` are computed — see their
/// producing functions for the exact rules.
/// Test: `super::tests::models::models_response_has_expected_shape`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelProviderEntry {
    pub provider_id: &'static str,
    pub default_model: &'static str,
    pub context_window: usize,
    pub credential_configured: bool,
    pub reachable_today: bool,
}

/// The synthetic local-Ollama entry.
///
/// Why: Ollama isn't a [`ProviderId`] — it's a separate legacy fast-path
/// (`crate::local_inference`) with its own availability probe rather than a
/// credential. Modeling it as a sibling struct (instead of shoehorning it
/// into [`ModelProviderEntry`]) keeps `credential_configured` meaningful
/// (Ollama has none) instead of overloading it as "available".
/// What: `available` is the cached `is_ollama_available_cached` probe result;
/// `default_model` mirrors `LocalInferenceConfig::default().model`.
/// Test: `super::tests::models::ollama_absent_reports_available_false`.
#[derive(Debug, Clone, Serialize)]
pub struct LocalModelEntry {
    pub provider_id: &'static str,
    pub default_model: String,
    pub available: bool,
    pub reachable_today: bool,
}

/// Full `GET /api/models` response body.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsCatalogResponse {
    pub providers: Vec<ModelProviderEntry>,
    pub local: LocalModelEntry,
}

/// Whether the *current legacy chat dispatch* can reach `id` today.
///
/// Why: The unified registry (#2400) already knows about seven providers, but
/// `crate::llm::adapter::adapter_for_model` — the code path actual chat turns
/// run through — only resolves OpenRouter (the default fallback), Anthropic
/// (direct, when `ANTHROPIC_API_KEY` is set), and Bedrock (`bedrock/` prefix)
/// to a working adapter today. OpenAI/Fireworks/Together/AtlasCloud DO fall
/// through a branch in that dispatch — but only as far as the OpenRouter
/// endpoint, which then receives each provider's bare (un-prefixed) model-id
/// slug. OpenRouter doesn't recognize those bare slugs as its own routes, so
/// the call fails: the gap is a model-ID / endpoint mismatch, not a missing
/// branch. Surfacing this distinction keeps the picker honest instead of
/// implying every registry entry is immediately usable.
/// What: `true` for OpenRouter, Bedrock, Anthropic; `false` for the
/// providers whose legacy dispatch falls through to OpenRouter with an
/// unresolvable model-id slug. Flip an entry to `true` here once its legacy
/// dispatch branch resolves to the correct provider-native endpoint (tracked
/// by #2410); this function is the one place that needs to change.
/// Test: `super::tests::models::reachable_today_matches_documented_set`.
fn reachable_today(id: ProviderId) -> bool {
    matches!(
        id,
        ProviderId::OpenRouter | ProviderId::Bedrock | ProviderId::Anthropic
    )
}

/// Whether `id`'s credential resolves via the 3-tier resolver, without ever
/// touching the resolved value.
///
/// Why: Bedrock authenticates via the ambient AWS credential chain, not a
/// resolver-managed key — [`ProviderId::credential_name`] returns `None` for
/// it by design (mirrored from `configurator::provider_for`, which resolves
/// Bedrock with no key unconditionally). Reporting it as "configured" avoids
/// implying a missing/broken credential for a provider that has none to
/// configure. Every keyed provider probes [`resolve_key`] and keeps only the
/// `is_some()` boolean — the `String` key itself is dropped immediately and
/// never referenced again, so it cannot leak into the response by accident.
/// What: `true`/`false` per provider; never the key.
/// Test: `super::tests::models::credential_values_never_serialized`,
/// `super::tests::models::zero_credentials_configured_is_stable`.
fn credential_configured(id: ProviderId) -> bool {
    match id.credential_name() {
        Some(name) => resolve_key(name).is_some(),
        None => true, // Bedrock: AWS credential chain, no stored key to probe.
    }
}

/// The default Ollama base URL, honoring the same `OLLAMA_HOST` override the
/// rest of the local-inference stack uses.
///
/// Why: Mirrors `llm::adapter::impls`'s / `repl::ollama`'s host resolution;
/// duplicated here (rather than imported) because both source modules keep
/// it private to their own tree — this is the same one-line default those
/// two already carry.
fn ollama_host() -> String {
    std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string())
}

/// `GET /api/models` — the inference provider catalog.
///
/// Why: Single entry point the Assistant-MVP model picker calls once at
/// startup (and on refresh) to learn which providers exist, which are
/// credentialed, and which are actually reachable through today's chat
/// dispatch — all without hardcoding a model list in the frontend.
/// What: Maps [`registry::all`] to [`ModelProviderEntry`], then appends the
/// synthetic Ollama [`LocalModelEntry`] using the memoized availability probe
/// (`crate::local_inference::is_ollama_available_cached` — cheap after the
/// first call, per-process cached, so this route never pays a fresh 500ms
/// TCP probe on every poll).
/// Test: `super::tests::models::*`.
pub(super) async fn get_models() -> Json<ModelsCatalogResponse> {
    let providers = registry::all()
        .iter()
        .map(|cap| ModelProviderEntry {
            provider_id: cap.id.as_str(),
            default_model: cap.default_model,
            context_window: cap.max_context_window,
            credential_configured: credential_configured(cap.id),
            reachable_today: reachable_today(cap.id),
        })
        .collect();

    let host = ollama_host();
    let available = crate::local_inference::is_ollama_available_cached(&host).await;
    let local = LocalModelEntry {
        provider_id: "ollama",
        default_model: crate::mcp::LocalInferenceConfig::default().model,
        available,
        // Ollama is already wired into the legacy dispatch (`ollama/` model
        // prefix, see `llm::adapter::adapter_for_model`) — reachable whenever
        // it's actually running, independent of the #2410 migration.
        reachable_today: true,
    };

    Json(ModelsCatalogResponse { providers, local })
}
