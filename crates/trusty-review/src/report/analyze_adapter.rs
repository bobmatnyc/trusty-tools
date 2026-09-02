//! Deterministic trusty-analyze → `AnalyzeMetrics` adapter (#2447, epic #2445).
//!
//! Why: a bare `report --analyze` must populate the metrics-driven sections
//! (the §7 complexity-distribution chart + RED/AMBER finding bands) from
//! trusty-analyze WITHOUT an LLM and WITHOUT a hand-authored metrics JSON. A
//! library dependency on trusty-analyze is impossible (a cargo cycle: analyze
//! already optionally depends on trusty-review via its `review` feature), so
//! this adapter is a thin JSON-RPC client over the analyze daemon's hardened
//! Unix socket plus a pure mapping from the daemon's wire JSON onto the report's
//! v0 [`AnalyzeMetrics`]. (#6287 retired the `127.0.0.1:7879` HTTP listener this
//! header used to name; there is no port and no discovery file — the socket path
//! is derived through `trusty_common::daemon_socket_path`.)
//!
//! #6350: nothing is resident on the far end of that socket any more. The
//! adapter starts the server itself, once per source, through
//! [`trusty_common::uds::OnDemandAnalyze`] — the same entry point every other
//! client uses, so a second client racing this one adopts the same process
//! rather than starting a second. A start that FAILS is not silent: it becomes
//! a `Transport` error, which prints the fallback line on stderr and lands in
//! the report's Gaps & Caveats as an unassessed dimension. Every
//! probe/fetch/parse failure is fail-open — the
//! adapter degrades to `None` and the report falls through to the built-in
//! scan; a missing analyze index is never an error. Since #6041 that degradation
//! is per endpoint rather than per repository: a dataset that fails leaves the
//! others' data in place and names itself under Gaps & Caveats. Only a fetch
//! where NO dataset answered falls all the way back to scan.
//!
//! What: [`AnalyzeMetricsSource`] is the injectable fetch seam;
//! [`HttpAnalyzeMetricsSource`] is the live implementation. The pure mapping
//! ([`map_metrics`], `complexity_buckets`, [`diagnostic_finding`],
//! [`refactor_finding`]) is unit-tested against fixture JSON with no live
//! daemon. `loc`/`counts` are deliberately left empty — the built-in scanner
//! owns those measured numbers.
//!
//! Test: `analyze_adapter_tests.rs` covers envelope parsing, the severity map,
//! the complexity-bucket thresholds, fail-open on malformed JSON, and the
//! per-endpoint budgets and independence (`a_failing_endpoint_keeps_what_the_
//! others_returned`, `a_fetch_where_no_endpoint_answered_falls_back_to_scan`).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::index_registry::resolve_report_index;
use super::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, MetricFinding, Severity,
};
use crate::integrations::search_client::IndexInfo;

// The per-endpoint paths, budgets, and failure vocabulary live one module over,
// beside each other, so an endpoint's cost and its timeout cannot drift apart.
pub use super::analyze_endpoints::{
    AnalyzeCaveat, AnalyzeEndpoint, EndpointFailure, diagnostics_budget_for,
};

// ─── Tunables ──────────────────────────────────────────────────────────────

/// Largest response frame the adapter will buffer, in bytes.
///
/// Why: `analyze.diagnostics` returns up to 500 `ToolDiagnostic` rows per page
/// and `analyze.refactor_suggestions` a ranked list, both of which can outgrow
/// `trusty_common::uds::MAX_FRAME_BYTES`'s 8 MiB control-plane default on a
/// large repository. 32 MiB matches what the daemon accepts on the request side
/// (`service::rpc::MAX_FRAME_BYTES`), so neither end refuses what the other
/// would send.
const MAX_RESPONSE_FRAME_BYTES: u64 = 32 * 1024 * 1024;

// ─── Error type ────────────────────────────────────────────────────────────

/// Errors produced while fetching analyze data. All are handled fail-open by
/// [`HttpAnalyzeMetricsSource::fetch`], which converts any `Err` to `None`.
///
/// Why: a typed error lets the fetch layer log the specific failure mode
/// (transport vs. non-2xx vs. parse) without string-matching, while the public
/// [`AnalyzeMetricsSource::fetch`] contract stays fail-open (`Option`).
/// What: transport, non-2xx API, and JSON-parse variants plus client-init.
/// Test: `fetch_returns_none_on_*` in the tests module exercise the fail-open
/// conversion; `error_display` covers the `Display` strings.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeAdapterError {
    /// HTTP transport failure (connection refused, DNS, a connection that died).
    #[error("trusty-analyze transport error: {0}")]
    Transport(String),

    /// The request outlived its per-endpoint budget.
    ///
    /// Separate from [`Self::Transport`] because the remedy differs: a timeout
    /// points at a deadline to raise, not a daemon to start, and the gap line
    /// says so.
    #[error("trusty-analyze request timed out: {0}")]
    Timeout(String),

    /// trusty-analyze answered with a JSON-RPC error frame.
    ///
    /// #6287: this was `Api { status: u16, body: String }` over HTTP. `code` is
    /// the JSON-RPC code, which carries the same distinction the status did —
    /// [`classify_failure`] reads it to tell a refusal from an outage.
    #[error("trusty-analyze returned error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// The daemon's own message.
        message: String,
    },

    /// Response JSON could not be parsed against the expected envelope.
    #[error("trusty-analyze response parse error: {0}")]
    Parse(String),
}

