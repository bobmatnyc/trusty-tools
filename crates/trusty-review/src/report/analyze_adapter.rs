//! Deterministic trusty-analyze → `AnalyzeMetrics` adapter (#2447, epic #2445).
//!
//! Why: a bare `report --analyze` must populate the metrics-driven sections
//! (the §7 complexity-distribution chart + RED/AMBER finding bands) from
//! trusty-analyze WITHOUT an LLM and WITHOUT a hand-authored metrics JSON. A
//! library dependency on trusty-analyze is impossible (a cargo cycle: analyze
//! already optionally depends on trusty-review via its `review` feature), so
//! this adapter is a thin HTTP client over the analyze daemon (`:7879`) plus a
//! pure mapping from the daemon's wire JSON onto the report's v0
//! [`AnalyzeMetrics`]. Every probe/fetch/parse failure is fail-open — the whole
//! adapter degrades to `None` and the report falls through to the built-in
//! scan; a missing analyze index is never an error.
//!
//! What: [`AnalyzeMetricsSource`] is the injectable fetch seam;
//! [`HttpAnalyzeMetricsSource`] is the live implementation. The pure mapping
//! ([`map_metrics`], `complexity_buckets`, [`diagnostic_finding`],
//! [`refactor_finding`]) is unit-tested against fixture JSON with no live
//! daemon. `loc`/`counts` are deliberately left empty — the built-in scanner
//! owns those measured numbers.
//!
//! Test: `analyze_adapter_tests.rs` covers envelope parsing, the severity map,
//! the complexity-bucket thresholds, and fail-open on malformed JSON.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, MetricFinding, Severity,
};

// ─── Tunables ──────────────────────────────────────────────────────────────

/// Per-request HTTP timeout for analyze fetches (fail-open on timeout).
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// HTTP transport failure (connection refused, timeout, DNS, ...).
    #[error("trusty-analyze transport error: {0}")]
    Transport(String),

    /// trusty-analyze returned a non-2xx status.
    #[error("trusty-analyze API returned {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },

    /// Response JSON could not be parsed against the expected envelope.
    #[error("trusty-analyze response parse error: {0}")]
    Parse(String),

    /// reqwest client construction failed (TLS backend unavailable).
    #[error("failed to build HTTP client: {0}")]
    ClientInit(String),
}

type AdapterResult<T> = std::result::Result<T, AnalyzeAdapterError>;

// ─── Wire types (trusty-analyze JSON, minimal shapes) ────────────────────────

