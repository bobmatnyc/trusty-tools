//! Unit tests for the required-context preflight gate (#590).
//!
//! Why: split from `context_gate.rs` to keep that file focused while exhaustively
//! covering every (require × reachable) combination for both dependencies.
//! What: drives `preflight_context` with injected fake search/analyze clients and
//! a fake LLM (the gate ignores the LLM but `ReviewDeps` requires one).
//! Test: this is the test module.

use super::*;
use crate::{
    config::{InvocationSurface, ReviewConfig},
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
    // #6686: the per-index probe the gate decides on. This stand-in reports a
    // fully-ready index so the fake exercises the branch under test, not this one.
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        match self.health {
            Some(ok) => Ok(HealthResponse {
                status: if ok { "ok" } else { "starting" }.to_string(),
                embedder: EmbedderState::Bool(ok),
                warmboot_summary: None,
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
    assert_eq!(
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Proceed
    );
}

#[tokio::test]
async fn skips_when_search_down_and_required() {
    let cfg = config();
    let d = deps(None, Some(true)); // search errors, analyze ready
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
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
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Skip(_)
    ));
}

#[tokio::test]
async fn degraded_when_search_down_and_opted_out() {
    let mut cfg = config();
    cfg.context.require_search = Some(false);
    let d = deps(None, Some(true));
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[tokio::test]
async fn skips_when_analyze_down_and_required() {
    let cfg = config();
    let d = deps(Some(true), Some(false)); // search ok, analyze not ready
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
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
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Skip(_)
    ));
}