type AdapterResult<T> = std::result::Result<T, AnalyzeAdapterError>;

/// Classify a fetch failure into the category the report states.
///
/// Why: the Gaps & Caveats line names which endpoint failed and why, and "why"
/// must come from the error class rather than from string-matching a message
/// that can quote a path. The daemon's timeout error is the daemon reporting its
/// OWN deadline was hit, so it lands in the same category as a client-side
/// timeout — both mean the analysis ran out of time, not that nothing was
/// listening.
///
/// #6287: the HTTP 408/504 arm became [`CODE_DEADLINE_EXCEEDED`], the code
/// trusty-analyze's `service::events` assigns its `gateway_timeout` error for
/// exactly this reason. Matching a code rather than a message keeps the rule
/// above intact — a message can quote a path and can be reworded; a code cannot.
/// Test: `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`,
/// `a_daemon_side_deadline_is_a_timeout_not_a_rejection`.
fn classify_failure(e: &AnalyzeAdapterError) -> EndpointFailure {
    match e {
        AnalyzeAdapterError::Timeout(_) => EndpointFailure::TimedOut,
        AnalyzeAdapterError::Rpc { code, .. } if *code == CODE_DEADLINE_EXCEEDED => {
            EndpointFailure::TimedOut
        }
        AnalyzeAdapterError::Rpc { .. } | AnalyzeAdapterError::Parse(_) => {
            EndpointFailure::Rejected
        }
        AnalyzeAdapterError::Transport(_) => EndpointFailure::Unanswered,
    }
}

/// The code trusty-analyze reports when a handler exhausted its own deadline.
///
/// Duplicated as a literal rather than imported: this crate has no Cargo edge on
/// trusty-analyze, and adding one to share an `i64` would pull a tree-sitter
/// engine into every `trusty-review report` build.
/// `trusty_analyze::service::events::CODE_DEADLINE_EXCEEDED` is the definition;
/// `a_daemon_side_deadline_is_a_timeout_not_a_rejection` is what keeps them
/// equal on this side.
const CODE_DEADLINE_EXCEEDED: i64 = -32005;

// ─── Wire types (trusty-analyze JSON, minimal shapes) ────────────────────────

/// One entry of `analyze.list_indexes` (`[{"id": ...}, ...]`).
///
/// Named apart from [`IndexInfo`] since #6677: that is trusty-search's registry
/// entry, `root_path` included, and this is the analyze daemon's id-only proxy
/// of it — the readiness probe reads this one, resolution reads that one.
#[derive(Debug, Deserialize)]
struct ServedIndex {
    id: String,
}

/// `GET /indexes/{id}/complexity_distribution` envelope (#5320).
///
/// Why: the exhaustive A–F histogram over the whole corpus. Its predecessor
/// here bucketed a `complexity_hotspots` top-N, which is sorted descending and
/// truncated — on a large repository the top 1000 are all grade D and F, so the
/// report stated that a 1.37M-line codebase contains no simple functions.
/// What: `total` is the counted code-chunk population (the percentage
/// denominator the renderer needs) and `buckets` carries every band.
/// Test: `analyze_adapter_tests.rs::distribution_maps_every_band`.
#[derive(Debug, Deserialize)]
struct DistributionEnvelope {
    #[serde(default)]
    total: u64,
    #[serde(default)]
    buckets: Vec<WireBucket>,
}

/// One A–F band of the full distribution.
#[derive(Debug, Deserialize)]
struct WireBucket {
    #[serde(default)]
    label: String,
    #[serde(default)]
    count: u64,
}

/// `GET /indexes/{id}/diagnostics` envelope.
#[derive(Debug, Deserialize)]
struct DiagnosticsEnvelope {
    #[serde(default)]
    diagnostics: Vec<WireDiagnostic>,
    /// Which external linters actually ran. Empty means none was installed —
    /// which is why the RED band can be empty without the code being clean
    /// (#5317).
    #[serde(default)]
    tools_run: Vec<String>,
}

/// One external-tool diagnostic (`ToolDiagnostic`).
#[derive(Debug, Deserialize)]
pub(super) struct WireDiagnostic {
    #[serde(default)]
    pub(super) tool: String,
    #[serde(default)]
    pub(super) file: String,
    /// `error` | `warning` | `info` | `hint` (lowercase).
    #[serde(default)]
    pub(super) severity: String,
    #[serde(default)]
    pub(super) code: Option<String>,
    /// The linter's own message. Verbatim tool output, not synthesis.
    #[serde(default)]
    pub(super) message: String,
}

/// `GET /indexes/{id}/refactor-suggestions` envelope.
#[derive(Debug, Deserialize)]
struct RefactorEnvelope {
    #[serde(default)]
    suggestions: Vec<WireRefactor>,
}