/// One entry of `GET /indexes` (`[{"id": ..., "root_path": ...}]`).
#[derive(Debug, Deserialize)]
struct IndexInfo {
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
struct WireDiagnostic {
    #[serde(default)]
    tool: String,
    #[serde(default)]
    file: String,
    /// `error` | `warning` | `info` | `hint` (lowercase).
    #[serde(default)]
    severity: String,
    #[serde(default)]
    code: Option<String>,
    /// The linter's own message. Verbatim tool output, not synthesis.
    #[serde(default)]
    message: String,
}

/// `GET /indexes/{id}/refactor-suggestions` envelope.
#[derive(Debug, Deserialize)]
struct RefactorEnvelope {
    #[serde(default)]
    suggestions: Vec<WireRefactor>,
}

/// One refactoring suggestion (`RefactorSuggestion`).
#[derive(Debug, Deserialize)]
struct WireRefactor {
    #[serde(default)]
    file: String,
    #[serde(default)]
    function_name: Option<String>,
    /// snake_case refactor type, e.g. `extract_method`.
    #[serde(default)]
    refactor_type: String,
    /// `low` | `medium` | `high` | `critical` (lowercase).
    #[serde(default)]
    severity: String,
    /// Why the rule fired, e.g. `cyclomatic complexity 31 (grade F)`.
    #[serde(default)]
    rationale: String,
    /// The concrete action, e.g. `Extract the body of 'f' into 2-3 smaller
    /// functions`.
    #[serde(default)]
    suggested_action: String,
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
fn map_diagnostic_severity(s: &str) -> Severity {
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
fn map_refactor_severity(s: &str) -> Severity {
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

// ─── Finding synthesis (prose-free, deterministic) ───────────────────────────

/// Build a [`MetricFinding`] from a tool diagnostic, or `None` when it maps to
/// the GREEN band (never rendered — omitted to keep the findings list
/// actionable, per the report's no-green rule).
///
/// Why: RED/AMBER findings must be listed deterministically with no LLM prose;
/// `title`/`category`/`component` are verbatim facts.
/// What: `title` = the rule code when present, else a synthesised
/// `"{tool} diagnostic"`; `category` = the producing tool (a stable provenance
/// category, not the prose message); `component` = the file; `severity` via the
/// diagnostic map; `description` = the linter's own message, verbatim (#5317 —
/// dropping it left every rendered prose slot reading the honesty marker).
/// Test: `diagnostic_finding_synthesises_title` and `..._drops_green`.
fn diagnostic_finding(d: &WireDiagnostic) -> Option<MetricFinding> {
    let severity = map_diagnostic_severity(&d.severity);
    if severity == Severity::Green {
        return None;
    }
    let title = d.code.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| {
        if d.tool.is_empty() {
            "diagnostic".to_string()
        } else {
            format!("{} diagnostic", d.tool)
        }
    });
    Some(MetricFinding {
        title,
        severity,
        category: if d.tool.is_empty() {
            "diagnostic".to_string()
        } else {
            d.tool.clone()
        },
        component: d.file.clone(),
        description: d.message.clone(),
        remediation: String::new(),
    })
}

/// Build a [`MetricFinding`] from a refactor suggestion, or `None` when it maps
/// to GREEN (omitted, as above).
///
/// Why: high/critical refactor suggestions are actionable maintainability
/// findings that belong in the AMBER band even with no synthesis — never RED,
/// see [`map_refactor_severity`].
/// What: `title` = the humanised refactor type plus the function name when
/// known (e.g. `"Extract method — parse_config"`); `category` =
/// `"maintainability"` (refactors are maintainability by construction);
/// `component` = the file; `severity` via the refactor map; `description` =
/// the analyzer's `rationale` and `remediation` = its `suggested_action`, both
/// verbatim (#5317 — dropping them is what made the entries contentless).
/// Test: `refactor_finding_synthesises_title`,
/// `refactor_finding_carries_rationale_and_action`.
fn refactor_finding(r: &WireRefactor) -> Option<MetricFinding> {
    let severity = map_refactor_severity(&r.severity);
    if severity == Severity::Green {
        return None;
    }
    let action = humanise_refactor_type(&r.refactor_type);
    let title = match &r.function_name {
        Some(f) if !f.is_empty() => format!("{action} — {f}"),
        _ => action,
    };
    Some(MetricFinding {
        title,
        severity,
        category: "maintainability".to_string(),
        component: r.file.clone(),
        description: r.rationale.clone(),
        remediation: r.suggested_action.clone(),
    })
}

/// Convert a snake_case refactor type into a readable title fragment.
///
/// Why: keeps synthesised titles legible without pulling in trusty-analyze's
/// enum. What: `extract_method` → `"Extract method"`; unknown/empty →
/// `"Refactor"`. Test: `refactor_finding_synthesises_title`.
fn humanise_refactor_type(t: &str) -> String {
    if t.is_empty() {
        return "Refactor".to_string();
    }
    let mut words = t.split('_').filter(|w| !w.is_empty());
    let mut out = String::new();
    if let Some(first) = words.next() {
        let mut chars = first.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    for w in words {
        out.push(' ');
        out.push_str(w);
    }
    out
}

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
}

/// A dimension the daemon answered incompletely, for one repository (#5317,
/// #5320).
///
/// Why: a fetch that succeeds is not the same as a fetch that answered
/// everything, and the difference is invisible on the page. An empty RED band
/// because no linter was installed reads exactly like a clean codebase; a
/// missing complexity table reads like a rendering slip. Both are facts the
/// report owes its reader, and both travel the same Gaps & Caveats path
/// [`AnalyzeGap`] already uses.
/// What: one variant per condition the fetch can distinguish, each with a fixed
/// report-facing phrase carrying no run-specific detail.
/// Test: `analyze_adapter_tests.rs::caveat_labels_are_stable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AnalyzeCaveat {
    /// The daemon serves no full-corpus histogram, so the §7 complexity
    /// distribution was left out rather than filled from a truncated top-N.
    ComplexityDistributionUnavailable,
    /// No external static-analysis tool was installed, so nothing could
    /// populate the RED/defect band.
    NoStaticAnalysisTools,
}

impl AnalyzeCaveat {
    /// The report-facing sentence for this caveat.
    ///
    /// Why: read by a stranger to this toolchain, so it names the condition and
    /// what it means for the section, not the variant.
    /// What: a fixed string per variant — deterministic across runs.
    /// Test: `analyze_adapter_tests.rs::caveat_labels_are_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ComplexityDistributionUnavailable => {
                "complexity distribution not available — the analysis daemon serves no \
                 full-corpus histogram, and the truncated top-N hotspot sample it does serve \
                 is not a distribution. The §7 complexity table is omitted rather than \
                 filled with shares of a truncation"
            }
            Self::NoStaticAnalysisTools => {
                "no external static-analysis tool was available to the analysis daemon — the \
                 RED/CRITICAL band is populated only from such tools, so an empty band here \
                 means unassessed, not clean"
            }
        }
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
/// Why: the real `--analyze` path talks to `:7879` over plain HTTP/1.1 (both
/// processes are on loopback).
/// What: holds the base URL and a reqwest client; `fetch` probes readiness then
/// pulls the three datasets and maps them.
/// Test: `http_source_maps_from_mock` (in the crate's e2e) drives a real
/// in-process HTTP mock; unit tests here cover the fail-open conversions.
pub struct HttpAnalyzeMetricsSource {
    base_url: String,
    http: reqwest::Client,
}

impl HttpAnalyzeMetricsSource {
    /// Construct a source pointed at `base_url` (e.g. `http://127.0.0.1:7879`).
    ///
    /// Why: the CLI resolves the URL from a manifest key / config / default and
    /// hands it here.
    /// What: trims a trailing slash and builds a timeout-bounded reqwest client.
    /// Test: `new_trims_trailing_slash`.
    pub fn new(base_url: impl Into<String>) -> AdapterResult<Self> {
        let mut base = base_url.into();
        if base.ends_with('/') {
            base.pop();
        }
        let http = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| AnalyzeAdapterError::ClientInit(e.to_string()))?;
        Ok(Self {
            base_url: base,
            http,
        })
    }

