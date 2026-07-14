//! Inference resolution for the Layer-3 portfolio manager (WI-3/WI-4, #2580/#2581).
//!
//! Why: DOC-36 §3.3 requires every manager LLM call (`/digest`'s narrative pass,
//! `/chat`'s conversational loop) to go through the unified
//! `trusty_common::inference::InferenceAdapter` (epic #2400) — never a bespoke
//! client. This module is the single seam that turns "the daemon's configured
//! provider" into a live `Arc<dyn InferenceAdapter>` for those handlers, and it is
//! the seam the hermetic CI suite (#2584) swaps for a `ScriptedAdapter` /
//! `MockInferenceServer`-backed adapter so no live provider key or network is ever
//! required. Resolution mirrors the commons conventions: a model slug plus the
//! two-stage [`trusty_common::inference::Configurator`]/`provider_for` credential
//! ladder over [`default_store`], with a documented configurable default model.
//! What: [`ManagerInference`] owns either a production credential-backed
//! [`Configurator`] + [`KeyStore`] (built once at provision) or a fixed injected
//! adapter (tests / future hot-swap); [`ManagerInference::resolve`] hands back the
//! `(model, adapter)` pair or a typed [`InferenceUnavailable`] degrade. The model
//! is read from [`MANAGER_MODEL_ENV`] (falling back to [`FALLBACK_MODEL_ENV`] then
//! [`DEFAULT_MANAGER_MODEL`]) — the documented config key for the manager persona.
//! Test: `resolve_reports_no_provider_when_unconfigured`,
//! `resolve_returns_injected_adapter`, `manager_model_env_precedence` in
//! `inference_tests.rs`, plus the HTTP-level digest/chat coverage in
//! `tests/manager_inference.rs`.

use std::sync::{Arc, Mutex};

use trusty_common::inference::credentials::{KeyStore, default_store};
use trusty_common::inference::{
    Configurator, InferenceAdapter, InferenceError, register_default_factories,
};

/// Environment key selecting the model slug for the manager's digest/chat calls.
///
/// Why: DOC-36 §3.3 leaves the concrete model configurable; the manager persona
/// wants its own knob (a portfolio digest can prefer a cheaper/faster model than
/// a per-session summariser) without disturbing the fleet-wide
/// [`FALLBACK_MODEL_ENV`]. Pinning a single named key makes the choice
/// documentable (see `docs/reference/environment-variables.md`) and testable.
/// What: the primary env var read by [`resolve_manager_model`].
/// Test: `manager_model_env_precedence`.
pub const MANAGER_MODEL_ENV: &str = "TRUSTY_MANAGER_MODEL";

/// Fleet-wide fallback model key, shared with the activity classifier.
///
/// Why: when an operator has already set `TRUSTY_LLM_MODEL` for the rest of the
/// stack, the manager should honour it rather than force a second variable —
/// mirroring `DaemonState::activity_monitor`'s own `TRUSTY_LLM_MODEL` default.
/// What: the secondary env var read when [`MANAGER_MODEL_ENV`] is unset.
/// Test: `manager_model_env_precedence`.
pub const FALLBACK_MODEL_ENV: &str = "TRUSTY_LLM_MODEL";

/// The default manager model slug when neither env key is set.
///
/// Why: a sensible, low-cost OpenRouter default keeps the manager operable out of
/// the box (matching the `trusty-analyze` deep-pass default), while the two-stage
/// resolver still routes it through whichever provider's credential resolves.
/// What: the fallback slug.
/// Test: `manager_model_env_precedence`.
pub const DEFAULT_MANAGER_MODEL: &str = "openai/gpt-4o-mini";

/// Max tokens requested for a single manager inference call.
///
/// Why: digests and chat replies are short prose; a bounded cap keeps latency and
/// cost predictable for a human-driven surface.
/// What: the `max_tokens` set on every [`trusty_common::inference::ChatRequest`]
/// the manager issues.
/// Test: exercised via `tests/manager_inference.rs`.
pub const MANAGER_MAX_TOKENS: u32 = 900;

/// Sampling temperature for manager inference calls.
///
/// Why: a low temperature keeps digests grounded in the supplied deterministic
/// snapshot rather than inventing portfolio state.
/// What: the `temperature` set on every manager request.
/// Test: side-effect-only; exercised indirectly by the digest happy-path test.
pub const MANAGER_TEMPERATURE: f32 = 0.2;