/// One refactoring suggestion (`RefactorSuggestion`).
#[derive(Debug, Deserialize)]
pub(super) struct WireRefactor {
    #[serde(default)]
    pub(super) file: String,
    #[serde(default)]
    pub(super) function_name: Option<String>,
    /// What kind of region the daemon measured, when it could tell (#6177).
    ///
    /// `class_body` or `function` for Python; absent for every other language and
    /// for any daemon predating the field, which is what keeps this a pure
    /// addition — a missing key reproduces the pre-#6177 render exactly.
    #[serde(default)]
    pub(super) region_kind: Option<String>,
    /// snake_case refactor type, e.g. `extract_method`.
    #[serde(default)]
    pub(super) refactor_type: String,
    /// `low` | `medium` | `high` | `critical` (lowercase).
    #[serde(default)]
    pub(super) severity: String,
    /// Why the rule fired, e.g. `cyclomatic complexity 31 (grade F)`.
    #[serde(default)]
    pub(super) rationale: String,
    /// The concrete action, e.g. `Extract the body of 'f' into 2-3 smaller
    /// functions`.
    #[serde(default)]
    pub(super) suggested_action: String,
}

// ─── Severity mapping (BINDING convention, epic #2445) ───────────────────────

/// Map a trusty-analyze diagnostic severity onto the report's RED/AMBER/GREEN
/// band.
///
/// Why: the report groups findings into three bands; the owner fixed a single
/// balanced mapping across BOTH severity vocabularies (diagnostics and
/// refactors) so a reader sees a consistent risk posture.
/// What (convention): `error → Red`, `warning → Amber`, everything else
/// (`info`, `hint`, unknown) → `Green`.
/// Test: `severity_map_diagnostics` in the tests module.
pub(super) fn map_diagnostic_severity(s: &str) -> Severity {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" | "critical" => Severity::Red,
        "warning" | "high" => Severity::Amber,
        _ => Severity::Green,
    }
}

/// Map a trusty-analyze refactor severity onto the report's RED/AMBER/GREEN
/// band.
///
/// Why (#5317): a refactor suggestion is not a defect. Its severity is derived
/// from the chunk's complexity grade alone (`Severity::from_grade` in
/// trusty-analyze), so `critical` there means "grade F", not "a critical risk".
/// Routing that into RED put twenty "Extract method" entries into the most
/// severe band of an acquirer-facing report, which reads code hygiene as
/// business risk. The RED band is reserved for defect-class findings — external
/// static-analysis errors — so a refactor suggestion tops out at AMBER however
/// severe the analyzer graded it.
/// What (convention): `critical`/`high` → `Amber`; `medium`, `low`, and unknown
/// → `Green` (dropped from the rendered bands).
/// Test: `severity_map_refactors`, `refactor_never_reaches_red`.
pub(super) fn map_refactor_severity(s: &str) -> Severity {
    match s.trim().to_ascii_lowercase().as_str() {
        "critical" | "error" | "high" | "warning" => Severity::Amber,
        _ => Severity::Green,
    }
}

// ─── Complexity distribution ─────────────────────────────────────────────────

/// Map the daemon's full A–F histogram onto the report's bucket list.
///
/// Why (#5320): the distribution is fetched whole and rendered whole. The
/// percentage column the renderer computes is a share of the bucket sum, so the
/// sum must be the counted population — which it is, exactly because this is
/// the exhaustive histogram rather than a truncated top-N sample.
/// What: preserves the daemon's ascending band order and its zero-count bands
/// (an empty band is a measurement); returns an empty distribution when the
/// envelope carried no counted chunks, which the renderer omits rather than
/// charting a row of zeroes.
/// Test: `distribution_maps_every_band`, `empty_distribution_maps_to_nothing`.
fn map_distribution(env: &DistributionEnvelope) -> ComplexityDistribution {
    if env.total == 0 {
        return ComplexityDistribution::default();
    }
    ComplexityDistribution {
        buckets: env
            .buckets
            .iter()
            .map(|b| ComplexityBucket {
                label: b.label.clone(),
                count: b.count,
            })
            .collect(),
    }
}

use super::analyze_findings::{diagnostic_finding, refactor_finding, relativize_components};

// ─── Pure mapping ────────────────────────────────────────────────────────────

/// Map the fetched analyze datasets onto a v0 [`AnalyzeMetrics`].
///
/// Why: a single pure function makes the whole adapter unit-testable against
/// fixture JSON with no live daemon — the HTTP layer only feeds it deserialized
/// wire values.
/// What: leaves `loc`/`counts` empty (the built-in scanner owns those); takes
/// `complexity` from the daemon's full histogram; builds `findings` from
/// RED/AMBER diagnostics then refactor suggestions, dropping any that would
/// render as a title and a path with no stated observation or action
/// ([`MetricFinding::is_contentless`], #5317). `schema_version` is tagged so the
/// JSON twin records its provenance.
/// Test: `map_metrics_populates_complexity_and_findings`,
/// `contentless_findings_are_dropped`.
fn map_metrics(
    distribution: Option<&DistributionEnvelope>,
    diagnostics: &[WireDiagnostic],
    refactors: &[WireRefactor],
) -> AnalyzeMetrics {
    let mut findings: Vec<MetricFinding> = Vec::new();
    findings.extend(diagnostics.iter().filter_map(diagnostic_finding));
    findings.extend(refactors.iter().filter_map(refactor_finding));
    findings.retain(|f| !f.is_contentless());

    AnalyzeMetrics {
        schema_version: "analyze-live-v0".to_string(),
        repository: String::new(),
        loc: Default::default(),
        counts: Default::default(),
        complexity: distribution.map(map_distribution).unwrap_or_default(),
        findings,
    }
}