#[tokio::test]
async fn degraded_when_analyze_down_and_opted_out() {
    let mut cfg = config();
    cfg.context.require_analyze = false;
    let d = deps(Some(true), Some(false));
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-analyze"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[tokio::test]
async fn search_down_skip_takes_priority_over_analyze() {
    // Both down, both required → the search (more fundamental) skip wins.
    let cfg = config();
    let d = deps(None, Some(false));
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Skip(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected search Skip, got {other:?}"),
    }
}

#[tokio::test]
async fn both_opted_out_and_down_proceeds_degraded() {
    let mut cfg = config();
    cfg.context.require_search = Some(false);
    cfg.context.require_analyze = false;
    let d = deps(None, Some(false));
    // Search degraded reason takes priority (checked first).
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

// ── Per-surface default (search-unreachable semantics fix) ────────────────────

/// Interactive surfaces (MCP tool calls, CLI local-diff/--base/--source-root
/// reviews) default to DEGRADED rather than hard-Skip when search is down and
/// the operator has not set an explicit `require_search` override — neither
/// surface can post a context-free verdict to a real PR, so a diff-only review
/// is still useful.
#[tokio::test]
async fn interactive_surface_defaults_to_degraded_when_search_down() {
    let cfg = config(); // require_search: None (no explicit override)
    let d = deps(None, Some(true)); // search down, analyze ready
    match preflight_context(&cfg, &d, InvocationSurface::Interactive).await {
        GateOutcome::Degraded(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Degraded for Interactive surface, got {other:?}"),
    }
}

/// Hosted surfaces (the webhook bot, CLI GitHub-PR runs) default to hard-Skip
/// when search is down and unconfigured — unchanged from the pre-fix behaviour
/// (zero regression for the gate use case).
#[tokio::test]
async fn hosted_surface_defaults_to_skip_when_search_down() {
    let cfg = config(); // require_search: None (no explicit override)
    let d = deps(None, Some(true));
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Skip(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("expected Skip for Hosted surface, got {other:?}"),
    }
}

/// An explicit `require_search = false` override wins even on a `Hosted`
/// surface — the operator's config always beats the surface default.
#[tokio::test]
async fn explicit_optout_degrades_even_hosted_surface() {
    let mut cfg = config();
    cfg.context.require_search = Some(false);
    let d = deps(None, Some(true));
    assert!(matches!(
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Degraded(_)
    ));
}

/// An explicit `require_search = true` override wins even on an `Interactive`
/// surface — the operator's config always beats the surface default.
#[tokio::test]
async fn explicit_require_skips_even_interactive_surface() {
    let mut cfg = config();
    cfg.context.require_search = Some(true);
    let d = deps(None, Some(true));
    assert!(matches!(
        preflight_context(&cfg, &d, InvocationSurface::Interactive).await,
        GateOutcome::Skip(_)
    ));
}

// ── Degraded-but-serving (#3693) ───────────────────────────────────────────────

/// Search stub whose `/health` reports `status: "degraded"` with a caller-chosen
/// warm-boot summary, independent of the `Some(bool)`/`None` shape `StubSearch`
/// uses (#3693, #4086).
struct DegradedSearch {
    summary: crate::integrations::health::WarmBootSummary,
}

#[async_trait]
impl SearchClient for DegradedSearch {
    // #6686: the per-index probe the gate decides on. This stand-in reports a
    // fully-ready index so the fake exercises the branch under test, not this one.
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "degraded".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: Some(self.summary.clone()),
        })
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

fn deps_with_search(search: Arc<dyn SearchClient>, analyze_ready: bool) -> ReviewDeps {
    ReviewDeps {
        llm: Arc::new(StubLlm),
        verifier: None,
        search,
        analyze: Some(Arc::new(StubAnalyze {
            ready: analyze_ready,
        })),
        dedup: None,
    }
}

/// The exact #3693 scenario: trusty-search reports `status: "degraded"`
/// purely because its file watcher was auto-disabled on a network mount
/// (`warm_boot_degraded == false`) — search is fully functional, so the gate
/// must PROCEED (not skip), even with the default `require_search=true`.
#[tokio::test]
async fn degraded_but_serving_proceeds() {
    let cfg = config(); // defaults: require_search=true
    let d = deps_with_search(
        Arc::new(DegradedSearch {
            summary: Default::default(),
        }),
        true,
    );
    assert_eq!(
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Proceed,
        "degraded-but-serving (benign watcher disable) must proceed cleanly — a clean warm boot \
         behind a 'degraded' status affects nothing a reviewer can act on (#3693)"
    );
}

// ── Per-index gate (#6686) and unknown index (#6687) ──────────────────────────

/// What the per-index probe should answer for a given test.
enum IndexProbe {
    /// A fully-healthy index.
    Ready,
    /// A caller-chosen status payload.
    Status(Box<crate::integrations::search_client::IndexStatusResponse>),
    /// `404 unknown index` — trusty-search has never heard of it.
    Unknown,
    /// The daemon answered `/health` but the status probe itself failed.
    ProbeFailed,
}

/// Search stub with an independently-configurable `/health` and per-index
/// status, so a test can hold one constant and vary the other.
///
/// Why: #6686 is entirely about the two probes disagreeing — a host with a
/// failed index and a healthy target index, and a clean host with a failed
/// target index. A fake that derives one from the other cannot express either.
/// What: `health_status` / `summary` drive `/health`; `probe` drives
/// `index_status`.
/// Test: used by the `#6686` / `#6687` tests below.
struct TargetIndexSearch {
    health_status: &'static str,
    summary: Option<crate::integrations::health::WarmBootSummary>,
    probe: IndexProbe,
}

#[async_trait]
impl SearchClient for TargetIndexSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: self.health_status.to_string(),
            embedder: EmbedderState::Str("ready".to_string()),
            warmboot_summary: self.summary.clone(),
        })
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        match &self.probe {
            IndexProbe::Ready => {
                Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
            }
            IndexProbe::Status(s) => Ok((**s).clone()),
            IndexProbe::Unknown => Err(SearchClientError::Api {
                status: 404,
                body: format!(r#"{{"error":"unknown index: {index_id}"}}"#),
            }),
            IndexProbe::ProbeFailed => {
                Err(SearchClientError::Transport("connection reset".to_string()))
            }
        }
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

/// The exact live payload from the #6686 report: an unrelated index
/// (`workspace`) with a failed corpus and a failed lane, on a host serving 40
/// other indexes.
fn unrelated_failed_index_summary() -> crate::integrations::health::WarmBootSummary {
    crate::integrations::health::WarmBootSummary {
        indexes_loaded: 41,
        indexes_skipped_timeout: 11,
        indexes_corpus_failed: 1,
        indexes_stage_failed: 1,
        warm_boot_degraded: true,
        ..Default::default()
    }
}

/// REGRESSION (#6686): a review whose OWN index is healthy must be
/// authoritative, whatever some other index on the host is doing.
///
/// Why: this is the reported bug. `/health` counts registry handles and
/// discards index ids, so ONE broken index anywhere — here `workspace`, a
/// walk-budget refusal on an unrelated repo root — made the gate return
/// `Degraded` and stamp a NOT AUTHORITATIVE banner onto every review on the
/// host, including reviews of projects whose index was perfectly fine. The
/// pre-fix gate read `warmboot_summary` and had no way to ask about the index
/// under review.
/// What: a daemon reporting `status: "degraded"` with the live warm-boot
/// counters, paired with a per-index probe that says the target index is ready.
/// Asserts `Proceed` — no banner, no degraded label.
/// Test: this IS the test.
#[tokio::test]
async fn healthy_target_index_proceeds_despite_an_unrelated_failed_index() {
    let cfg = config(); // defaults: require_search=true
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "degraded",
            summary: Some(unrelated_failed_index_summary()),
            probe: IndexProbe::Ready,
        }),
        true,
    );
    assert_eq!(
        preflight_context(&cfg, &d, InvocationSurface::Hosted).await,
        GateOutcome::Proceed,
        "#6686: the index under review is healthy — an unrelated index's warm-boot failure must \
         not degrade this review or stamp a NOT AUTHORITATIVE banner on it"
    );
}

