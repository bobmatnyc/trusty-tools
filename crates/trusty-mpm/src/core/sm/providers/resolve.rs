//! Provider + model resolution for the SM (DOC-14 §5.2–§5.4).
//!
//! Why: SM-2's job is to turn the declarative `[session_manager.inference]`
//! config (provider selector + per-task model tiers, with optional routing
//! prefixes and a deprecated `model` alias) into a concrete provider instance
//! and bare model id for each call — orchestration, summarization, or
//! compaction. Centralising that logic here keeps later tickets (SM-5
//! compaction, SM-7 endpoint) from re-deriving the rules.
//! What: this module exposes (1) [`resolve_provider_and_model`] — pure
//! prefix-routing of a model string against the config provider; (2)
//! [`resolve_tier_model`] — per-task tier selection (sm/summary/compaction)
//! including the deprecated-`model` alias fallback; and (3) [`ProviderRegistry`]
//! — the credential-aware entry point that builds an `Arc<dyn LlmProvider>` for
//! a resolved call, applying the `auto` precedence chain (Anthropic → Bedrock →
//! OpenRouter → degraded) and surfacing degraded mode as a graceful
//! [`SmLlmError::Degraded`] rather than a panic.
//! Test: the `tests` module covers all three prefixes + bare routing, tier
//! defaults, alias fallback, compaction override, provider validation,
//! precedence, and degraded mode.

use std::sync::Arc;

use tracing::debug;
#[cfg(not(feature = "bedrock"))]
use tracing::info;
#[cfg(feature = "bedrock")]
use tracing::warn;

use super::{
    ANTHROPIC_MODEL_PREFIX, AnthropicProvider, BEDROCK_MODEL_PREFIX, LlmProvider,
    OPENROUTER_MODEL_PREFIX, OpenRouterProvider, ProviderKind, SmLlmError,
};
use crate::core::sm::config::SmInferenceConfig;

#[cfg(feature = "bedrock")]
use super::BedrockProvider;

// ─── Per-task tiers ─────────────────────────────────────────────────────────────

/// The SM's per-task model tiers (DOC-14 §5.4).
///
/// Why: the SM runs two classes of call with different cost/quality profiles —
/// orchestration (Sonnet tier) and summarization/compaction (Haiku tier) — and
/// must select the right model id per task.
/// What: identifies which tier a call belongs to so [`resolve_tier_model`] can
/// pick the matching config field (`sm_model` / `summary_model` /
/// `compaction_model`).
/// Test: `tier_*` tests in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmModelTier {
    /// SM chat / orchestration — MEDIUM (Sonnet) tier (`sm_model`).
    Orchestration,
    /// Session summarization (`last_summary`) — INEXPENSIVE (Haiku) tier
    /// (`summary_model`).
    Summary,
    /// Rolling auto-compaction compression — Haiku tier; uses
    /// `compaction_model` when set, else falls back to `summary_model`.
    Compaction,
}

// ─── Pure prefix routing (DOC-14 §5.2 D5.3) ────────────────────────────────────

/// Resolve `(ProviderKind, bare_model_id)` from a possibly-prefixed model
/// string and the config provider.
///
/// Why: a model id may carry an `anthropic/`, `bedrock/`, or `openrouter/`
/// prefix to pin a provider for that call, overriding the active `provider`
/// (D5.3); a bare id routes to `default_provider`. This is the single source of
/// truth for that rule.
/// What: strips a known prefix (returning the matching kind + bare id) or
/// returns `(default_provider, model)` unchanged for a bare id. Note: when
/// `default_provider` is [`ProviderKind::Auto`], the *kind* stays `Auto` here —
/// the credential-aware precedence chain is applied later by
/// [`ProviderRegistry::build`].
/// Test: `route_anthropic_prefix`, `route_bedrock_prefix`,
/// `route_openrouter_prefix`, `route_bare_uses_default`.
pub fn resolve_provider_and_model(
    model: &str,
    default_provider: ProviderKind,
) -> (ProviderKind, String) {
    if let Some(bare) = model.strip_prefix(ANTHROPIC_MODEL_PREFIX) {
        return (ProviderKind::Anthropic, bare.to_string());
    }
    if let Some(bare) = model.strip_prefix(BEDROCK_MODEL_PREFIX) {
        return (ProviderKind::Bedrock, bare.to_string());
    }
    if let Some(bare) = model.strip_prefix(OPENROUTER_MODEL_PREFIX) {
        return (ProviderKind::OpenRouter, bare.to_string());
    }
    (default_provider, model.to_string())
}