// ─── Fetch seam ──────────────────────────────────────────────────────────────

/// Injectable source of live analyze metrics for one index.
///
/// Why: decouples the report pipeline from the concrete HTTP client so the e2e
/// test can inject an in-process mock (or a stub) instead of standing up a real
/// daemon, and so the fail-open contract lives at one boundary.
/// What: `fetch` returns `Some(metrics)` on success and `None` on ANY failure
/// (unreachable daemon, index not served, non-2xx, parse error) — never `Err`.
/// Test: `HttpAnalyzeMetricsSource` fail-open paths in the tests module.
#[async_trait]
pub trait AnalyzeMetricsSource: Send + Sync {
    /// Fetch and map metrics for `index_id`, fail-open to `None`.
    async fn fetch(&self, index_id: &str) -> Option<AnalyzeMetrics>;

    /// The same fetch, with the reason named when it yields no metrics (#5239).
    ///
    /// Why: [`Self::fetch`]'s `None` is correct as a control-flow signal and
    /// useless as a report fact — a reader cannot tell an unassessed dimension
    /// from a clean one. This variant preserves the fail-open contract (it
    /// still never returns `Err`) while carrying the reason far enough to be
    /// rendered under Gaps & Caveats.
    /// What: the default implementation delegates to [`Self::fetch`] and, since
    /// it cannot see why, reports [`AnalyzeGap::Unavailable`]. Implementations
    /// that know more override it.
    /// Test: `analyze_adapter_tests.rs::default_fetch_named_reports_unavailable`.
    async fn fetch_named(&self, index_id: &str) -> AnalyzeFetch {
        match self.fetch(index_id).await {
            Some(m) => AnalyzeFetch::Fetched {
                metrics: Box::new(m),
                caveats: Vec::new(),
            },
            None => AnalyzeFetch::Missing(AnalyzeGap::Unavailable),
        }
    }

    /// The trusty-search indexes registered on this machine (#6677).
    ///
    /// Why: the id a checkout DERIVES to and the id it is REGISTERED under can
    /// differ, and only the registry says so — `root_path` is what tells them
    /// apart. The list is read once per enrichment and handed to
    /// [`super::index_registry::resolve_report_index`].
    /// What: the default is an empty list — a source with no daemon behind it
    /// substitutes nothing, so every stub keeps resolving to the derived id.
    /// Test: `analyze_adapter_tests.rs::a_repo_served_under_another_id_is_fetched_by_that_id`.
    async fn registered_indexes(&self) -> Vec<IndexInfo> {
        Vec::new()
    }
}

/// Why one repository has no live analyze metrics (#5239, DOC-67 §9).
///
/// Why: "the daemon was down" and "this repo was never indexed" are different
/// facts to an acquirer reading the gap list — the first says nothing about the
/// codebase, the second says the operator skipped a setup step — so they are
/// distinct variants rather than one opaque string. The variant is also what
/// keeps a raw transport error, which can quote a URL or a response body, out
/// of the generated artifact: the detail stays on stderr, the category is what
/// reaches the report.
/// What: the two reasons [`HttpAnalyzeMetricsSource`] can distinguish, plus the
/// catch-all a source that cannot tell them apart reports.
/// Test: `analyze_adapter_tests.rs::gap_labels_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AnalyzeGap {
    /// The daemon answered, but serves no index for this repository.
    NotIndexed,
    /// The daemon could not be reached, or its answer could not be used.
    Unreachable,
    /// No metrics, and the source could not say why.
    Unavailable,
}

impl AnalyzeGap {
    /// The report-facing phrase for this gap, e.g. `"trusty-analyze unreachable"`.
    ///
    /// Why: the Gaps & Caveats line is read by a stranger to this toolchain, so
    /// it names the condition in plain words rather than a variant name.
    /// What: a fixed string per variant — deterministic, and free of any
    /// run-specific detail.
    /// Test: `analyze_adapter_tests.rs::gap_labels_are_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotIndexed => "trusty-analyze index not built",
            Self::Unreachable => "trusty-analyze unreachable",
            Self::Unavailable => "trusty-analyze data unavailable",
        }
    }
}

/// The outcome of one named fetch.
///
/// Why/What: see [`AnalyzeMetricsSource::fetch_named`]. `Fetched` is boxed
/// because [`AnalyzeMetrics`] dwarfs the gap variant.
/// Test: `analyze_adapter_tests.rs::default_fetch_named_reports_unavailable`.
#[derive(Debug)]
#[non_exhaustive]
pub enum AnalyzeFetch {
    /// Live metrics were fetched and mapped, along with any dimension the
    /// daemon could not answer completely (#5317, #5320).
    Fetched {
        /// The mapped metrics.
        metrics: Box<AnalyzeMetrics>,
        /// Dimensions answered incompletely; empty on a full answer.
        caveats: Vec<AnalyzeCaveat>,
    },
    /// No metrics; this is the reason, for the report's gap list.
    Missing(AnalyzeGap),
}

