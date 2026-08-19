//! The trusty-analyze endpoints `--analyze` fetches: their paths, their
//! per-request budgets, and the report-facing vocabulary for the ones that fail.
//!
//! Why: the adapter used one 15 s budget for every endpoint, which is shorter
//! than trusty-analyze's own deadline for `/diagnostics`. That daemon runs
//! project-scoped clippy under a 180 s cooperative deadline and answers 200 at
//! 142 s in the field, so the client gave up on a request the daemon went on to
//! answer. A client timeout below the server's own deadline is the exact
//! inversion #6034 fixed inside trusty-analyze: whoever gives up first decides
//! what the caller sees, and only the daemon's answer carries which tools ran
//! and what they found. The budgets therefore live beside the paths they apply
//! to, so adding an endpoint forces a decision about its cost.
//!
//! What: [`AnalyzeEndpoint`] owns the path, the report-facing name, the
//! consequence of losing it, and the request budget. [`AnalyzeCaveat`] and
//! [`EndpointFailure`] carry a per-endpoint failure to the report's Gaps &
//! Caveats list, so one slow endpoint names itself instead of erasing the whole
//! fetch.
//!
//! Test: `analyze_adapter_tests.rs::{diagnostics_budget_outlives_the_daemon_
//! deadline_ladder, caveat_labels_are_stable}`.

use std::fmt;
use std::time::Duration;

// ─── Request budgets ─────────────────────────────────────────────────────────

/// Per-request budget for the endpoints that answer from data already computed.
///
/// Why: `/indexes`, the complexity histogram, and the refactor list are reads
/// over an in-memory corpus and answer in seconds. A short budget on them is
/// what keeps an unreachable daemon from stalling a report for minutes.
const CHEAP_BUDGET: Duration = Duration::from_secs(15);

/// The environment variable trusty-analyze reads for its diagnostics deadline.
///
/// Why: the daemon's budget is operator-tunable, so a client budget pinned to
/// the default would fall back below the deadline the moment anyone raised it.
/// Reading the same variable keeps the ordering invariant at every configured
/// value rather than only at the default.
const DIAGNOSTICS_DEADLINE_ENV: &str = "TRUSTY_DIAGNOSTICS_DEADLINE_SECS";

/// trusty-analyze's diagnostics deadline when the operator sets none.
///
/// Pinned to `DEFAULT_DIAGNOSTICS_DEADLINE_SECS` in
/// `trusty-analyze/src/core/deadlines.rs`. It is a copy, not a shared constant:
/// trusty-analyze optionally depends on this crate through its `review` feature,
/// so a library dependency the other way is a cargo cycle.
const DEFAULT_DIAGNOSTICS_DEADLINE: Duration = Duration::from_secs(180);

/// Total headroom trusty-analyze stacks above its own cooperative deadline
/// before anything gives up.
///
/// Why: the daemon's ladder is `deadline` → `+30 s` handler grace (a 504 with a
/// JSON body naming what was cut off) → `+30 s` router `TimeoutLayer` (an
/// empty-bodied 504) → `+30 s` MCP client. A client that gives up at or below
/// the router rung turns every one of those structured answers into a bare
/// transport error, which is what #6034 fixed for the MCP path. This client
/// takes the same outermost rung, so the daemon's own response — including its
/// cutoff report — always arrives.
const SERVER_LADDER_HEADROOM: Duration = Duration::from_secs(90);

/// The diagnostics deadline this client should assume the daemon is using.
///
/// What: reads [`DIAGNOSTICS_DEADLINE_ENV`] exactly as trusty-analyze does,
/// falling back to [`DEFAULT_DIAGNOSTICS_DEADLINE`] on a missing, unparseable,
/// or zero value.
fn configured_diagnostics_deadline() -> Duration {
    std::env::var(DIAGNOSTICS_DEADLINE_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map_or(DEFAULT_DIAGNOSTICS_DEADLINE, Duration::from_secs)
}

/// The client budget for a diagnostics request against a daemon running
/// `deadline`.
///
/// Why: taking the deadline as a parameter is what makes the ordering invariant
/// testable across the whole configurable range without mutating process-wide
/// environment state — the same reason trusty-analyze's `handler_budget_for`
/// takes one.
/// Test: `diagnostics_budget_outlives_the_daemon_deadline_ladder`.
pub fn diagnostics_budget_for(deadline: Duration) -> Duration {
    deadline + SERVER_LADDER_HEADROOM
}

// ─── Endpoints ───────────────────────────────────────────────────────────────

/// One trusty-analyze endpoint the adapter fetches.
///
/// Why: the path, the budget, and the sentence a reader sees when it fails are
/// three facts about the same endpoint, and keeping them apart is how the
/// budget drifted from the cost in the first place.
/// What: `path` builds the request path, `as_str` names the endpoint in the
/// report, `consequence` says what its absence costs the report, and `budget`
/// gives the per-request timeout.
/// Test: `diagnostics_budget_outlives_the_daemon_deadline_ladder`,
/// `caveat_labels_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AnalyzeEndpoint {
    /// `GET /indexes` — the readiness probe.
    IndexList,
    /// `GET /indexes/{id}/complexity_distribution` — the §7 histogram.
    ComplexityDistribution,
    /// `GET /indexes/{id}/diagnostics` — the external static-analysis pass.
    Diagnostics,
    /// `GET /indexes/{id}/refactor-suggestions` — maintainability findings.
    RefactorSuggestions,
}