    /// GET `path` and deserialize the JSON body into `T`, mapping every failure
    /// mode to a typed [`AnalyzeAdapterError`].
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> AdapterResult<T> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnalyzeAdapterError::Transport(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AnalyzeAdapterError::Transport(format!("read body of {url}: {e}")))?;
        if !status.is_success() {
            return Err(AnalyzeAdapterError::Api {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body).map_err(|e| AnalyzeAdapterError::Parse(format!("{path}: {e}")))
    }

    /// Confirm `index_id` is served by the daemon (`GET /indexes`).
    ///
    /// Why: distinguishes "repo not indexed" (a fail-open skip with a clear
    /// warning) from a transport error, per the indexing prerequisite (#2448).
    async fn index_served(&self, index_id: &str) -> AdapterResult<bool> {
        let indexes: Vec<IndexInfo> = self.get_json("/indexes").await?;
        Ok(indexes.iter().any(|i| i.id == index_id))
    }

    /// The success path behind [`AnalyzeMetricsSource::fetch`]: probe readiness,
    /// pull the datasets, and map them. Returns `Err` on any failure; the
    /// public `fetch` swallows it to `None`.
    ///
    /// The complexity distribution is the one dataset whose absence is
    /// tolerated rather than fatal (#5320): a daemon predating
    /// `/complexity_distribution` answers 404, and the honest response to that
    /// is to omit the §7 table and say so, not to abandon the findings that DID
    /// come back.
    async fn try_fetch(
        &self,
        index_id: &str,
    ) -> AdapterResult<Option<(AnalyzeMetrics, Vec<AnalyzeCaveat>)>> {
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

        let distribution: Option<DistributionEnvelope> = match self
            .get_json(&format!("/indexes/{index_id}/complexity_distribution"))
            .await
        {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(index_id, error = %e, "--analyze: no complexity distribution");
                eprintln!(
                    "[trusty-review report] --analyze: '{index_id}' complexity distribution \
                     unavailable ({e}); the §7 table is omitted"
                );
                caveats.push(AnalyzeCaveat::ComplexityDistributionUnavailable);
                None
            }
        };

        let diagnostics: DiagnosticsEnvelope = self
            .get_json(&format!("/indexes/{index_id}/diagnostics"))
            .await?;
        if diagnostics.tools_run.is_empty() {
            caveats.push(AnalyzeCaveat::NoStaticAnalysisTools);
        }

        let refactors: RefactorEnvelope = self
            .get_json(&format!("/indexes/{index_id}/refactor-suggestions"))
            .await?;

        Ok(Some((
            map_metrics(
                distribution.as_ref(),
                &diagnostics.diagnostics,
                &refactors.suggestions,
            ),
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

// ─── Index-id resolution ─────────────────────────────────────────────────────

/// Derive the trusty-search/analyze index id for a local checkout path.
///
/// Why: trusty-search registers an index under the repo directory's basename
/// (`trusty-search index .` and the git hooks both derive the id from the
/// directory name), so the report can address the same index without extra
/// configuration.
/// What: returns the final path component as a `String`, or `None` for a
/// path with no basename (e.g. `/`).
/// Test: `derive_index_id_uses_basename`.
pub fn derive_index_id(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
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

    // #5323: daemon-authored text lands in an acquirer-facing artifact, so it
    // crosses the redaction boundary before it reaches the model. Resolved once
    // per enrichment, not once per repository — it touches the filesystem.
    let secrets = super::redact::report_secrets();

    for repo in &mut model.repositories {
        // Precedence: a declared metrics file always wins.
        if repo.metrics.is_some() {
            continue;
        }
        // Only local checkouts can be served by trusty-analyze/trusty-search.
        let Some(path) = repo.local_path.as_ref() else {
            continue;
        };
        let Some(index_id) = derive_index_id(path) else {
            continue;
        };
        match source.fetch_named(&index_id).await {
            AnalyzeFetch::Fetched {
                mut metrics,
                caveats,
            } => {
                eprintln!(
                    "[trusty-review report] --analyze: populated metrics for '{}' from index '{index_id}'",
                    repo.name
                );
                super::redact::scrub_metrics(&mut metrics, &secrets);
                repo.metrics = Some(*metrics);
                for caveat in caveats {
                    partial.entry(caveat).or_default().push(repo.name.clone());
                }
            }
            AnalyzeFetch::Missing(gap) => {
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
            .map(|(caveat, repos)| format!("{} — affects: {}.", caveat.as_str(), repos.join(", "))),
    );
    lines
}

#[cfg(test)]
#[path = "analyze_adapter_tests.rs"]
mod tests;