/// Live HTTP implementation of [`AnalyzeMetricsSource`] over the analyze daemon.
///
/// Why: the real `--analyze` path dials trusty-analyze's Unix socket (#6287,
/// ADR-0032); both processes are on the same machine by construction.
/// What: holds the socket path; `fetch` probes readiness then pulls the three
/// datasets and maps them.
/// Test: `http_source_maps_from_mock` (in the crate's e2e) drives a real
/// in-process stub daemon; unit tests here cover the fail-open conversions.
pub struct HttpAnalyzeMetricsSource {
    socket: PathBuf,
    /// The on-demand starter, shared across every fetch this source makes.
    ///
    /// One handle, not one per call: its spawn gate is what keeps two
    /// concurrent fetches from each starting a server (#6350).
    launcher: trusty_common::uds::OnDemandAnalyze,
    /// Memoised result of the single start attempt.
    ///
    /// Why memoised: `enrich_with_analyze_gaps` iterates repositories, so an
    /// un-memoised start would re-probe the socket once per repo. Why the
    /// FAILURE is memoised too: a machine with no trusty-analyze installed
    /// would otherwise pay a spawn budget per repository, and report the same
    /// failure N times.
    started: tokio::sync::OnceCell<Result<(), String>>,
}

impl HttpAnalyzeMetricsSource {
    /// Construct a source pointed at the daemon's socket.
    ///
    /// Why: the CLI resolves the path from a manifest key / config / the derived
    /// default and hands it here.
    ///
    /// #6287: infallible now, where the HTTP version could fail building a
    /// reqwest client. `AdapterResult` is kept so every call site is unchanged;
    /// there is simply no `Err` arm left to take. The per-request budget moved
    /// with it — `send_framed_request_capped` takes the timeout per call, so
    /// there is no client-level default to set and no way for a call site to
    /// forget one.
    ///
    /// # Errors
    ///
    /// Never, since #6287. The signature is retained for call-site stability.
    ///
    /// Test: `new_accepts_a_socket_path`.
    pub fn new(socket: impl Into<PathBuf>) -> AdapterResult<Self> {
        let socket = socket.into();
        Ok(Self {
            launcher: trusty_common::uds::OnDemandAnalyze::at(&socket),
            socket,
            started: tokio::sync::OnceCell::new(),
        })
    }