/// Resolve the configured manager model slug from the environment.
///
/// Why: centralises the documented precedence ([`MANAGER_MODEL_ENV`] >
/// [`FALLBACK_MODEL_ENV`] > [`DEFAULT_MANAGER_MODEL`]) so both provisioning and
/// any diagnostic read agree.
/// What: returns the first non-empty env value in precedence order, else the
/// default slug.
/// Test: `manager_model_env_precedence`.
pub fn resolve_manager_model() -> String {
    std::env::var(MANAGER_MODEL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var(FALLBACK_MODEL_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MANAGER_MODEL.to_string())
}

/// A typed reason the manager could not obtain a live inference adapter.
///
/// Why: DOC-36 §4's degrade bar and #2580's acceptance criteria require a
/// non-panicking, actionable signal when no provider is configured — the handler
/// maps this to a 503 with a deterministic fallback rather than crashing. Keeping
/// it a `thiserror` enum (library convention) lets the handler branch on the
/// cause while surfacing a stable, secret-free message.
/// What: [`Self::NoProvider`] carries a fixed, actionable message; the underlying
/// resolver error is logged (never surfaced) so a credential value can never leak.
/// Test: `resolve_reports_no_provider_when_unconfigured`.
#[derive(Debug, thiserror::Error)]
pub enum InferenceUnavailable {
    /// No provider credential resolved (or no adapter is wired for it).
    #[error(
        "no inference provider is configured; set a provider key with \
         `tm config keys set <provider>` (e.g. openrouter) — or query \
         GET /api/v1/manager/status for the deterministic rollup"
    )]
    NoProvider,
}

/// How the manager obtains an [`InferenceAdapter`]: credential-resolved or fixed.
///
/// Why: production resolves a provider from the daemon's credential store on
/// demand, while tests (and a future provider hot-swap) inject a concrete adapter.
/// Modelling both as one enum keeps [`ManagerInference::resolve`] a single path.
/// What: [`Self::Credentialed`] holds the registered [`Configurator`] and the
/// resolved [`KeyStore`]; [`Self::Fixed`] holds a ready adapter.
/// Test: `resolve_returns_injected_adapter`, `resolve_reports_no_provider_when_unconfigured`.
enum Source {
    /// Build an adapter from a model slug + credential on each call (production).
    Credentialed {
        /// Factory registry (all default HTTP providers registered).
        configurator: Configurator,
        /// Resolved secure credential store (env > `.env.local` > secure store).
        store: Box<dyn KeyStore>,
    },
    /// A fixed, pre-built adapter (test injection / future reconfigure).
    Fixed(Arc<dyn InferenceAdapter>),
    /// Explicitly no provider — always degrades. Models "the operator has no
    /// provider configured" independent of ambient env/credential state, so the
    /// degrade path (and the hermetic digest degrade test) is deterministic.
    Unconfigured,
}

/// Inner mutable state of [`ManagerInference`], guarded by a single mutex.
struct Inner {
    /// The configured model slug (env-resolved, or a test override).
    model: String,
    /// The adapter-resolution strategy.
    source: Source,
}

/// The manager's inference seam — resolves a live adapter or a typed degrade.
///
/// Why: [`crate::daemon::manager::ManagerState`] threads exactly one of these so
/// every digest/chat handler shares one provider-resolution policy, and the
/// hermetic suite can swap it for a scripted adapter through the shared `Arc`
/// without a bespoke `DaemonState` constructor. Interior mutability (a plain
/// `Mutex`; reads are rare, human-driven) lets [`Self::set_adapter`] hot-swap the
/// source across all handler clones.
/// What: owns [`Inner`] behind a `Mutex`. [`Self::provision`] builds the
/// production credential-backed variant; [`Self::with_adapter`]/[`Self::set_adapter`]
/// install a fixed adapter; [`Self::resolve`] returns `(model, adapter)` or
/// [`InferenceUnavailable`].
/// Test: `resolve_reports_no_provider_when_unconfigured`, `resolve_returns_injected_adapter`.
pub struct ManagerInference {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for ManagerInference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the credential store or factory set; the model + source
        // kind are the only diagnostic bits, and neither can leak a secret.
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let kind = match guard.source {
            Source::Credentialed { .. } => "credentialed",
            Source::Fixed(_) => "fixed",
            Source::Unconfigured => "unconfigured",
        };
        f.debug_struct("ManagerInference")
            .field("model", &guard.model)
            .field("source", &kind)
            .finish()
    }
}

impl ManagerInference {
    /// Provision the production credential-backed inference seam.
    ///
    /// Why: DOC-36 §3.3 wires the manager to the commons `Configurator` with the
    /// default HTTP provider factories and the daemon's resolved credential store;
    /// the model is the documented env-configurable slug. Built once at daemon
    /// startup alongside the portfolio palace.
    /// What: registers all default provider factories into a fresh
    /// [`Configurator`], resolves the secure [`default_store`], and records the
    /// env-resolved model.
    /// Test: `resolve_reports_no_provider_when_unconfigured` (built via provision).
    pub fn provision() -> Self {
        let mut configurator = Configurator::new();
        register_default_factories(&mut configurator);
        Self {
            inner: Mutex::new(Inner {
                model: resolve_manager_model(),
                source: Source::Credentialed {
                    configurator,
                    store: default_store(),
                },
            }),
        }
    }