// ─── Per-task tier selection (DOC-14 §5.4) ──────────────────────────────────────

/// Select the configured model string for a task tier, applying the deprecated
/// `model` alias and the compaction → summary fallback.
///
/// Why: §5.4 specifies exact precedence per tier, plus SM-1's carry-forward
/// obligation that an empty `sm_model` falls back to the deprecated `model`
/// alias. Encoding it once avoids drift across call sites.
/// What: for [`SmModelTier::Orchestration`] returns `sm_model`, or `model`
/// (deprecated alias) when `sm_model` is empty. For [`SmModelTier::Summary`]
/// returns `summary_model`. For [`SmModelTier::Compaction`] returns
/// `compaction_model` when non-empty, else `summary_model`. Returns the
/// (possibly still prefixed) tier model string; callers pass it to
/// [`resolve_provider_and_model`]. An empty result means "no model configured
/// for this tier" and is surfaced as [`SmLlmError::Validation`].
/// Test: `tier_orchestration_uses_sm_model`,
/// `tier_orchestration_alias_fallback`, `tier_summary_default`,
/// `tier_compaction_override`, `tier_compaction_falls_back_to_summary`,
/// `tier_empty_is_validation_error`.
pub fn resolve_tier_model(
    cfg: &SmInferenceConfig,
    tier: SmModelTier,
) -> Result<String, SmLlmError> {
    let chosen = match tier {
        SmModelTier::Orchestration => {
            if !cfg.sm_model.trim().is_empty() {
                cfg.sm_model.clone()
            } else {
                // Deprecated `model` alias carry-forward (SM-1 review).
                cfg.model.clone()
            }
        }
        SmModelTier::Summary => cfg.summary_model.clone(),
        SmModelTier::Compaction => {
            if !cfg.compaction_model.trim().is_empty() {
                cfg.compaction_model.clone()
            } else {
                cfg.summary_model.clone()
            }
        }
    };
    if chosen.trim().is_empty() {
        return Err(SmLlmError::Validation(format!(
            "no model configured for the {tier:?} tier \
             (set sm_model/summary_model/compaction_model in [session_manager.inference])"
        )));
    }
    Ok(chosen)
}

// ─── A fully-resolved call ─────────────────────────────────────────────────────

/// The outcome of resolving a tier into a concrete provider + bare model id.
///
/// Why: SM-5/SM-7 need both the provider handle (to call `complete`) and the
/// resolved model metadata (for logging the per-call cost/usage with the right
/// model id) in one value.
/// What: holds the built `Arc<dyn LlmProvider>`, the bare model id sent
/// upstream, and the [`ProviderKind`] actually selected (after the `auto`
/// precedence chain).
/// Test: `registry_*` tests assert these fields.
pub struct ResolvedCall {
    /// The provider to invoke `complete` on.
    pub provider: Arc<dyn LlmProvider>,
    /// The bare model id (routing prefix stripped) for this call.
    pub model: String,
    /// The provider kind actually selected.
    pub kind: ProviderKind,
}

impl std::fmt::Debug for ResolvedCall {
    /// Why: `Arc<dyn LlmProvider>` is not `Debug`, but call sites (and tests
    /// via `expect_err`) need a `Debug` `ResolvedCall`. We print the provider's
    /// `name()` instead of the opaque trait object.
    /// What: formats `kind`, `model`, and the provider name.
    /// Test: exercised by `resolve_tests` `expect_err` call sites.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedCall")
            .field("kind", &self.kind)
            .field("model", &self.model)
            .field("provider", &self.provider.name())
            .finish()
    }
}

// ─── Credential-aware registry ─────────────────────────────────────────────────