    /// Start trusty-analyze if nothing is serving, exactly once per source.
    ///
    /// Why this is here and not at the CLI call site: the CLI hands this type a
    /// socket path and asks for metrics; "is anything serving that path" is this
    /// type's problem, and putting it here means an embedded caller
    /// (`trusty-analyze --features review`, the MCP tools) gets the same
    /// behaviour without duplicating the start.
    ///
    /// What: delegates to the shared [`trusty_common::uds::OnDemandAnalyze`],
    /// memoising the outcome. A failure is converted to a string rather than
    /// kept as an error, because `OnceCell` stores one value for every caller
    /// and `SupervisorError` is not `Clone`.
    ///
    /// # Errors
    ///
    /// [`AnalyzeAdapterError::Transport`] when the server could not be started.
    /// It is NOT degraded to a silent `None` here: the caller converts it into
    /// the visible "falling back to scan" line and a Gaps & Caveats entry.
    ///
    /// Test: `a_failed_start_is_reported_rather_than_swallowed`.
    async fn ensure_started(&self) -> AdapterResult<()> {
        let outcome = self
            .started
            .get_or_init(|| async {
                self.launcher
                    .ensure_running()
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}"))
            })
            .await;
        match outcome {
            Ok(()) => Ok(()),
            Err(reason) => Err(AnalyzeAdapterError::Transport(format!(
                "could not start trusty-analyze on demand for {}: {reason}",
                self.socket.display()
            ))),
        }
    }

    /// Call one endpoint and deserialize its `result` into `T`, mapping every
    /// failure mode to a typed [`AnalyzeAdapterError`].
    ///
    /// #6287 removed the retry #6038 added. That retry existed for one cause: a
    /// pooled HTTP/1.1 keep-alive connection the daemon closed after a request
    /// was already committed to it, which RFC 9112 §9.3 puts recovery for on the
    /// client. `send_framed_request_capped` dials per call, so there is no
    /// pooled connection to lose and nothing left for a retry to recover — a
    /// dial that fails now means nothing is listening, which retrying cannot
    /// change.
    ///
    /// What: one framed exchange under `budget` — the endpoint's own timeout,
    /// because the daemon answers a diagnostics call in minutes and a histogram
    /// call in seconds and one budget cannot be right for both — then the
    /// JSON-RPC envelope check.
    /// Test: `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`,
    /// `fetch_returns_none_on_unreachable_daemon`.
    /// One request, with a respawn-and-retry when the server has gone away.
    ///
    /// Why (#6350): an on-demand server exits after its idle window, and a
    /// multi-repository report is exactly the caller that can straddle one — the
    /// diagnostics endpoint alone has a multi-minute budget, so a run over
    /// several repositories can leave the socket untouched for longer than the
    /// ten-minute default. `ensure_started` runs once per source, so without
    /// this every fetch after the exit would hard-fail with nothing left to
    /// restart it. #6287 removed the old retry on the assumption that the far
    /// end was resident; #6350 invalidated that assumption.
    ///
    /// What: on a `Transport` failure — a socket that refused, vanished, or hung
    /// up — start the server again and reissue the request EXACTLY once. The
    /// second failure is returned as-is.
    ///
    /// A `Timeout` deliberately does not retry: the server answered the connect
    /// and is working, so reissuing would double an already-long budget and hide
    /// a slow handler behind a doubled wait. Nor does an `Rpc` or `Parse`
    /// failure, which are answers, not absences.
    ///
    /// # Errors
    ///
    /// The second attempt's error, or the restart's if that is what failed.
    ///
    /// Test: `tests/on_demand.rs`' `the_adapter_respawns_a_server_that_idled_out`.
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: AnalyzeEndpoint,
        index_id: &str,
        budget: Duration,
    ) -> AdapterResult<T> {
        match self.call_once(endpoint, index_id, budget).await {
            Err(AnalyzeAdapterError::Transport(first)) => {
                tracing::debug!(
                    index_id,
                    endpoint = endpoint.as_str(),
                    error = %first,
                    "--analyze: the socket did not answer; restarting the server and retrying once"
                );
                self.restart().await.map_err(|e| {
                    AnalyzeAdapterError::Transport(format!("{first}; and the retry failed: {e}"))
                })?;
                self.call_once(endpoint, index_id, budget).await
            }
            other => other,
        }
    }

    /// Start the server, bypassing [`Self::ensure_started`]'s memo.
    ///
    /// Why not reuse the memo: it holds the outcome of the FIRST start, and the
    /// case this exists for is a server that started successfully and has since
    /// exited. Reading the memo would return that stale `Ok` and start nothing.
    ///
    /// # Errors
    ///
    /// [`AnalyzeAdapterError::Transport`] when the server could not be started.
    async fn restart(&self) -> AdapterResult<()> {
        self.launcher
            .ensure_running()
            .await
            .map(|_| ())
            .map_err(|e| {
                AnalyzeAdapterError::Transport(format!(
                    "could not restart trusty-analyze for {}: {e:#}",
                    self.socket.display()
                ))
            })
    }

    async fn call_once<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: AnalyzeEndpoint,
        index_id: &str,
        budget: Duration,
    ) -> AdapterResult<T> {
        let method = endpoint.method();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": endpoint.params(index_id),
        });
        let response: trusty_common::uds::server::RpcResponse =
            trusty_common::uds::send_framed_request_capped(
                &self.socket,
                &request,
                budget,
                MAX_RESPONSE_FRAME_BYTES,
            )
            .await
            .map_err(|e| match e {
                trusty_common::uds::UdsRpcError::Timeout { .. } => {
                    AnalyzeAdapterError::Timeout(format!("{method} exceeded {budget:?}"))
                }
                other => AnalyzeAdapterError::Transport(format!(
                    "{method} over {}: {other}",
                    self.socket.display()
                )),
            })?;
        if let Some(error) = response.error {
            return Err(AnalyzeAdapterError::Rpc {
                code: error.code,
                message: error.message,
            });
        }
        let result = response.result.ok_or_else(|| {
            AnalyzeAdapterError::Parse(format!("{method}: neither a result nor an error"))
        })?;
        serde_json::from_value(result)
            .map_err(|e| AnalyzeAdapterError::Parse(format!("{method}: {e}")))
    }

    /// Confirm `index_id` is served by the daemon (`analyze.list_indexes`).
    ///
    /// Why: distinguishes "repo not indexed" (a fail-open skip with a clear
    /// warning) from a transport error, per the indexing prerequisite (#2448).
    async fn index_served(&self, index_id: &str) -> AdapterResult<bool> {
        let endpoint = AnalyzeEndpoint::IndexList;
        let indexes: Vec<ServedIndex> = self.call(endpoint, index_id, endpoint.budget()).await?;
        Ok(indexes.iter().any(|i| i.id == index_id))
    }

    /// Fetch one dataset endpoint, reporting a failure as a NAMED caveat rather
    /// than aborting the whole repository's fetch.
    ///
    /// Why (#6041): the per-repo fetch used to be all-or-nothing — one endpoint
    /// failing discarded the data every other endpoint had already returned. In
    /// the field the diagnostics call was the slow one, so a complexity
    /// histogram that answered in seconds was thrown away with it and the whole
    /// report fell back to scan. Each endpoint now stands alone: what arrived is
    /// kept, and what did not names itself.
    /// What: `Some(T)` on success; on failure logs the detail to stderr, pushes
    /// an [`AnalyzeCaveat::EndpointUnavailable`] naming the endpoint and the
    /// failure category, and returns `None`. The error text never reaches the
    /// caveat — it can quote a URL or a response body, and the artifact gets the
    /// category only.
    /// Test: `a_failing_endpoint_keeps_what_the_others_returned`,
    /// `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`.
    async fn fetch_dataset<T: serde::de::DeserializeOwned>(
        &self,
        index_id: &str,
        endpoint: AnalyzeEndpoint,
        caveats: &mut Vec<AnalyzeCaveat>,
    ) -> Option<T> {
        match self.call(endpoint, index_id, endpoint.budget()).await {
            Ok(v) => Some(v),
            Err(e) => {
                let reason = classify_failure(&e);
                tracing::warn!(index_id, endpoint = endpoint.as_str(), error = %e,
                    "--analyze: endpoint unavailable; the rest of the fetch continues");
                eprintln!(
                    "[trusty-review report] --analyze: '{index_id}' {} unavailable ({e}); \
                     the sections it feeds are marked unassessed",
                    endpoint.as_str()
                );
                caveats.push(AnalyzeCaveat::EndpointUnavailable(endpoint, reason));
                None
            }
        }
    }

    /// The success path behind [`AnalyzeMetricsSource::fetch`]: probe readiness,
    /// pull the datasets, and map them. Returns `Err` on any failure; the
    /// public `fetch` swallows it to `None`.
    ///
    /// Every dataset endpoint is fetched independently (#6041): a failure on one
    /// leaves the others' data in place and adds a caveat naming the endpoint
    /// that dropped out. Only the readiness probe is still fatal — without it
    /// the adapter cannot tell an unindexed repository from a broken one — and
    /// so is the case where NO dataset answered, which is a fetch that assessed
    /// nothing and must fall back to scan rather than render an empty pass.
    async fn try_fetch(
        &self,
        index_id: &str,
    ) -> AdapterResult<Option<(AnalyzeMetrics, Vec<AnalyzeCaveat>)>> {
        // #6350: nothing is resident on this socket; start it before dialling.
        self.ensure_started().await?;

        if !self.index_served(index_id).await? {
            tracing::warn!(
                index_id,
                "--analyze: repo not indexed in trusty-analyze/trusty-search; \
                 falling back to scan"
            );
            eprintln!(
                "[trusty-review report] --analyze: '{index_id}' not indexed in \
                 trusty-analyze/trusty-search; falling back to scan"
            );
            return Ok(None);
        }
        let mut caveats: Vec<AnalyzeCaveat> = Vec::new();

        let distribution: Option<DistributionEnvelope> = self
            .fetch_dataset(
                index_id,
                AnalyzeEndpoint::ComplexityDistribution,
                &mut caveats,
            )
            .await;
        let diagnostics: Option<DiagnosticsEnvelope> = self
            .fetch_dataset(index_id, AnalyzeEndpoint::Diagnostics, &mut caveats)
            .await;
        let refactors: Option<RefactorEnvelope> = self
            .fetch_dataset(index_id, AnalyzeEndpoint::RefactorSuggestions, &mut caveats)
            .await;

        if distribution.is_none() && diagnostics.is_none() && refactors.is_none() {
            // Nothing was assessed, so there is nothing partial to render.
            // Reporting metrics here would put an empty findings table and an
            // empty §7 on the page, which reads as a clean pass.
            return Err(AnalyzeAdapterError::Transport(format!(
                "no dataset endpoint answered for '{index_id}'"
            )));
        }
        // An empty `tools_run` is the daemon answering that no linter existed —
        // a different fact from the endpoint not answering at all (#5317).
        if diagnostics.as_ref().is_some_and(|d| d.tools_run.is_empty()) {
            caveats.push(AnalyzeCaveat::NoStaticAnalysisTools);
        }

        let diags = diagnostics.map(|d| d.diagnostics).unwrap_or_default();
        let refs = refactors.map(|r| r.suggestions).unwrap_or_default();
        Ok(Some((
            map_metrics(distribution.as_ref(), &diags, &refs),
            caveats,
        )))
    }
}