/// REGRESSION (#6686): the banner reason comes from the index under review, and
/// says what that index's own status payload says.
///
/// Why: the counter arithmetic it replaces emitted "queries return LEXICAL
/// results only" for `workspace` — an index whose lexical lane had ALSO failed
/// and whose `search_capabilities` was `[]`. A reader acting on that reason
/// would believe a lane that was dead. The host here is spotless (`status: "ok"`,
/// no warm-boot summary at all), so the pre-fix gate had nothing to report and
/// returned `Proceed`: this test fails in BOTH directions against it.
/// What: a clean `/health` paired with a per-index probe reporting all three
/// lanes failed and no capabilities. Asserts `Degraded`, that the reason names
/// the index and each failed lane, and that it does not claim lexical results.
/// Test: this IS the test.
#[tokio::test]
async fn degraded_target_index_reason_comes_from_the_per_index_probe() {
    let cfg = config();
    let failed: crate::integrations::search_client::IndexStatusResponse = serde_json::from_str(
        r#"{
            "index_id": "workspace",
            "search_capabilities": [],
            "stages": {
                "lexical": {"status": "failed", "failure": "walk budget refused"},
                "semantic": {"status": "failed"},
                "graph": {"status": "failed"}
            }
        }"#,
    )
    .expect("fixture must parse");
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "ok",
            summary: None,
            probe: IndexProbe::Status(Box::new(failed)),
        }),
        true,
    );
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Degraded(reason) => {
            assert!(
                reason.contains("workspace"),
                "the reason must name the index under review; got: {reason}"
            );
            for lane in ["lexical", "semantic", "graph"] {
                assert!(
                    reason.contains(lane),
                    "the reason must name the failed {lane} lane; got: {reason}"
                );
            }
            assert!(
                !reason.contains("LEXICAL results only"),
                "#6686: this index's lexical lane failed — the reason must never claim lexical \
                 results are available; got: {reason}"
            );
            assert!(
                reason.contains("trusty-search at"),
                "the reason must say which daemon it is about; got: {reason}"
            );
        }
        other => panic!(
            "#6686: a failed target index must label the review, got {other:?} — a spotless \
             /health is exactly what let this through before"
        ),
    }
}

/// REGRESSION (#6687): an index trusty-search has never heard of is a loud skip
/// that names it, not a review with no context.
///
/// Why: the resolver falls back to the default index id (`main`) for a checkout
/// whose only registered indexes are `.worktrees/*` descendants. `POST
/// /indexes/main/search` then answers `404 {"error":"unknown index: main"}`,
/// `runner_context` collapsed that into `Vec::new()`, and the review published
/// an AUTHORITATIVE verdict having read none of the project. The pre-fix gate
/// never asked, so it returned `Proceed`.
/// What: a healthy daemon whose per-index probe 404s. Asserts `Skip` and that
/// the message names the index id that was tried.
/// Test: this IS the test.
#[tokio::test]
async fn unknown_index_skips_and_names_the_index() {
    let cfg = config();
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "ok",
            summary: None,
            probe: IndexProbe::Unknown,
        }),
        true,
    );
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Skip(reason) => {
            assert!(
                reason.contains(&format!("`{}`", cfg.search_index)),
                "#6687: the skip must name the index it tried to query; got: {reason}"
            );
            assert!(
                reason.contains("no code context"),
                "the skip must say why no verdict is possible; got: {reason}"
            );
        }
        other => panic!(
            "#6687: a missing index must skip, got {other:?} — proceeding here is a verdict \
             produced without reading the project"
        ),
    }
}

