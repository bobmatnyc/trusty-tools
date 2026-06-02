//! Required-context preflight gate (#590).
//!
//! Why: trusty-review's entire value is the context it injects from trusty-search
//! (code context) and trusty-analyze (static analysis).  A review produced
//! WITHOUT that context is actively harmful — it gives false confidence from a
//! verdict that never saw the project.  So before any review subject (PR review,
//! local-diff review, and the forward-compatible commit-review of #589) gathers
//! context, this gate probes both dependencies.  If a REQUIRED dependency is
//! unreachable the review is SKIPPED loudly with an actionable error; if an
//! operator explicitly opted out (`require_*` = false) the run proceeds but is
//! tagged DEGRADED / non-authoritative.
//!
//! What: `preflight_context` probes `SearchClient::health` and
//! `AnalyzeClient::has_analysis` concurrently and folds the two `require_*` flags
//! into a single `GateOutcome`: `Proceed`, `Skip(reason)`, or `Degraded(reason)`.
//! The gate lives here (not inline in the runner) so every subject goes through
//! the same code path and `runner.rs` stays under the 500-line cap.
//!
//! Test: `gate_tests.rs` drives every (require × reachable) combination with
//! injected fakes; the `#[ignore]`-free unit tests need no network.

use tracing::{info, warn};

use crate::{config::ReviewConfig, pipeline::runner::ReviewDeps};

/// Decision produced by the required-context preflight gate.
///
/// Why: the runner needs a single typed verdict to decide whether to abort the
/// review (skip), proceed with a loud non-authoritative label (degraded), or run
/// normally — without re-deriving the `require_*` logic at the call-site.
/// What: `Skip`/`Degraded` carry a human-readable, actionable reason string
/// (which daemon is down and how to start it).
/// Test: `gate_tests::*` assert the variant for each input combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// All required context dependencies are reachable — run a normal review.
    Proceed,
    /// A REQUIRED dependency is unavailable — skip the review (no verdict).  The
    /// string is an actionable operator-facing message.
    Skip(String),
    /// A dependency is unavailable but the operator opted out of requiring it —
    /// proceed with a DEGRADED, explicitly non-authoritative review.  The string
    /// names what context is missing.
    Degraded(String),
}

/// Probe the required context dependencies and decide whether to proceed.
///
/// Why: enforces the #590 contract — both trusty-search and trusty-analyze are
/// REQUIRED by default; a missing one skips the review rather than silently
/// degrading to a context-free, false-confidence verdict.  Running this once,
/// before context gathering, makes the policy apply uniformly to every review
/// subject.
/// What: probes search health and analyze readiness concurrently.  For each
/// dependency that is down: if its `require_*` flag is true → `Skip` with an
/// actionable message; if false → record a degraded reason.  Search is checked
/// first so its (more fundamental) outage produces the skip message.  When no
/// dependency is required-and-down but at least one opted-out dependency is down,
/// returns `Degraded`; otherwise `Proceed`.  Note: when `deps.analyze` is `None`
/// (analyze client not wired in at all, e.g. the CLI compare path) the analyze
/// requirement is treated as unmet exactly as if the daemon were down.
/// Test: `gate_tests::{skips_when_search_down, degraded_when_search_down_optout,
/// skips_when_analyze_down, proceeds_when_both_healthy}`.
pub async fn preflight_context(config: &ReviewConfig, deps: &ReviewDeps) -> GateOutcome {
    let search_url = &config.search_url;
    let analyzer_url = &config.analyzer_url;
    let index = &config.search_index;

    // Probe both dependencies concurrently — context retrieval is latency
    // sensitive and these are independent network calls.
    let search_fut = async { deps.search.health().await };
    let analyze_fut = async {
        match deps.analyze.as_ref() {
            Some(a) => a.has_analysis(index).await,
            // No analyze client wired in at all — treat as "no analysis".
            None => false,
        }
    };
    let (search_health, analyze_ready) = tokio::join!(search_fut, analyze_fut);

    let search_ok = match &search_health {
        Ok(h) if h.is_healthy() => true,
        Ok(h) => {
            warn!(status = %h.status, "trusty-search health is not 'ok'");
            false
        }
        Err(e) => {
            warn!("trusty-search health probe failed: {e}");
            false
        }
    };

    // ── trusty-search gate (checked first: it is the more fundamental dep) ──
    if !search_ok {
        if config.context.require_search {
            return GateOutcome::Skip(format!(
                "trusty-search unreachable at {search_url} — start it (`trusty-search start`); \
                 refusing to review without code context (set \
                 TRUSTY_REVIEW_REQUIRE_SEARCH=false or [context] require_search=false to opt \
                 into a degraded, non-authoritative review)"
            ));
        }
        info!(
            "trusty-search unavailable but require_search=false — proceeding DEGRADED (non-authoritative)"
        );
        return GateOutcome::Degraded(format!(
            "trusty-search unavailable at {search_url}; review produced WITHOUT code context"
        ));
    }

    // ── trusty-analyze gate ────────────────────────────────────────────────
    if !analyze_ready {
        if config.context.require_analyze {
            return GateOutcome::Skip(format!(
                "trusty-analyze unreachable/not-ready at {analyzer_url} — start it \
                 (`trusty-analyze serve`) and index `{index}`; refusing to review without \
                 static-analysis context (set TRUSTY_REVIEW_REQUIRE_ANALYZE=false or \
                 [context] require_analyze=false to opt into a degraded, non-authoritative review)"
            ));
        }
        info!(
            "trusty-analyze unavailable but require_analyze=false — proceeding DEGRADED (non-authoritative)"
        );
        return GateOutcome::Degraded(format!(
            "trusty-analyze unavailable at {analyzer_url}; review produced WITHOUT \
             static-analysis context"
        ));
    }

    GateOutcome::Proceed
}

/// Prominent banner prepended to a degraded review body so the verdict is never
/// mistaken for an authoritative one.
///
/// Why: the #590 premise forbids a degraded review masquerading as a normal
/// verdict.  Embedding a loud warning in the rendered body (in addition to the
/// `status` field and the `error` reason) makes the non-authoritativeness visible
/// to any human reading the review markdown, not just to programmatic consumers.
/// What: returns a Markdown blockquote warning that names the missing context.
/// Test: `gate_tests::degraded_banner_contains_warning`.
pub fn degraded_banner(reason: &str) -> String {
    format!(
        "> ⚠️ **DEGRADED REVIEW — NOT AUTHORITATIVE**\n>\n> {reason}.\n> \
         This review ran WITHOUT required project context and must not be treated \
         as a trustworthy verdict. Start the missing daemon and re-run for an \
         authoritative review.\n\n"
    )
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "context_gate_tests.rs"]
mod gate_tests;