#[async_trait]
impl AnalyzeMetricsSource for HttpAnalyzeMetricsSource {
    async fn fetch(&self, index_id: &str) -> Option<AnalyzeMetrics> {
        match self.fetch_named(index_id).await {
            AnalyzeFetch::Fetched { metrics, .. } => Some(*metrics),
            AnalyzeFetch::Missing(_) => None,
        }
    }

    /// #6677: the registry is trusty-search's, not the analyze daemon's —
    /// `analyze.list_indexes` proxies the ids and drops `root_path`, which is
    /// the field resolution needs.
    async fn registered_indexes(&self) -> Vec<IndexInfo> {
        super::index_registry::registered_indexes().await
    }

    /// #5239: the same fail-open fetch, naming which of the two conditions
    /// produced an empty result so the report can say so.
    async fn fetch_named(&self, index_id: &str) -> AnalyzeFetch {
        match self.try_fetch(index_id).await {
            Ok(Some((metrics, caveats))) => AnalyzeFetch::Fetched {
                metrics: Box::new(metrics),
                caveats,
            },
            Ok(None) => AnalyzeFetch::Missing(AnalyzeGap::NotIndexed),
            Err(e) => {
                tracing::warn!(
                    index_id,
                    error = %e,
                    "--analyze: fetch failed; falling back to scan"
                );
                eprintln!(
                    "[trusty-review report] --analyze: fetch for '{index_id}' failed \
                     ({e}); falling back to scan"
                );
                // The error text stays here, on stderr: it can quote a URL or a
                // response body, neither of which belongs in an artifact handed
                // to a third party. The report gets the category only.
                AnalyzeFetch::Missing(AnalyzeGap::Unreachable)
            }
        }
    }
}

// ─── Model enrichment (precedence seam, #2448) ───────────────────────────────

/// Fill live analyze metrics into a built [`ReportModel`](crate::report::ReportModel), honouring the
/// fail-open precedence: declared metrics file > `--analyze` live fetch > None.
///
/// Why: `--analyze` must populate the complexity chart + finding bands for a
/// bare run, but must NEVER override a hand-authored metrics JSON, and must
/// never abort the report — an unindexed repo or an unreachable daemon simply
/// leaves the repo at its declared/scan state.
/// What: for each repository that has NO declared metrics AND is a local
/// checkout (remote repos are never indexed locally), derives the index id from
/// the checkout path and fetches via `source`; a `Some` result populates
/// `repo.metrics`. Repos with declared metrics or no local path are skipped.
/// Test: `report_analyze_e2e.rs` drives this against an in-process HTTP mock.
pub async fn enrich_with_analyze(
    model: &mut super::model::ReportModel,
    source: &dyn AnalyzeMetricsSource,
) {
    let _ = enrich_with_analyze_gaps(model, source).await;
}