/// A missing index is not opt-out-able.
///
/// Why: `require_search=false` says "review anyway if the DAEMON is down" —
/// a degraded run still reads whatever the daemon can give. A missing index
/// gives nothing at all, on every query, and there is no degraded version of
/// that. If the opt-out relaxed this too, #6687 would reopen for every
/// interactive caller, whose surface default is exactly that opt-out.
/// Test: this IS the test.
#[tokio::test]
async fn unknown_index_skips_even_when_search_is_opted_out() {
    let mut cfg = config();
    cfg.context.require_search = Some(false);
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "ok",
            summary: None,
            probe: IndexProbe::Unknown,
        }),
        true,
    );
    match preflight_context(&cfg, &d, InvocationSurface::Interactive).await {
        GateOutcome::Skip(reason) => assert!(
            reason.contains(&format!("`{}`", cfg.search_index)),
            "got: {reason}"
        ),
        other => panic!(
            "#6687: require_search=false degrades a daemon outage, not a missing \
             index — got {other:?}"
        ),
    }
}

/// A status probe that could not complete degrades; it does not skip.
///
/// Why: the daemon answered `/health`, so it is up. Refusing every review over a
/// transient probe failure is the false-positive machine #4086 removed; running
/// one silently is the gap #6686 closed. Label it and carry on.
/// Test: this IS the test.
#[tokio::test]
async fn index_status_probe_failure_degrades_rather_than_skipping() {
    let cfg = config();
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "ok",
            summary: None,
            probe: IndexProbe::ProbeFailed,
        }),
        true,
    );
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Degraded(reason) => assert!(
            reason.contains("could not be read"),
            "the reason must say the per-index verdict is missing; got: {reason}"
        ),
        other => panic!("a probe failure on a live daemon must label, not skip: {other:?}"),
    }
}

/// A trusty-search that cannot serve at all (embedder not ready) must STILL
/// fail-closed under the default `require_search=true`.
///
/// Why: #4086 relaxes the warm-boot gap, and the risk of that change is
/// relaxing the genuine outage too. Without an embedder there is no semantic
/// code context to be had, so there is nothing to label — the review must not
/// run. This test is the guard on that boundary.
/// Test: this test itself.
#[tokio::test]
async fn not_serving_search_still_skips() {
    struct EmbedderDownSearch;

    #[async_trait]
    impl SearchClient for EmbedderDownSearch {
        // #6686: the per-index probe the gate decides on. This stand-in reports a
        // fully-ready index so the fake exercises the branch under test, not this one.
        async fn index_status(
            &self,
            index_id: &str,
        ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError>
        {
            Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
        }

        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Ok(HealthResponse {
                status: "ok".to_string(),
                embedder: EmbedderState::Str("loading".to_string()),
                warmboot_summary: None,
            })
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

    let cfg = config();
    let d = deps_with_search(Arc::new(EmbedderDownSearch), true);
    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
        GateOutcome::Skip(msg) => assert!(msg.contains("trusty-search"), "msg: {msg}"),
        other => panic!("a search that cannot serve at all must Skip, got {other:?}"),
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
        // #6686: the per-index probe the gate decides on. This stand-in reports a
        // fully-ready index so the fake exercises the branch under test, not this one.
        async fn index_status(
            &self,
            index_id: &str,
        ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError>
        {
            Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
        }

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
    cfg.context.require_search = Some(false); // the --source-root diff-only fallback clears this
    let d = ReviewDeps {
        llm: Arc::new(StubLlm),
        verifier: None,
        search: Arc::new(SourceRootNoticeSearch),
        analyze: Some(Arc::new(StubAnalyze { ready: true })),
        dedup: None,
    };

    match preflight_context(&cfg, &d, InvocationSurface::Hosted).await {
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