/// Builds providers for resolved calls, applying credential precedence (§5.3).
///
/// Why: turning a resolved `(kind, model)` into a live provider needs
/// credentials (`ANTHROPIC_API_KEY`, AWS chain, `OPENROUTER_API_KEY`) and the
/// `auto` precedence chain. Keeping that in one credential-aware type lets SM-7
/// build it once from the environment and reuse it per request, and lets tests
/// inject explicit keys instead of reading the real environment.
/// What: holds the optional provider credentials and the parsed config
/// `provider`. [`Self::build`] resolves a tier model into a [`ResolvedCall`],
/// applying the `auto` precedence chain (Anthropic → Bedrock → OpenRouter →
/// degraded) when neither a prefix nor an explicit `provider` pins one.
/// Test: `registry_routes_explicit_prefix`, `registry_auto_precedence`,
/// `registry_degraded_without_creds`, `registry_rejects_unknown_provider`.
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    /// `ANTHROPIC_API_KEY` value, if present and non-empty.
    pub anthropic_api_key: Option<String>,
    /// Whether AWS credentials are resolvable (the daemon probes this; tests
    /// set it directly). Only consulted by the `auto` precedence chain when the
    /// `bedrock` feature is enabled. NOTE: [`Self::from_env`] sets this from a
    /// conservative env-marker heuristic that cannot see `~/.aws/credentials`,
    /// ECS/EKS/IMDS sources — set `provider = "bedrock"` explicitly in those
    /// deployments (see [`Self::from_env`]).
    pub aws_credentials_available: bool,
    /// `OPENROUTER_API_KEY` value, if present and non-empty.
    pub openrouter_api_key: Option<String>,
}

impl ProviderRegistry {
    /// Build a registry by reading provider credentials from the environment.
    ///
    /// Why: the daemon (SM-7) constructs the registry once from the process
    /// environment; this keeps that single, documented place.
    /// What: reads `ANTHROPIC_API_KEY` / `OPENROUTER_API_KEY` (treating empty
    /// as absent) and probes AWS credential presence via `AWS_ACCESS_KEY_ID` /
    /// `AWS_PROFILE` / `AWS_ROLE_ARN` env markers (a cheap, non-async heuristic;
    /// the real SDK chain is the final authority at call time).
    ///
    /// This env-marker probe is **intentionally conservative**: it only sees
    /// credentials advertised through those three env vars. It deliberately
    /// does NOT detect AWS-native credential sources that the SDK chain resolves
    /// at call time — the shared-credentials file (`~/.aws/credentials`), an ECS
    /// task role, EKS IRSA, or EC2 IMDS. In those AWS-native deployments the
    /// `auto` precedence chain may therefore skip Bedrock even though Bedrock is
    /// in fact reachable. Operators running in such environments should set
    /// `[session_manager.inference].provider = "bedrock"` EXPLICITLY to bypass
    /// the `auto` heuristic and pin Bedrock; the explicit provider path does not
    /// consult this flag.
    /// Test: side-effect-only env read; logic covered via the explicit-field
    /// constructor used by `registry_*` tests.
    pub fn from_env() -> Self {
        let non_empty = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let aws_credentials_available = ["AWS_ACCESS_KEY_ID", "AWS_PROFILE", "AWS_ROLE_ARN"]
            .iter()
            .any(|k| non_empty(k).is_some());

        // Operator hint: the env-marker heuristic missed AWS credentials, but
        // the process looks AWS-hosted (these markers are set by ECS/EKS/Lambda
        // and `aws-sdk` web-identity flows whose creds the SDK chain resolves at
        // call time but this cheap probe cannot see). Nudge the operator to pin
        // Bedrock explicitly so the `auto` chain doesn't skip it. stderr-only.
        if !aws_credentials_available {
            let aws_hosted_markers = [
                "AWS_EXECUTION_ENV",
                "ECS_CONTAINER_METADATA_URI",
                "ECS_CONTAINER_METADATA_URI_V4",
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
                "AWS_WEB_IDENTITY_TOKEN_FILE",
            ];
            if aws_hosted_markers
                .iter()
                .any(|k| std::env::var_os(k).is_some())
            {
                debug!(
                    "AWS-hosted environment detected but no file/role-based AWS credentials \
                     visible to the env heuristic; set [session_manager.inference].provider = \
                     \"bedrock\" explicitly to pin Bedrock"
                );
            }
        }

        Self {
            anthropic_api_key: non_empty("ANTHROPIC_API_KEY"),
            aws_credentials_available,
            openrouter_api_key: non_empty("OPENROUTER_API_KEY"),
        }
    }