/// Same enrichment, returning one Gaps & Caveats line per degraded condition
/// (#5239, DOC-67 §9).
///
/// Why: fail-open is the right contract and fail-SILENT is not. A findings
/// table that renders empty because the daemon was down is indistinguishable,
/// on the page, from a codebase with no findings — so every repository the
/// fetch could not populate is named, grouped by reason, in the report itself.
/// The fetch contract is unchanged: nothing here aborts, and the report still
/// renders from the built-in scan.
/// What: walks the same repositories [`enrich_with_analyze`] does, using
/// [`AnalyzeMetricsSource::fetch_named`]; returns at most one line per
/// [`AnalyzeGap`] kind and one per [`AnalyzeCaveat`] kind, each naming the
/// affected repositories in model order so two runs over the same state produce
/// identical lines. Repositories with a declared metrics file, and remote
/// entries, are skipped — neither is a gap. Returns an empty vec when every
/// eligible repository was populated completely.
/// Test: `analyze_adapter_tests.rs::{enrich_names_unreachable_repositories,
/// enrich_reports_no_gaps_when_every_repo_is_populated,
/// enrich_reports_caveats_for_partially_answered_repositories}`, plus
/// `redact_tests.rs::enrich_scrubs_configured_credentials_from_findings` for
/// the #5323 redaction boundary.
pub async fn enrich_with_analyze_gaps(
    model: &mut super::model::ReportModel,
    source: &dyn AnalyzeMetricsSource,
) -> Vec<String> {
    // BTreeMap, not HashMap: the rendered line order must not depend on hash
    // iteration order (DOC-67 §9's determinism requirement).
    let mut missing: std::collections::BTreeMap<AnalyzeGap, Vec<String>> = Default::default();
    let mut partial: std::collections::BTreeMap<AnalyzeCaveat, Vec<String>> = Default::default();
    // #6137: one line per repository whose index described a different checkout.
    let mut stale: Vec<String> = Vec::new();

    // #5323: daemon-authored text lands in an acquirer-facing artifact, so it
    // crosses the redaction boundary before it reaches the model. Resolved once
    // per enrichment, not once per repository — it touches the filesystem.
    let secrets = super::redact::report_secrets();
    // #6677: one registry read for the whole walk — resolution needs the
    // daemon's `root_path` values, and they do not change mid-enrichment.
    let indexes = source.registered_indexes().await;

    for repo in &mut model.repositories {
        // Precedence: a declared metrics file always wins.
        if repo.metrics.is_some() {
            continue;
        }
        // Only local checkouts can be served by trusty-analyze/trusty-search.
        let Some(path) = repo.local_path.as_ref() else {
            continue;
        };
        // #6677: the derived id when the daemon holds it, otherwise the index
        // registered at this checkout's root_path; `None` only for a path that
        // derives to nothing, which is the skip this always made.
        let Some(index_id) = resolve_report_index(path, &indexes).into_id() else {
            continue;
        };
        match source.fetch_named(&index_id).await {
            AnalyzeFetch::Fetched {
                mut metrics,
                caveats,
            } => {
                super::redact::scrub_metrics(&mut metrics, &secrets);
                // #6082: the daemon reports absolute paths; the report cites
                // repository-relative ones everywhere else.
                relativize_components(&mut metrics, path);
                // #6137: an index addressed by directory basename can serve a
                // DIFFERENT checkout of the same repository. Data describing
                // another tree is stale-index evidence, never a measurement of
                // this one.
                match super::analyze_scope::accept(&repo.name, &index_id, path, *metrics) {
                    Ok(m) => {
                        repo.metrics = Some(m);
                        for caveat in caveats {
                            partial.entry(caveat).or_default().push(repo.name.clone());
                        }
                    }
                    Err(gap) => {
                        // #6080: the investigation pass writes into the same
                        // `metrics` struct, so a section reporting an
                        // analyze-only figure needs this marker to tell a
                        // measurement from an artefact of that sharing.
                        repo.analyze_gap =
                            Some(super::analyze_scope::STALE_INDEX_REMEDY.to_string());
                        stale.push(gap);
                    }
                }
            }
            AnalyzeFetch::Missing(gap) => {
                repo.analyze_gap = Some(super::analyze_scope::NO_ANALYZE_DATA_REMEDY.to_string());
                missing.entry(gap).or_default().push(repo.name.clone());
            }
        }
    }

    let mut lines: Vec<String> = missing
        .into_iter()
        .map(|(gap, repos)| {
            format!(
                "{} — no analysis pass ran for: {}. \
                 Those applications are described from the repository scan alone; \
                 their findings, complexity, and health factors are not assessed, \
                 not clean.",
                gap.as_str(),
                repos.join(", ")
            )
        })
        .collect();
    lines.extend(
        partial
            .into_iter()
            .map(|(caveat, repos)| format!("{caveat} — affects: {}.", repos.join(", "))),
    );
    lines.extend(stale);
    lines
}

#[cfg(test)]
#[path = "analyze_adapter_tests.rs"]
mod tests;