impl AnalyzeEndpoint {
    /// The request path for this endpoint against `index_id`.
    pub fn path(self, index_id: &str) -> String {
        match self {
            Self::IndexList => "/indexes".to_string(),
            Self::ComplexityDistribution => {
                format!("/indexes/{index_id}/complexity_distribution")
            }
            Self::Diagnostics => format!("/indexes/{index_id}/diagnostics"),
            Self::RefactorSuggestions => format!("/indexes/{index_id}/refactor-suggestions"),
        }
    }

    /// The endpoint's name as the report states it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IndexList => "index list",
            Self::ComplexityDistribution => "complexity distribution",
            Self::Diagnostics => "diagnostics",
            Self::RefactorSuggestions => "refactor suggestions",
        }
    }

    /// What the report loses when this endpoint does not answer.
    ///
    /// Why: a named gap that only says which endpoint failed still leaves the
    /// reader to guess which section is now unassessed. Every phrase here
    /// refuses to let an emptied section read as a clean one.
    fn consequence(self) -> &'static str {
        match self {
            Self::IndexList => {
                "no dimension of this repository was assessed — the report falls back to the \
                 repository scan alone"
            }
            Self::ComplexityDistribution => {
                "the §7 complexity table is omitted rather than filled from a truncated top-N \
                 sample, which is not a distribution"
            }
            Self::Diagnostics => {
                "the RED/CRITICAL defect band is populated only from external static analysis, \
                 so an empty band here means unassessed, not clean"
            }
            Self::RefactorSuggestions => {
                "the maintainability findings are unlisted, which means unassessed, not clean"
            }
        }
    }

    /// The per-request timeout for this endpoint.
    ///
    /// Why: diagnostics is the one endpoint that runs external tooling — a
    /// project-scoped `cargo clippy` — under the daemon's own deadline, so its
    /// budget is derived from that ladder. Everything else reads memory and
    /// keeps the short budget, which is what stops a dead daemon from holding
    /// the report open for minutes.
    /// Test: `diagnostics_budget_outlives_the_daemon_deadline_ladder`.
    pub fn budget(self) -> Duration {
        match self {
            Self::Diagnostics => diagnostics_budget_for(configured_diagnostics_deadline()),
            _ => CHEAP_BUDGET,
        }
    }
}

// ─── Failure vocabulary ──────────────────────────────────────────────────────

/// How one endpoint failed, in terms a report reader can act on.
///
/// Why: "diagnostics did not answer" and "diagnostics ran out of time" point at
/// different remedies — a daemon to start versus a deadline to raise — and the
/// distinction survives into the artifact without carrying a URL or a response
/// body with it.
/// What: three categories the adapter can tell apart from its own error type.
/// Test: `caveat_labels_are_stable`, `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum EndpointFailure {
    /// The budget ran out, on either side of the connection.
    TimedOut,
    /// Nothing answered — the daemon was unreachable or the connection died.
    Unanswered,
    /// The daemon answered, but with something unusable (non-2xx, bad JSON).
    Rejected,
}

impl EndpointFailure {
    /// The clause the report uses for this failure.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimedOut => "did not answer within the time allowed",
            Self::Unanswered => "could not be reached",
            Self::Rejected => "returned an answer this report could not use",
        }
    }
}

/// A dimension the daemon answered incompletely, for one repository (#5317,
/// #5320, #6041).
///
/// Why: a fetch that succeeds is not the same as a fetch that answered
/// everything, and the difference is invisible on the page. An empty RED band
/// because no linter was installed reads exactly like a clean codebase; a
/// missing complexity table reads like a rendering slip. Both are facts the
/// report owes its reader, and both travel the same Gaps & Caveats path
/// [`AnalyzeGap`](super::analyze_adapter::AnalyzeGap) already uses.
/// What: one variant per condition the fetch can distinguish. The rendered
/// sentence is built from fixed per-endpoint and per-failure phrases, so it
/// names the endpoint and the reason while carrying no run-specific detail.
/// Test: `analyze_adapter_tests.rs::caveat_labels_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AnalyzeCaveat {
    /// One endpoint did not answer while others did.
    EndpointUnavailable(AnalyzeEndpoint, EndpointFailure),
    /// No external static-analysis tool was installed, so nothing could
    /// populate the RED/defect band.
    NoStaticAnalysisTools,
}

impl fmt::Display for AnalyzeCaveat {
    /// The report-facing sentence for this caveat.
    ///
    /// Why: read by a stranger to this toolchain, so it names the condition and
    /// what it means for the section, not the variant.
    /// What: deterministic across runs — every fragment is a fixed string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndpointUnavailable(endpoint, reason) => write!(
                f,
                "the analysis daemon's {} endpoint {} — {}",
                endpoint.as_str(),
                reason.as_str(),
                endpoint.consequence()
            ),
            Self::NoStaticAnalysisTools => f.write_str(
                "no external static-analysis tool was available to the analysis daemon — the \
                 RED/CRITICAL band is populated only from such tools, so an empty band here \
                 means unassessed, not clean",
            ),
        }
    }
}