    /// Construct an inference seam bound to a fixed, pre-built adapter.
    ///
    /// Why: the hermetic suite (#2584) needs a deterministic adapter
    /// (`ScriptedAdapter`, or an `OpenAiCompatAdapter` pointed at
    /// `MockInferenceServer`) with no live key; a future phase may inject a
    /// hot-swapped provider the same way.
    /// What: stores `adapter` and `model` as the [`Source::Fixed`] variant.
    /// Test: `resolve_returns_injected_adapter`.
    pub fn with_adapter(adapter: Arc<dyn InferenceAdapter>, model: impl Into<String>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                model: model.into(),
                source: Source::Fixed(adapter),
            }),
        }
    }

    /// Hot-swap the resolution source to a fixed adapter through a shared `&self`.
    ///
    /// Why: the HTTP-level hermetic tests build a real `DaemonState` (which
    /// provisions the production seam) and then install a scripted adapter through
    /// the shared `Arc<ManagerState>` — no bespoke daemon constructor required.
    /// This is the single documented override seam; it reconfigures *inference*,
    /// never a portfolio record, so it does not breach the read-only boundary.
    /// What: replaces the source with [`Source::Fixed`] and overrides the model.
    /// Test: `tests/manager_inference.rs` installs a scripted adapter via this.
    pub fn set_adapter(&self, adapter: Arc<dyn InferenceAdapter>, model: impl Into<String>) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.model = model.into();
        guard.source = Source::Fixed(adapter);
    }

    /// Force the seam into the "no provider configured" state.
    ///
    /// Why: models an operator with no credential regardless of ambient env, so
    /// the digest degrade path (503 + deterministic fallback) is deterministically
    /// exercisable over HTTP without unsetting process-wide credentials. A future
    /// phase may also flip here when a provider is explicitly disabled.
    /// What: replaces the source with [`Source::Unconfigured`], so [`Self::resolve`]
    /// returns [`InferenceUnavailable::NoProvider`].
    /// Test: `tests/manager_inference.rs` (`manager_digest_degrades_without_provider`).
    pub fn set_unconfigured(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.source = Source::Unconfigured;
    }

    /// The currently-configured model slug.
    ///
    /// Why: the digest/chat responses echo the model that authored a narrative so
    /// consumers can attribute it; also lets tests assert the resolved slug.
    /// What: clones the model string under the lock.
    /// Test: `manager_model_env_precedence` (via provision), `resolve_returns_injected_adapter`.
    pub fn model(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .model
            .clone()
    }

    /// Resolve a live adapter (and its model slug), or a typed degrade.
    ///
    /// Why: the one call every manager LLM handler makes before issuing a chat
    /// request. Building under the lock is safe — [`Configurator::build`] is
    /// synchronous (no `await`), so the guard is released before the caller's
    /// `chat` future is awaited.
    /// What: for a fixed source, clones the ready adapter; for a credentialed
    /// source, runs [`Configurator::build`] against the model slug and store,
    /// converting the `Box` into an `Arc`. A missing-credential / no-adapter /
    /// construction error maps to [`InferenceUnavailable::NoProvider`] with the
    /// underlying cause logged (never surfaced) so no secret can leak.
    /// Test: `resolve_returns_injected_adapter`, `resolve_reports_no_provider_when_unconfigured`.
    pub fn resolve(&self) -> Result<(String, Arc<dyn InferenceAdapter>), InferenceUnavailable> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let model = guard.model.clone();
        match &guard.source {
            Source::Unconfigured => Err(InferenceUnavailable::NoProvider),
            Source::Fixed(adapter) => Ok((model, Arc::clone(adapter))),
            Source::Credentialed {
                configurator,
                store,
            } => match configurator.build(&model, store.as_ref()) {
                Ok(boxed) => Ok((model, Arc::from(boxed))),
                Err(e) => {
                    // MissingCredential / NoAdapterRegistered / construction
                    // failure all degrade identically; log the cause for the
                    // operator (it carries no secret) and surface the stable,
                    // actionable message.
                    match e {
                        InferenceError::MissingCredential { .. } => {
                            tracing::debug!(
                                "manager inference unavailable: no credential resolved"
                            );
                        }
                        other => {
                            tracing::warn!("manager inference unavailable: {other}");
                        }
                    }
                    Err(InferenceUnavailable::NoProvider)
                }
            },
        }
    }
}

#[cfg(test)]
#[path = "inference_tests.rs"]
mod tests;
