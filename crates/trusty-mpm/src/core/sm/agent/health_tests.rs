//! Tests for [`SessionManagerAgent::health`] (SM-STDIO `sm.health` source).
//!
//! Why: `sm.health` must report `{ ok, provider, degraded, model_tiers }` without
//! a network call. These tests pin the three states — no runtime (inert),
//! degraded resolver, and a healthy mock provider — plus the tier reporting,
//! deterministically with the mock resolver.
//! What: builds the agent feature-aware over a [`MockResolver`] and asserts the
//! [`SmHealth`] shape.
//! Test: this is the test module.

use std::sync::Arc;

use super::super::mock::{MockChatProvider, MockResolver};
use crate::core::sm::agent::SessionManagerAgent;
use crate::core::sm::config::SessionManagerConfig;

/// An enabled SM config (health does not require enablement, but mirrors prod).
fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build an agent over the mock resolver, feature-aware (no memory in tests).
fn agent_with(cfg: SessionManagerConfig, resolver: Arc<MockResolver>) -> SessionManagerAgent {
    let data_root = std::env::temp_dir();
    #[cfg(feature = "sm-memory")]
    {
        SessionManagerAgent::with_runtime(cfg, resolver, data_root, None)
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        SessionManagerAgent::with_runtime(cfg, resolver, data_root)
    }
}

/// Why: an inert agent (built via `new`, no runtime) has no provider, so health
/// must report degraded with `provider = "none"` and `ok = false`.
/// What: builds a `new()` agent and asserts the degraded health shape.
/// Test: this is the test.
#[tokio::test]
async fn health_no_runtime_is_degraded() {
    let agent = SessionManagerAgent::new(enabled_config());
    let health = agent.health().await;
    assert!(!health.ok);
    assert!(health.degraded);
    assert_eq!(health.provider, "none");
}

/// Why: when the resolver reports degraded (no credentials), health must surface
/// graceful degraded mode rather than ok.
/// What: builds an agent over a degraded resolver and asserts degraded health.
/// Test: this is the test.
#[tokio::test]
async fn health_degraded_resolver_is_degraded() {
    let agent = agent_with(enabled_config(), Arc::new(MockResolver::degraded()));
    let health = agent.health().await;
    assert!(!health.ok);
    assert!(health.degraded);
    assert_eq!(health.provider, "none");
}

/// Why: with a resolvable provider, health must report ok + the resolved
/// provider name and NOT degraded.
/// What: builds an agent over a provider resolver; asserts `ok` + provider name.
/// Test: this is the test.
#[tokio::test]
async fn health_with_provider_is_ok() {
    let provider = MockChatProvider::new("unused", 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider));
    let agent = agent_with(enabled_config(), resolver);
    let health = agent.health().await;
    assert!(health.ok);
    assert!(!health.degraded);
    // The mock resolver tags resolved calls as Anthropic.
    assert_eq!(health.provider, "anthropic");
}

/// Why: health must surface the configured per-tier model ids so an operator can
/// confirm the deployment is wired as intended.
/// What: asserts the orchestration/summary/compaction model ids match the
/// default config tiers.
/// Test: this is the test.
#[tokio::test]
async fn health_reports_model_tiers() {
    let provider = MockChatProvider::new("unused", 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider));
    let agent = agent_with(enabled_config(), resolver);
    let health = agent.health().await;
    assert_eq!(
        health.model_tiers.orchestration,
        "anthropic/claude-sonnet-4-6"
    );
    assert_eq!(health.model_tiers.summary, "anthropic/claude-haiku");
    // Compaction falls back to summary when unset.
    assert_eq!(health.model_tiers.compaction, "anthropic/claude-haiku");
}
