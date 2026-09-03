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
//! #6712: that decision was made for diagnostics alone, and every other endpoint
//! kept the 15 s constant on the premise that it read data already computed.
//! `analyze.complexity_distribution` does not — it pulls the whole chunk corpus
//! from trusty-search and grades every chunk, which measures 41–46 s on a
//! 104k-chunk index. So the endpoint that fills the §7 table timed out on every
//! run against a large repository, and the report said the distribution was
//! unavailable. The cost of a corpus scan is a property of the REPOSITORY, not
//! of this client, so its budget is configurable the way the diagnostics one is.
//!
//! What: [`AnalyzeEndpoint`] owns the path, the report-facing name, the
//! consequence of losing it, and the request budget. [`AnalyzeCaveat`] and
//! [`EndpointFailure`] carry a per-endpoint failure to the report's Gaps &
//! Caveats list, so one slow endpoint names itself instead of erasing the whole
//! fetch.
//!
//! Test: `analyze_adapter_tests.rs::{diagnostics_budget_outlives_the_daemon_deadline_ladder,
//! a_corpus_scanning_endpoint_outlives_the_probe_budget,
//! the_corpus_budget_falls_back_to_the_default, caveat_labels_are_stable}`.

use std::fmt;
use std::time::Duration;

// ─── Request budgets ─────────────────────────────────────────────────────────

/// Per-request budget for the readiness probe.
///
/// Why: `analyze.list_indexes` returns a registry the daemon already holds, so
/// it answers in milliseconds at any corpus size. Keeping it short is what stops
/// an unreachable daemon from holding a report open for minutes before the run
/// falls back to scan.
///
/// #6712: the histogram and the refactor list used to share this budget on the
/// premise that they too read computed data. They do not — see
/// [`DEFAULT_CORPUS_BUDGET`].
const PROBE_BUDGET: Duration = Duration::from_secs(15);

/// Per-request budget for the endpoints that scan the whole chunk corpus, when
/// the run configures none.
///
/// Why: `analyze.complexity_distribution` and `analyze.refactor_suggestions`
/// each fetch every chunk of the index from trusty-search and then grade it, so
/// their cost scales with the repository. Measured against the trusty-tools
/// checkout (104,433 chunks / 77,148 graded / 4,384 files) the histogram takes
/// 41–46 s — three times the 15 s these endpoints were given, so on a large
/// repository the §7 table was never filled (#6712). 180 s is four times the
/// measured cost, which leaves room for a corpus several times larger, and it
/// matches the default deadline trusty-analyze already gives its own long
/// operation. A starting point, not a measured optimum — the manifest key
/// `[report].analyze_timeout_secs` and the CLI's `--analyze-timeout-secs`
/// override it.
pub const DEFAULT_CORPUS_BUDGET: Duration = Duration::from_secs(180);

/// The budget a corpus-scanning endpoint gets, from what the run configured.
///
/// Why: taking the configured value as a parameter is what makes the budget
/// injectable — a test proves the ordering and the honouring at millisecond
/// scale instead of waiting out three minutes — and it is the same shape
/// [`diagnostics_budget_for`] takes for the same reason.
/// What: `None` and `Some(0)` both yield [`DEFAULT_CORPUS_BUDGET`]; zero would
/// time out every request instantly, which is never what a `0` in a manifest
/// means.
/// Test: `the_corpus_budget_falls_back_to_the_default`.
#[must_use]
pub fn corpus_budget_from_secs(configured: Option<u64>) -> Duration {
    configured
        .filter(|&s| s > 0)
        .map_or(DEFAULT_CORPUS_BUDGET, Duration::from_secs)
}

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
/// What: `method` and `params` build the request, `as_str` names the endpoint
/// in the report, `consequence` says what its absence costs the report, and
/// `budget` gives the per-request timeout.
/// Test: `diagnostics_budget_outlives_the_daemon_deadline_ladder`,
/// `caveat_labels_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AnalyzeEndpoint {
    /// `analyze.list_indexes` — the readiness probe.
    IndexList,
    /// `analyze.complexity_distribution` — the §7 histogram.
    ComplexityDistribution,
    /// `analyze.diagnostics` — the external static-analysis pass.
    Diagnostics,
    /// `analyze.refactor_suggestions` — maintainability findings.
    RefactorSuggestions,
}

impl AnalyzeEndpoint {
    /// The daemon method this endpoint calls.
    ///
    /// #6287 (ADR-0032): these were URL paths. trusty-analyze serves JSON-RPC
    /// over a Unix socket now, so an endpoint is a method name plus a params
    /// object — see [`Self::params`].
    ///
    /// The names are literals rather than imports from
    /// `trusty_analyze::service::rpc::METHODS`: this crate has no Cargo edge on
    /// the analysis daemon, and adding one to share four `&str`s would pull a
    /// tree-sitter engine into every `trusty-review report` build.
    pub fn method(self) -> &'static str {
        match self {
            Self::IndexList => "analyze.list_indexes",
            Self::ComplexityDistribution => "analyze.complexity_distribution",
            Self::Diagnostics => "analyze.diagnostics",
            Self::RefactorSuggestions => "analyze.refactor_suggestions",
        }
    }

    /// The params for this endpoint against `index_id`.
    ///
    /// `IndexList` takes none — it lists every index, and the caller filters.
    pub fn params(self, index_id: &str) -> serde_json::Value {
        match self {
            Self::IndexList => serde_json::Value::Null,
            _ => serde_json::json!({ "index_id": index_id }),
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

    /// The per-request timeout for this endpoint, given the run's `corpus`
    /// budget.
    ///
    /// Why: three costs, not two (#6712). The readiness probe reads a registry
    /// and answers in milliseconds. The histogram and the refactor list scan the
    /// whole corpus, so their cost is the repository's and the caller supplies
    /// it. Diagnostics additionally runs external tooling under the daemon's own
    /// deadline, so it takes the larger of that ladder and `corpus` — a run that
    /// raises the corpus budget for a big repository must not leave the one
    /// endpoint that spawns `cargo clippy` on a shorter leash, and taking the
    /// max is what keeps the #6041 ordering invariant intact while doing it.
    /// Test: `diagnostics_budget_outlives_the_daemon_deadline_ladder`,
    /// `a_corpus_scanning_endpoint_outlives_the_probe_budget`.
    pub fn budget(self, corpus: Duration) -> Duration {
        match self {
            Self::IndexList => PROBE_BUDGET,
            Self::ComplexityDistribution | Self::RefactorSuggestions => corpus,
            Self::Diagnostics => {
                diagnostics_budget_for(configured_diagnostics_deadline()).max(corpus)
            }
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
