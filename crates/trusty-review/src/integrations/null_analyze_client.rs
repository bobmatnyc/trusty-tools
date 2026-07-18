//! No-op `AnalyzeClient` used for the `--source-root` diff-only fallback (#2994).
//!
//! Why: mirrors `NullSearchClient` (see that module's doc comment for the full
//! rationale behind the swap). When `ReviewConfig::resolve_source_root`
//! degrades to diff-only because `--source-root` does not map to a registered
//! index, BOTH network-facing context dependencies must stop querying
//! whatever `config.search_index` happens to hold — search AND analyze.
//! Relaxing `context.require_analyze` alone is not enough: that flag only
//! changes what the required-context gate
//! (`pipeline::context_gate::preflight_context`) does when analyze is
//! unreachable; it does not stop `gather_context`
//! (`pipeline::runner_context::gather_context`) from calling
//! `has_analysis`/`complexity_hotspots`/`smells` against the REAL client,
//! which — if the trusty-analyze daemon happens to be healthy — would
//! silently query the wrong project's index. Swapping in a
//! `NullAnalyzeClient` guarantees `has_analysis` always reports unavailable,
//! so `gather_context` never queries the stale index.
//!
//! What: `NullAnalyzeClient` implements `AnalyzeClient` such that `health()`
//! always returns `Err(AnalyzeClientError::Unavailable(reason))` (the caller
//! supplies the `--source-root`-specific notice as `reason`), `has_analysis`
//! always returns `false`, and `complexity_hotspots`/`smells` are harmless
//! empty-result no-ops in case anything calls them after `has_analysis` has
//! already said no.
//!
//! Test: `null_analyze_client_health_is_unavailable`,
//! `null_analyze_client_has_analysis_is_false`,
//! `null_analyze_client_hotspots_and_smells_are_empty`.

use async_trait::async_trait;

use super::analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
};

/// An `AnalyzeClient` that always reports itself unavailable with a fixed reason.
///
/// Why: see module doc — the analyze-side counterpart of `NullSearchClient`,
/// used to fully disable static-analysis context in the `--source-root`
/// diff-only fallback rather than leaving it wired to a client that could
/// otherwise silently query a stale index.
/// What: holds the human-readable `reason` surfaced through
/// `AnalyzeClientError::Unavailable`.
/// Test: `null_analyze_client_health_is_unavailable`.
pub struct NullAnalyzeClient {
    reason: String,
}

impl NullAnalyzeClient {
    /// Construct a `NullAnalyzeClient` that always fails health/readiness
    /// checks with `reason`.
    ///
    /// Why: the caller (`commands::run`/`commands::compare`) already computed
    /// the operator-facing `--source-root` notice; reusing that exact string
    /// keeps this client's failure reason consistent with the
    /// `NullSearchClient` swapped in alongside it.
    /// What: stores `reason` for use by `health()`.
    /// Test: `null_analyze_client_health_is_unavailable`.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl AnalyzeClient for NullAnalyzeClient {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Err(AnalyzeClientError::Unavailable(self.reason.clone()))
    }

    async fn has_analysis(&self, _index_id: &str) -> bool {
        false
    }

    async fn complexity_hotspots(
        &self,
        _index_id: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(Vec::new())
    }

    async fn smells(&self, _index_id: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(Vec::new())
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_analyze_client_health_is_unavailable() {
        let client = NullAnalyzeClient::new("no index for --source-root /tmp/x");
        let err = client.health().await.expect_err("must report unavailable");
        assert!(matches!(err, AnalyzeClientError::Unavailable(_)));
        assert!(
            err.to_string().contains("no index for --source-root"),
            "error must carry the caller-supplied reason: {err}"
        );
    }

    #[tokio::test]
    async fn null_analyze_client_has_analysis_is_false() {
        let client = NullAnalyzeClient::new("reason");
        assert!(
            !client.has_analysis("any-index").await,
            "has_analysis must always be false so gather_context skips analyze entirely"
        );
    }

    #[tokio::test]
    async fn null_analyze_client_hotspots_and_smells_are_empty() {
        let client = NullAnalyzeClient::new("reason");
        assert!(
            client
                .complexity_hotspots("idx", None)
                .await
                .expect("no-op")
                .is_empty(),
            "complexity_hotspots must be a harmless empty no-op"
        );
        assert!(
            client.smells("idx").await.expect("no-op").is_empty(),
            "smells must be a harmless empty no-op"
        );
    }
}
