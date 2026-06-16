//! SM provider/health status for `sm.health` (DOC-14 §5.3, §1A.1).
//!
//! Why: the SM-STDIO `sm.health` method must report whether the SM can actually
//! run a turn — is a provider resolvable, is it degraded, and which model tiers
//! are configured — WITHOUT making a network call. That status logic is SM core,
//! not transport, so it lives on [`SessionManagerAgent`] (the stdio adapter just
//! forwards it). Reusing the SM-2 [`TierResolver`] seam means `resolve` only
//! checks credentials + constructs a provider handle; it does not call the model,
//! so health is cheap and deterministic-to-test with the mock resolver.
//! What: [`SmHealth`] (`{ ok, provider, degraded, model_tiers }`) and
//! [`SessionManagerAgent::health`], which resolves the orchestration tier to
//! determine `provider`/`degraded`/`ok` and reads the three tier model ids from
//! config.
//! Test: `agent/health_tests.rs` — degraded (no runtime / degraded resolver),
//! healthy (mock provider), and the model-tier reporting.

use super::SessionManagerAgent;
use crate::core::sm::providers::SmModelTier;

/// The SM health snapshot returned by `sm.health` (§1A.1).
///
/// Why: a parent driver (`claude-mpm`) probes this before driving a turn to know
/// whether inference is available and, if not, that the SM is in graceful
/// degraded mode rather than broken. Bundling the four spec fields into one
/// serializable struct keeps the adapter mapping a single `serde_json::to_value`.
/// What: `ok` is true when a non-degraded provider resolved for orchestration;
/// `provider` is the resolved provider's name (or `"none"` when degraded);
/// `degraded` is `!ok`; `model_tiers` lists the configured tier model ids.
/// Test: `agent/health_tests.rs`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SmHealth {
    /// True when a provider with credentials resolved for the orchestration tier.
    pub ok: bool,
    /// The resolved provider name (`"anthropic"`/`"bedrock"`/`"openrouter"`), or
    /// `"none"` when no provider is available (degraded).
    pub provider: String,
    /// Graceful degraded mode (no inference provider configured), the inverse of
    /// `ok`.
    pub degraded: bool,
    /// The configured per-tier model ids (orchestration / summary / compaction).
    pub model_tiers: SmModelTiers,
}

/// The configured model id for each SM task tier (§5.4).
///
/// Why: `sm.health` surfaces what models the SM is configured to use per tier so
/// an operator can confirm the deployment is wired as intended.
/// What: the orchestration (Sonnet), summary (Haiku), and compaction model ids,
/// each as the raw (possibly provider-prefixed) config string.
/// Test: `agent/health_tests.rs::health_reports_model_tiers`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SmModelTiers {
    /// Orchestration-tier model (`sm_model`, with the deprecated-alias fallback).
    pub orchestration: String,
    /// Summarization-tier model (`summary_model`).
    pub summary: String,
    /// Compaction-tier model (`compaction_model`, falling back to `summary_model`).
    pub compaction: String,
}

impl SessionManagerAgent {
    /// Report the SM's provider/health status without a network call (§5.3).
    ///
    /// Why: `sm.health` (SM-STDIO) needs `{ ok, provider, degraded, model_tiers }`
    /// to tell a parent driver whether the SM can run a turn. Resolving the
    /// orchestration tier through the [`TierResolver`](crate::core::sm::providers::TierResolver)
    /// checks credentials + builds a provider handle but does NOT call the model,
    /// so this is cheap and testable with the mock resolver.
    /// What: with no runtime (inert agent) → degraded with `provider = "none"`.
    /// With a runtime, resolves the orchestration tier: a [`SmLlmError::Degraded`]
    /// (or any resolution error) → degraded; success → `ok = true` and the
    /// resolved provider name. Always fills `model_tiers` from config.
    /// Test: `agent/health_tests.rs`.
    pub async fn health(&self) -> SmHealth {
        let model_tiers = self.model_tiers();
        let Some(runtime) = self.runtime.as_ref() else {
            return SmHealth {
                ok: false,
                provider: "none".to_string(),
                degraded: true,
                model_tiers,
            };
        };
        match runtime
            .resolver
            .resolve(&self.config.inference, SmModelTier::Orchestration)
            .await
        {
            Ok(call) => SmHealth {
                ok: true,
                provider: call.kind.name().to_string(),
                degraded: false,
                model_tiers,
            },
            Err(_) => SmHealth {
                ok: false,
                provider: "none".to_string(),
                degraded: true,
                model_tiers,
            },
        }
    }

    /// Read the configured per-tier model ids from config (§5.4).
    ///
    /// Why: health reports what each tier is configured to use; resolving the
    /// tier model strings via the same SM-2 selection logic keeps health and the
    /// real chat/compaction calls reporting identical ids.
    /// What: returns the orchestration/summary/compaction model strings via
    /// [`resolve_tier_model`](crate::core::sm::providers::resolve_tier_model),
    /// substituting an empty string when a tier has no model configured.
    /// Test: `agent/health_tests.rs::health_reports_model_tiers`.
    fn model_tiers(&self) -> SmModelTiers {
        use crate::core::sm::providers::resolve_tier_model;
        let tier = |t| resolve_tier_model(&self.config.inference, t).unwrap_or_default();
        SmModelTiers {
            orchestration: tier(SmModelTier::Orchestration),
            summary: tier(SmModelTier::Summary),
            compaction: tier(SmModelTier::Compaction),
        }
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