    /// Resolve a tier into a concrete [`ResolvedCall`] (provider + bare model).
    ///
    /// Why: this is the API SM-5/SM-7 call per request: "give me the provider
    /// and model for the orchestration/summary/compaction tier", honouring
    /// prefixes, the deprecated alias, the `auto` precedence chain, and
    /// degraded mode.
    /// What: (1) parses `cfg.provider` (rejecting unknown values); (2) selects
    /// the tier model via [`resolve_tier_model`]; (3) routes it via
    /// [`resolve_provider_and_model`]; (4) when the routed kind is `Auto`,
    /// applies the credential precedence chain; (5) constructs the concrete
    /// provider, or returns [`SmLlmError::Degraded`] when no credentials exist.
    /// Test: `registry_routes_explicit_prefix`, `registry_auto_precedence`,
    /// `registry_degraded_without_creds`, `registry_rejects_unknown_provider`.
    pub async fn build(
        &self,
        cfg: &SmInferenceConfig,
        tier: SmModelTier,
    ) -> Result<ResolvedCall, SmLlmError> {
        let (effective_kind, bare_model) = self.resolve_kind_and_model(cfg, tier)?;
        let provider = self.construct(effective_kind, &bare_model).await?;
        Ok(ResolvedCall {
            provider,
            model: bare_model,
            kind: effective_kind,
        })
    }

    /// Resolve a tier into its `(effective_kind, bare_model)` WITHOUT building
    /// the concrete provider.
    ///
    /// Why: this is the pure, network-free decision half of [`Self::build`] —
    /// parsing/validating `provider`, selecting the tier model, applying prefix
    /// routing, and running the `auto` precedence chain. Splitting it out keeps
    /// `build` thin and lets tests assert the routing decision (including the
    /// Bedrock-preference precedence) without `construct`'s real AWS SDK config
    /// load (which can probe IMDS/network).
    /// What: (1) parses `cfg.provider`; (2) selects the tier model via
    /// [`resolve_tier_model`]; (3) routes via [`resolve_provider_and_model`];
    /// (4) resolves an `Auto` routed kind through [`Self::auto_precedence`].
    /// Test: `registry_auto_precedence`, `registry_degraded_without_creds`,
    /// `registry_auto_prefers_bedrock_when_available` (feature-gated).
    fn resolve_kind_and_model(
        &self,
        cfg: &SmInferenceConfig,
        tier: SmModelTier,
    ) -> Result<(ProviderKind, String), SmLlmError> {
        let default_provider = ProviderKind::parse(&cfg.provider)?;
        let tier_model = resolve_tier_model(cfg, tier)?;
        let (routed_kind, bare_model) = resolve_provider_and_model(&tier_model, default_provider);

        let effective_kind = match routed_kind {
            ProviderKind::Auto => self.auto_precedence()?,
            explicit => explicit,
        };

        debug!(
            ?tier,
            requested = %tier_model,
            ?effective_kind,
            bare_model = %bare_model,
            "sm resolve provider+model"
        );
        Ok((effective_kind, bare_model))
    }

    /// Pick the first provider with credentials in §5.3 precedence order.
    ///
    /// Why: `provider = "auto"` (the default) must deterministically prefer
    /// Anthropic → Bedrock → OpenRouter, then degrade gracefully.
    /// What: returns the first available kind; [`SmLlmError::Degraded`] when
    /// none are available. Bedrock is only considered when the `bedrock`
    /// feature is compiled in.
    /// Test: `registry_auto_precedence`, `registry_degraded_without_creds`.
    fn auto_precedence(&self) -> Result<ProviderKind, SmLlmError> {
        if self.anthropic_api_key.is_some() {
            return Ok(ProviderKind::Anthropic);
        }
        #[cfg(feature = "bedrock")]
        if self.aws_credentials_available {
            return Ok(ProviderKind::Bedrock);
        }
        if self.openrouter_api_key.is_some() {
            return Ok(ProviderKind::OpenRouter);
        }
        Err(SmLlmError::Degraded(
            "no ANTHROPIC_API_KEY, AWS credentials, or OPENROUTER_API_KEY available".to_string(),
        ))
    }

