//! No-op `SearchClient` used for the `--source-root` diff-only fallback (#2994).
//!
//! Why: when `--source-root <dir>` does not map to any registered
//! trusty-search index, `ReviewConfig::resolve_source_root` chooses the SAFE
//! fallback — diff-only review with a clear notice — rather than triggering an
//! ephemeral index (see that method's doc comment for the rationale, which
//! interacts with the #2914 ephemeral-index-leak bug). Simply relaxing
//! `context.require_search` is not enough on its own: if the trusty-search
//! daemon happens to be healthy, the required-context gate
//! (`pipeline::context_gate::preflight_context`) would proceed normally and
//! silently query WHATEVER index `search_index` happens to hold (a stale or
//! unrelated project) instead of degrading. Swapping in a `NullSearchClient`
//! guarantees the health probe fails, forcing the gate into its existing
//! `Degraded` path so the fallback is both safe and loudly labelled.
//!
//! What: `NullSearchClient` implements `SearchClient` such that `health()`
//! always returns `Err(SearchClientError::Unavailable(reason))` (the caller
//! supplies the `--source-root`-specific notice as `reason`); `list_indexes`
//! and `search` are harmless empty-result no-ops in case anything calls them
//! after the gate has already produced its verdict.
//!
//! Test: `null_search_client_health_is_unavailable`,
//! `null_search_client_list_and_search_are_empty`.

use async_trait::async_trait;

use super::health::HealthResponse;
use super::search_client::{IndexInfo, SearchClient, SearchClientError, SearchResult};

/// A `SearchClient` that always reports itself unavailable with a fixed reason.
///
/// Why: see module doc — this is the mechanism that turns a `--source-root`
/// with no registered index into a genuinely diff-only review instead of a
/// silent wrong-index query.
/// What: holds the human-readable `reason` surfaced through
/// `SearchClientError::Unavailable`.
/// Test: `null_search_client_health_is_unavailable`.
pub struct NullSearchClient {
    reason: String,
}

impl NullSearchClient {
    /// Construct a `NullSearchClient` that always fails health checks with
    /// `reason`.
    ///
    /// Why: the caller (`commands::run`/`commands::compare`) already computed
    /// the operator-facing notice explaining why context retrieval is
    /// unavailable; reusing that exact string keeps the warn-log message and
    /// the gate's degraded reason consistent.
    /// What: stores `reason` for use by `health()`.
    /// Test: `null_search_client_health_is_unavailable`.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl SearchClient for NullSearchClient {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Err(SearchClientError::Unavailable(self.reason.clone()))
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(Vec::new())
    }

    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(Vec::new())
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_search_client_health_is_unavailable() {
        let client = NullSearchClient::new("no index for --source-root /tmp/x");
        let err = client.health().await.expect_err("must report unavailable");
        assert!(matches!(err, SearchClientError::Unavailable(_)));
        assert!(
            err.to_string().contains("no index for --source-root"),
            "error must carry the caller-supplied reason: {err}"
        );
    }

    #[tokio::test]
    async fn null_search_client_list_and_search_are_empty() {
        let client = NullSearchClient::new("reason");
        assert!(
            client.list_indexes().await.expect("no-op").is_empty(),
            "list_indexes must be a harmless empty no-op"
        );
        assert!(
            client
                .search("idx", "query", None)
                .await
                .expect("no-op")
                .is_empty(),
            "search must be a harmless empty no-op"
        );
    }
}
