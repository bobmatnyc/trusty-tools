//! Unit tests for the required-context preflight gate (#590).
//!
//! Why: split from `context_gate.rs` to keep that file focused while exhaustively
//! covering every (require × reachable) combination for both dependencies.
//! What: drives `preflight_context` with injected fake search/analyze clients and
//! a fake LLM (the gate ignores the LLM but `ReviewDeps` requires one).
//! Test: this is the test module.

use super::*;
use crate::{
    config::ReviewConfig,
    integrations::{
        analyze_client::{
            AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
        },
        search_client::{
            EmbedderState, HealthResponse, IndexInfo, SearchClient, SearchClientError, SearchResult,
        },
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    pipeline::runner::ReviewDeps,
};
use async_trait::async_trait;
use std::sync::Arc;

// ── Fakes ─────────────────────────────────────────────────────────────────────

struct StubLlm;

#[async_trait]
impl LlmProvider for StubLlm {
    fn name(&self) -> &str {
        "stub"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("unused".to_string()))
    }
}

/// Search client whose health is configurable: healthy, unhealthy, or erroring.
struct StubSearch {
    /// `Some(true)` → status "ok"; `Some(false)` → status "starting";
    /// `None` → transport error from `health()`.
    health: Option<bool>,
}

#[async_trait]
impl SearchClient for StubSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        match self.health {
            Some(ok) => Ok(HealthResponse {
                status: if ok { "ok" } else { "starting" }.to_string(),
                embedder: EmbedderState::Bool(ok),
            }),
            None => Err(SearchClientError::Unavailable("down".to_string())),
        }
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }
    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

/// Analyze client whose readiness is a fixed boolean.
struct StubAnalyze {
    ready: bool,
}

#[async_trait]
impl AnalyzeClient for StubAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Ok(AnalyzeHealthResponse {
            status: "ok".to_string(),
            search_reachable: true,
        })
    }
    async fn has_analysis(&self, _: &str) -> bool {
        self.ready
    }
    async fn complexity_hotspots(
        &self,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(vec![])
    }
    async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(vec![])
    }
}

fn deps(search_health: Option<bool>, analyze_ready: Option<bool>) -> ReviewDeps {
    ReviewDeps {
        llm: Arc::new(StubLlm),
        verifier: None,
        search: Arc::new(StubSearch {
            health: search_health,
        }),
        analyze: analyze_ready
            .map(|r| Arc::new(StubAnalyze { ready: r }) as Arc<dyn AnalyzeClient>),
        dedup: None,
    }
}