    /// Construct the concrete provider for an explicit kind + bare model.
    ///
    /// Why: separates credential lookup/error-mapping from the precedence
    /// logic; returns a clear degraded/validation error when the required
    /// credential is missing or the `bedrock` feature is absent.
    /// What: builds [`AnthropicProvider`] / [`OpenRouterProvider`] from the
    /// matching key, or [`BedrockProvider`] (feature-gated). `Auto` cannot
    /// reach here (it is resolved earlier) and is treated as an internal error.
    /// Test: `registry_routes_explicit_prefix`,
    /// `registry_bedrock_prefix_without_feature` (cfg-gated).
    async fn construct(
        &self,
        kind: ProviderKind,
        bare_model: &str,
    ) -> Result<Arc<dyn LlmProvider>, SmLlmError> {
        match kind {
            ProviderKind::Anthropic => {
                let key = self.anthropic_api_key.clone().ok_or_else(|| {
                    SmLlmError::Degraded(
                        "anthropic provider selected but ANTHROPIC_API_KEY is not set".to_string(),
                    )
                })?;
                Ok(Arc::new(AnthropicProvider::new(
                    key,
                    bare_model.to_string(),
                )?))
            }
            ProviderKind::OpenRouter => {
                let key = self.openrouter_api_key.clone().ok_or_else(|| {
                    SmLlmError::Degraded(
                        "openrouter provider selected but OPENROUTER_API_KEY is not set"
                            .to_string(),
                    )
                })?;
                Ok(Arc::new(OpenRouterProvider::new(
                    key,
                    bare_model.to_string(),
                )?))
            }
            ProviderKind::Bedrock => self.construct_bedrock(bare_model).await,
            ProviderKind::Auto => Err(SmLlmError::Validation(
                "internal: ProviderKind::Auto must be resolved before construction".to_string(),
            )),
        }
    }

    /// Construct a Bedrock provider (only when the `bedrock` feature is on).
    ///
    /// Why: isolates the feature gate so `construct` stays readable, and gives a
    /// clear error when an operator pins `bedrock/` in a build that lacks the
    /// feature.
    /// What: with the feature, awaits [`BedrockProvider::new`] (credential +
    /// region resolution); without it, returns a [`SmLlmError::Validation`]
    /// telling the operator to rebuild with `--features bedrock`.
    /// Test: `registry_bedrock_prefix_without_feature` (no-feature build).
    #[cfg(feature = "bedrock")]
    async fn construct_bedrock(
        &self,
        bare_model: &str,
    ) -> Result<Arc<dyn LlmProvider>, SmLlmError> {
        if !self.aws_credentials_available {
            warn!("bedrock selected but no AWS credentials detected; attempting SDK chain anyway");
        }
        let provider = BedrockProvider::new(bare_model.to_string(), None).await?;
        Ok(Arc::new(provider))
    }

    /// No-feature stub: report that Bedrock requires the `bedrock` feature.
    ///
    /// Why: an operator who pins a `bedrock/` model in a default build must get
    /// a clear, actionable error rather than a confusing degraded state.
    /// What: always returns [`SmLlmError::Validation`].
    /// Test: `registry_bedrock_prefix_without_feature`.
    #[cfg(not(feature = "bedrock"))]
    async fn construct_bedrock(
        &self,
        _bare_model: &str,
    ) -> Result<Arc<dyn LlmProvider>, SmLlmError> {
        info!("bedrock model selected but the `bedrock` cargo feature is not enabled");
        Err(SmLlmError::Validation(
            "bedrock provider requires building trusty-mpm with `--features bedrock`".to_string(),
        ))
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
