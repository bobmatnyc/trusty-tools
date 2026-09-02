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
//! What: `preflight_context` probes `SearchClient::health` (is the daemon up),
//! `SearchClient::index_status` (can the index under review answer, #6686), and
//! `AnalyzeClient::has_analysis` concurrently, then folds the two `require_*`
//! flags into a single `GateOutcome`: `Proceed`, `Skip(reason)`, or
//! `Degraded(reason)`.
//! The gate lives here (not inline in the runner) so every subject goes through
//! the same code path and `runner.rs` stays under the 500-line cap.
//!
//! Test: `gate_tests.rs` drives every (require × reachable) combination with
//! injected fakes; the `#[ignore]`-free unit tests need no network.

use tracing::{info, warn};

use crate::{
    config::{InvocationSurface, ReviewConfig},
    integrations::health::ServingState,
    pipeline::runner::ReviewDeps,
};

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
/// subject.  `surface` decides the search-specific SAFE DEFAULT when the
/// operator has not set an explicit `require_search` override (search-
/// unreachable semantics fix): `Hosted` callers (the webhook bot, CLI
/// GitHub-PR runs) stay strict; `Interactive` callers (MCP tool calls, CLI
/// local-diff/--base/--source-root reviews) default to degrading instead of
/// hard-skipping, since neither can post a context-free verdict to a real PR.
/// What: probes search health and analyze readiness concurrently.  For search,
/// the EFFECTIVE requirement is `config.context.effective_require_search(surface)`
/// (explicit override wins; otherwise the surface default); for analyze it is
/// the plain `config.context.require_analyze` (unaffected by `surface`, out of
/// scope for this fix).  When a dependency is down and required → `Skip` with
/// an actionable message; when down and not required → record a degraded
/// reason.  Search is checked first so its (more fundamental) outage produces
/// the skip message.  When no dependency is required-and-down but at least one
/// opted-out dependency is down, returns `Degraded`; otherwise `Proceed`.  Note:
/// when `deps.analyze` is `None` (analyze client not wired in at all, e.g. the
/// CLI compare path) the analyze requirement is treated as unmet exactly as if
/// the daemon were down.
///
/// The search Degraded reason prefers the health probe's own
/// `SearchClientError::Unavailable(reason)` text over the generic template
/// when the probe returned an error (rather than a non-"ok" status): this is
/// how a `NullSearchClient`'s `--source-root`-specific notice (issue #2994's
/// diff-only fallback) reaches the persisted review body via
/// `degraded_banner` — a generic "trusty-search unavailable at {url}" message
/// would otherwise discard that actionable text (re-review finding #2). This
/// composes with the surface default above: a `NullSearchClient` swapped in by
/// the `--source-root` fallback always fails its `health()` probe, so it
/// routes through this same Degraded branch (never `Proceed`) regardless of
/// whether `surface` is `Hosted` or `Interactive` — only the reason text and
/// the required-vs-degraded branch selection differ per surface.
///
/// #6686: degraded-ness is decided by `GET /indexes/{id}/status` for the index
/// under review, not by `/health`. `/health` counts registry handles and
/// discards index ids, so a single failed index anywhere on the host degraded
/// every review on it — including a review whose own index was healthy — and the
/// banner reason it produced was counter arithmetic that could be flatly wrong
/// about which lanes survived. `/health` now answers reachability only
/// (`HealthResponse::reachability_state`); the reason a reader sees comes from
/// the per-index probe and names that index and its actually-failed lanes.
///
/// #6687: an index trusty-search has never heard of is a `Skip` that names it,
/// and one that no `require_search` opt-out relaxes. Every search against such
/// an index answers `404 unknown index`; the alternative outcome is a review
/// that saw none of the project and published a verdict anyway.
/// Test: `gate_tests::{skips_when_search_down_and_required,
/// degraded_when_search_down_and_opted_out,
/// skips_when_analyze_down_and_required, proceeds_when_both_healthy,
/// interactive_surface_defaults_to_degraded_when_search_down,
/// hosted_surface_defaults_to_skip_when_search_down,
/// degraded_reason_prefers_health_error_detail,
/// degraded_but_serving_proceeds, not_serving_search_still_skips,
/// healthy_target_index_proceeds_despite_an_unrelated_failed_index,
/// degraded_target_index_reason_comes_from_the_per_index_probe,
/// unknown_index_skips_and_names_the_index,
/// unknown_index_skips_even_when_search_is_opted_out}`.
pub async fn preflight_context(
    config: &ReviewConfig,
    deps: &ReviewDeps,
    surface: InvocationSurface,
) -> GateOutcome {
    let search_url = &config.search_url;
    let index = &config.search_index;
    let require_search = config.context.effective_require_search(surface);

    // Probe the dependencies concurrently — context retrieval is latency
    // sensitive and these are independent network calls.
    // #6686: `index_status` is the probe that decides degraded-ness, scoped to
    // the index THIS review queries.
    let search_fut = async { deps.search.health().await };
    let index_fut = async { deps.search.index_status(index).await };
    let analyze_fut = async {
        match deps.analyze.as_ref() {
            Some(a) => a.has_analysis(index).await,
            // No analyze client wired in at all — treat as "no analysis".
            None => false,
        }
    };
    let (search_health, index_status, analyze_ready) =
        tokio::join!(search_fut, index_fut, analyze_fut);

    // Captures the health probe's own error text (e.g. a `NullSearchClient`'s
    // `--source-root` notice) so the Degraded branch below can surface it
    // verbatim instead of a generic message that discards it.
    //
    // Issue #3693: this used to be `h.is_healthy()`, i.e. a hard
    // `status == "ok"` string match. trusty-search 0.38.1 intentionally
    // reports `status: "degraded"` on EFS/NFS-mounted repos purely because it
    // auto-disabled its file watcher (a benign, OS-level capability gap —
    // search itself stays 100% functional), which made every review on such
    // a deployment fail-closed.
    //
    // #6686: `/health` now answers ONE question here — is the daemon reachable
    // and serving. It counts registry handles and discards index ids, so its
    // warm-boot counters are host-wide by construction, and branching on them
    // degraded every review on a host where any unrelated index had failed. The
    // index-scoped question moved to `index_status` below.
    let mut search_error_detail: Option<String> = None;
    let search_ok = match &search_health {
        Ok(h) => match h.reachability_state() {
            // `reachability_state` never returns `Degraded`; a daemon that
            // answers the probe and has an embedder is serving, whatever its
            // warm boot did.
            ServingState::Serving | ServingState::Degraded(_) => true,
            ServingState::NotServing(reason) => {
                warn!(status = %h.status, reason = %reason, "trusty-search health is not serving");
                search_error_detail = Some(reason);
                false
            }
        },
        Err(e) => {
            warn!("trusty-search health probe failed: {e}");
            search_error_detail = Some(e.to_string());
            false
        }
    };

    // ── trusty-search gate (checked first: it is the more fundamental dep) ──
    if !search_ok {
        if require_search {
            return GateOutcome::Skip(format!(
                "trusty-search unreachable at {search_url} — start it (`trusty-search start`); \
                 refusing to review without code context (set \
                 TRUSTY_REVIEW_REQUIRE_SEARCH=false or [context] require_search=false to opt \
                 into a degraded, non-authoritative review)"
            ));
        }
        info!(
            surface = ?surface,
            "trusty-search unavailable and require_search is not effectively true for this \
             surface (explicit opt-out or interactive-surface default) — proceeding DEGRADED \
             (non-authoritative)"
        );
        let reason = match search_error_detail {
            Some(detail) => format!("trusty-search unavailable at {search_url} — {detail}"),
            None => format!(
                "trusty-search unavailable at {search_url}; review produced WITHOUT code context"
            ),
        };
        return GateOutcome::Degraded(reason);
    }

    // ── per-index gate (#6686, #6687) ──────────────────────────────────────
    // The daemon is up. The remaining question is about ONE index: the one this
    // review queries. Three outcomes, and they are genuinely different:
    //   * the index does not exist        → Skip, naming it (#6687)
    //   * the index exists and is broken  → record a reason, review is labelled
    //   * the status probe itself failed  → record a reason, review is labelled
    let mut search_degraded_reason: Option<String> = None;
    match index_status {
        Ok(status) => match status.serving_state() {
            ServingState::Serving => {}
            // `IndexStatusResponse::serving_state` produces only `Serving` and
            // `Degraded`; a `NotServing` from a future revision is at least as
            // serious, so it takes the same labelled path rather than being
            // dropped.
            ServingState::Degraded(reason) | ServingState::NotServing(reason) => {
                warn!(
                    index = %index,
                    reason = %reason,
                    "the index under review is degraded — proceeding, review will be labelled \
                     DEGRADED (#6686)"
                );
                search_degraded_reason = Some(reason);
            }
        },
        // #6687: an unknown index is a configuration fault, not an outage the
        // operator can opt out of. Every search this review would issue returns
        // `404 unknown index`, `runner_context` used to turn that into an empty
        // result set, and the review published an AUTHORITATIVE verdict having
        // seen no code at all. There is no degraded version of that — skip, and
        // say which index was missing.
        Err(e) if e.is_unknown_index() => {
            warn!(index = %index, "trusty-search has no index `{index}` — skipping review");
            return GateOutcome::Skip(format!(
                "trusty-search at {search_url} has no index `{index}` — refusing to review with \
                 no code context. Every search against it returns `404 unknown index`, so the \
                 review would see none of the project. Index this checkout \
                 (`trusty-search index <repo-root>`) or point the review at an existing index \
                 (`[search] index` / TRUSTY_SEARCH_INDEX); `trusty-search list-indexes` shows \
                 what is registered. This is NOT opt-out-able: \
                 TRUSTY_REVIEW_REQUIRE_SEARCH=false degrades a daemon outage, not a missing \
                 index."
            ));
        }
        Err(e) => {
            warn!(index = %index, "index status probe failed: {e}");
            search_degraded_reason = Some(format!(
                "the status of index `{index}` could not be read ({e}) — the review ran without \
                 a per-index health verdict, so missing context would not have been detected"
            ));
        }
    }

    // ── trusty-analyze gate ────────────────────────────────────────────────
    if !analyze_ready {
        if config.context.require_analyze {
            // #4440: the old text told every operator to `trusty-analyze serve`.
            // That advice is irrelevant in the DEFAULT subprocess mode used by
            // `serve`/MCP and `run`, where no analyze daemon exists at all — it
            // sent people hunting for a daemon they were never supposed to be
            // running. Name the actual preconditions of each mode instead.
            //
            // #6287: the daemon ADDRESS is gone from the text too. Since
            // `build_review_state` moved onto the subprocess client, no
            // trusty-review path on this gate contacts an analyze daemon at all,
            // and the one path that still does — `report --analyze` — dials a
            // socket whose path it prints for itself.
            return GateOutcome::Skip(format!(
                "trusty-analyze static-analysis context is unavailable for index `{index}` — \
                 refusing to review without it. No analyze daemon is used: to fix this, start \
                 trusty-search at {search_url} and confirm it is SERVING (a `degraded` warm \
                 boot still counts as serving), then put a runnable `trusty-analyze` binary on \
                 PATH (override with TRUSTY_ANALYZE_BIN). (Set \
                 TRUSTY_REVIEW_REQUIRE_ANALYZE=false or [context] require_analyze=false to opt \
                 into a degraded, non-authoritative review.)"
            ));
        }
        info!(
            "trusty-analyze unavailable but require_analyze=false — proceeding DEGRADED (non-authoritative)"
        );
        return GateOutcome::Degraded(
            "trusty-analyze unavailable; review produced WITHOUT static-analysis context"
                .to_string(),
        );
    }

    // ── the index under review is serving-but-degraded (#4086, #6686) ──────
    // Checked last so a hard analyze outage still wins the reason slot. Search
    // answered and supplied context, so this is not a skip — but the gap must
    // reach the reader of the review, not just the daemon's own log.
    if let Some(reason) = search_degraded_reason {
        return GateOutcome::Degraded(format!("trusty-search at {search_url}: {reason}"));
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
