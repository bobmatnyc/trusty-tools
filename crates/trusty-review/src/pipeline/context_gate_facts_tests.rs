//! [`GateFacts`]: which dependency the gate found wanting (#9194).
//!
//! Why: a `Degraded(reason)` is one string, so the context-source ledger
//! could not tell a search outage from an analyze outage.
//! What: drives `preflight_context_detailed` with the gate tests' fakes and
//! checks the facts name the right dependency, and that the outcome is the
//! one `preflight_context` returns.
//! Test: this module.

use super::*;

/// The gate tests' config with an explicit index and both dependencies
/// opted out, so a down dependency degrades instead of skipping.
fn opted_out() -> ReviewConfig {
    let mut cfg = config();
    cfg.search_index = "main".to_string();
    cfg.context.require_search = Some(false);
    cfg.context.require_analyze = false;
    cfg
}

/// An analyze client whose readiness probe fails with a transport error.
struct RefusedAnalyze;

#[async_trait]
impl AnalyzeClient for RefusedAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Err(AnalyzeClientError::Transport("socket refused".to_string()))
    }
    async fn has_analysis(&self, _: &str) -> bool {
        false
    }
    async fn analysis_status(&self, _: &str) -> Result<(), AnalyzeClientError> {
        Err(AnalyzeClientError::Transport("socket refused".to_string()))
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

/// #9194 (amendment 6): a down search daemon names search, with the reason
/// the review is labelled with; analyze was never probed, so it is `None`.
#[tokio::test]
async fn facts_name_search_when_the_daemon_is_down() {
    let (outcome, facts) =
        preflight_context_detailed(&opted_out(), &deps(None, None), InvocationSurface::Hosted)
            .await;
    let GateOutcome::Degraded(reason) = outcome else {
        panic!("a down, opted-out search degrades: {outcome:?}");
    };
    assert_eq!(facts.search.as_deref(), Some(reason.as_str()));
    assert_eq!(facts.analyze, None);
}

/// #9194: a degraded or unreadable index names search, with that reason.
#[tokio::test]
async fn facts_name_the_index_reason_when_degraded() {
    let d = deps_with_search(
        Arc::new(TargetIndexSearch {
            health_status: "ok",
            summary: None,
            probe: IndexProbe::ProbeFailed,
        }),
        true,
    );
    let (outcome, facts) =
        preflight_context_detailed(&opted_out(), &d, InvocationSurface::Hosted).await;
    let GateOutcome::Degraded(reason) = outcome else {
        panic!("an unreadable index status degrades: {outcome:?}");
    };
    assert_eq!(facts.search.as_deref(), Some(reason.as_str()));
    assert!(reason.contains("could not be read"), "{reason}");
    assert_eq!(facts.analyze, None);
}

/// #9194: an analyze outage names analyze, not search, and carries the
/// probe's own reason.
#[tokio::test]
async fn facts_name_analyze_when_it_is_down() {
    let (outcome, facts) = preflight_context_detailed(
        &opted_out(),
        &deps(Some(true), Some(false)),
        InvocationSurface::Hosted,
    )
    .await;
    assert!(matches!(outcome, GateOutcome::Degraded(_)), "{outcome:?}");
    assert_eq!(facts.search, None);
    let analyze = facts.analyze.expect("analyze is named");
    assert!(analyze.contains("trusty-analyze unavailable"), "{analyze}");
    assert!(
        analyze.contains("no analysis for index `main`"),
        "{analyze}"
    );
}

/// #9194 (review): the analyze fact carries the transport error, which a
/// bare `has_analysis` bool dropped.
#[tokio::test]
async fn facts_carry_the_analyze_transport_error() {
    let d = ReviewDeps {
        analyze: Some(Arc::new(RefusedAnalyze)),
        ..deps(Some(true), None)
    };
    let (_, facts) = preflight_context_detailed(&opted_out(), &d, InvocationSurface::Hosted).await;
    let analyze = facts.analyze.expect("analyze is named");
    assert!(analyze.contains("socket refused"), "{analyze}");
}

/// #9194: a gate that proceeds names nothing.
#[tokio::test]
async fn facts_are_empty_when_the_gate_proceeds() {
    let (outcome, facts) = preflight_context_detailed(
        &opted_out(),
        &deps(Some(true), Some(true)),
        InvocationSurface::Hosted,
    )
    .await;
    assert_eq!(outcome, GateOutcome::Proceed);
    assert_eq!(facts, GateFacts::default());
}

/// #9194: the detailed gate decides exactly as `preflight_context` does,
/// across the require/reachable/surface combinations the gate tests cover.
#[tokio::test]
async fn preflight_context_and_detailed_return_the_same_outcome() {
    let mut strict = config();
    strict.search_index = "main".to_string();
    let mut no_index = opted_out();
    no_index.search_index = String::new();
    let health = [Some(true), Some(false), None];
    let analyze = [Some(true), Some(false), None];
    let surfaces = [InvocationSurface::Hosted, InvocationSurface::Interactive];
    for cfg in [&strict, &opted_out(), &no_index] {
        for (h, a, surface) in health
            .iter()
            .flat_map(|h| analyze.iter().map(move |a| (*h, *a)))
            .flat_map(|(h, a)| surfaces.iter().map(move |s| (h, a, *s)))
        {
            let d = deps(h, a);
            let plain = preflight_context(cfg, &d, surface).await;
            let (detailed, _) = preflight_context_detailed(cfg, &d, surface).await;
            assert_eq!(detailed, plain, "health {h:?}, analyze {a:?}, {surface:?}");
        }
    }
}

/// #9194: the default `analysis_status` asks `has_analysis` and names the
/// index when the answer is no.
#[tokio::test]
async fn analysis_status_default_names_the_index() {
    assert!(
        StubAnalyze { ready: true }
            .analysis_status("main")
            .await
            .is_ok()
    );
    let err = StubAnalyze { ready: false }
        .analysis_status("main")
        .await
        .expect_err("no analysis");
    assert!(
        err.to_string().contains("no analysis for index `main`"),
        "{err}"
    );
}
