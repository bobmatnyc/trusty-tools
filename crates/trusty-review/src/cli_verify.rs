//! Verifier-provider construction for the CLI / daemon entry points (Phase 2, #583).
//!
//! Why: both the one-shot CLI (`run` / `compare`) and the long-lived `serve`
//! daemon must build the verifier-role provider, but with different
//! failure-handling: the CLI degrades to no-verification on a build error, while
//! the daemon treats a build failure as fatal (the liveness gate exists to catch
//! exactly that misconfiguration).  Keeping both builders here keeps `main.rs`
//! focused on argument parsing and dispatch.
//!
//! What: `build_verifier_opt` (non-fatal, returns `Option`) and
//! `build_verifier_for_serve` (fatal, returns `Result<Option<_>>`).
//!
//! Test: the network build path is not unit-tested; the gating logic these feed
//! (`enforce_verifier_liveness`) is covered in `pipeline::verify_liveness::tests`.

use std::sync::Arc;

use tracing::warn;

use trusty_review::{config::ReviewConfig, llm::LlmProvider, llm::build_provider};

/// Build the verifier provider when verification is enabled, else `None`
/// (non-fatal).
///
/// Why: the verifier role is a separate model (Haiku by default) resolved
/// independently of the reviewer.  The one-shot CLI must not abort a review just
/// because the verifier could not be built — it degrades to no-verification.
/// What: returns `Some(provider)` when `config.verification.enabled` and the
/// build succeeds; `None` when verification is disabled or the build fails
/// (logged).
/// Test: build path is network-bound; covered transitively by the verification
/// runner tests with injected fakes.
pub async fn build_verifier_opt(config: &ReviewConfig) -> Option<Arc<dyn LlmProvider>> {
    if !config.verification.enabled {
        return None;
    }
    let role = &config.role_models.verifier;
    match build_provider(&role.model, &role.provider, &config.openrouter_api_key).await {
        Ok(p) => Some(p),
        Err(e) => {
            warn!("failed to build verifier provider (continuing without verification): {e}");
            None
        }
    }
}

/// Build the verifier provider for the `serve` daemon (fatal on failure).
///
/// Why: unlike the one-shot CLI, the long-lived daemon must not silently degrade
/// to no-verification on a verifier-build failure — that failure is exactly the
/// kind of misconfiguration the liveness gate exists to catch.  When verification
/// is enabled a build error is fatal; when disabled we return `None`.
/// What: returns `Ok(Some(provider))` when enabled and the build succeeds,
/// `Ok(None)` when verification is disabled, and `Err` when enabled but the build
/// fails.
/// Test: build path is network-bound; the decision branch is covered by the
/// liveness-gate unit tests via `enforce_verifier_liveness`.
#[cfg(feature = "http-server")]
pub async fn build_verifier_for_serve(
    config: &ReviewConfig,
) -> anyhow::Result<Option<Arc<dyn LlmProvider>>> {
    if !config.verification.enabled {
        return Ok(None);
    }
    let role = &config.role_models.verifier;
    let p = build_provider(&role.model, &role.provider, &config.openrouter_api_key)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build verifier provider: {e}"))?;
    Ok(Some(p))
}