fn config() -> ReviewConfig {
    ReviewConfig::load(None)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn proceeds_when_both_healthy() {
    let cfg = config(); // defaults: both required
    let d = deps(Some(true), Some(true));
    assert_eq!(preflight_context(&cfg, &d).await, GateOutcome::Proceed);
}

#[tokio::test]
async fn skips_when_search_down_and_required() {
    let cfg = config();
    let d = deps(None, Some(true)); // search errors, analyze ready
    match preflight_context(&cfg, &d).await {
        GateOutcome::Skip(msg) => {
            assert!(msg.contains("trusty-search"), "msg: {msg}");
            assert!(msg.contains("start"), "msg must be actionable: {msg}");
        }
        other => panic!("expected Skip, got {other:?}"),
    }
}

#[tokio::test]
async fn skips_when_search_unhealthy_and_required() {
    let cfg = config();
    let d = deps(Some(false), Some(true)); // search status != "ok"
    assert!(matches!(
        preflight_context(&cfg, &d).await,
        GateOutcome::Skip(_)
    ));
}

#[tokio::test]
async fn degraded_when_search_down_and_opted_out() {
    let mut cfg = config();
    cfg.context.require_search = false;
    let d = deps(None, Some(true));
    match preflight_context(&cfg, &d).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[tokio::test]
async fn skips_when_analyze_down_and_required() {
    let cfg = config();
    let d = deps(Some(true), Some(false)); // search ok, analyze not ready
    match preflight_context(&cfg, &d).await {
        GateOutcome::Skip(msg) => {
            assert!(msg.contains("trusty-analyze"), "msg: {msg}");
            assert!(msg.contains("start"), "msg must be actionable: {msg}");
        }
        other => panic!("expected Skip, got {other:?}"),
    }
}

#[tokio::test]
async fn skips_when_analyze_absent_and_required() {
    let cfg = config();
    let d = deps(Some(true), None); // no analyze client at all
    assert!(matches!(
        preflight_context(&cfg, &d).await,
        GateOutcome::Skip(_)
    ));
}

#[tokio::test]
async fn degraded_when_analyze_down_and_opted_out() {
    let mut cfg = config();
    cfg.context.require_analyze = false;
    let d = deps(Some(true), Some(false));
    match preflight_context(&cfg, &d).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-analyze"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[tokio::test]
async fn search_down_skip_takes_priority_over_analyze() {
    // Both down, both required → the search (more fundamental) skip wins.
    let cfg = config();
    let d = deps(None, Some(false));
    match preflight_context(&cfg, &d).await {
        GateOutcome::Skip(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected search Skip, got {other:?}"),
    }
}

#[tokio::test]
async fn both_opted_out_and_down_proceeds_degraded() {
    let mut cfg = config();
    cfg.context.require_search = false;
    cfg.context.require_analyze = false;
    let d = deps(None, Some(false));
    // Search degraded reason takes priority (checked first).
    match preflight_context(&cfg, &d).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[test]
fn degraded_banner_contains_warning() {
    let banner = degraded_banner("trusty-search unavailable at http://x");
    assert!(banner.contains("DEGRADED"));
    assert!(banner.contains("NOT AUTHORITATIVE"));
    assert!(banner.contains("trusty-search unavailable at http://x"));
}

/// #2994 re-review, finding #2: the Degraded reason must surface the health
/// probe's OWN error text (e.g. a `NullSearchClient`'s `--source-root`-specific
/// notice) rather than a hardcoded generic message that discards it.
///
/// Why: previously `preflight_context` only `warn!`-logged the health error
/// and built a fixed "trusty-search unavailable at {url}" string for the
/// Degraded reason, so the actionable `--source-root` notice (e.g. "Run
/// `trusty-search index <dir>`") never reached `degraded_banner` / the
/// persisted review body — contradicting README.md's claim that the notice is
/// prepended as a banner.
/// What: uses a search stub whose `health()` fails with a distinctive,
/// source-root-shaped error message; asserts the Degraded reason contains
/// that exact text.
/// Test: this test.
#[tokio::test]
async fn degraded_reason_prefers_health_error_detail() {
    struct SourceRootNoticeSearch;

    #[async_trait]
    impl SearchClient for SourceRootNoticeSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Err(SearchClientError::Unavailable(
                "--source-root /tmp/proj has no registered trusty-search index — proceeding in \
                 diff-only mode. Run `trusty-search index /tmp/proj` to enable full context"
                    .to_string(),
            ))
        }
        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Ok(vec![])
        }
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Ok(vec![])
        }
    }

    let mut cfg = config();
    cfg.context.require_search = false; // the --source-root diff-only fallback clears this
    let d = ReviewDeps {
        llm: Arc::new(StubLlm),
        verifier: None,
        search: Arc::new(SourceRootNoticeSearch),
        analyze: Some(Arc::new(StubAnalyze { ready: true })),
        dedup: None,
    };

    match preflight_context(&cfg, &d).await {
        GateOutcome::Degraded(msg) => {
            assert!(
                msg.contains("Run `trusty-search index /tmp/proj`"),
                "Degraded reason must carry the source-root-specific notice text, not a \
                 generic message: {msg}"
            );
        }
        other => panic!("expected Degraded, got {other:?}"),
    }
}
