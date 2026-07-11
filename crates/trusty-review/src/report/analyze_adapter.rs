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
//! ([`map_metrics`], [`complexity_buckets`], [`diagnostic_finding`],
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

/// How many complexity hotspots to request when building the distribution.
///
/// Why: `/complexity_hotspots` returns the top-N chunks ranked by descending
/// cyclomatic complexity; a large N gives a representative distribution. The
/// buckets therefore describe the N MOST complex functions, not the entire
/// corpus (a documented, honest sampling limit — the endpoint exposes no
/// full-corpus histogram).
/// What: passed as `?top_n=` to the hotspots endpoint.
/// Test: the value is not asserted; mapping tests drive fixtures directly.
const HOTSPOT_SAMPLE: usize = 1000;

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

/// `GET /indexes/{id}/complexity_hotspots` envelope (post-#2446).
#[derive(Debug, Deserialize)]
struct HotspotsEnvelope {
    #[serde(default)]
    hotspots: Vec<WireHotspot>,
}

/// One hotspot — the flattened `CodeChunk` plus the #2446 complexity numbers.
/// Only the fields the mapping needs are declared; the rest are ignored.
#[derive(Debug, Deserialize)]
struct WireHotspot {
    #[serde(default)]
    cyclomatic: u32,
}

/// `GET /indexes/{id}/diagnostics` envelope.
#[derive(Debug, Deserialize)]
struct DiagnosticsEnvelope {
    #[serde(default)]
    diagnostics: Vec<WireDiagnostic>,
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
/// Why: refactor suggestions use a `low/medium/high/critical` scale; the same
/// balanced convention applies.
/// What (convention): `critical → Red`, `high → Amber`, `medium`/`low`/unknown
/// → `Green`.
/// Test: `severity_map_refactors` in the tests module.
fn map_refactor_severity(s: &str) -> Severity {
    match s.trim().to_ascii_lowercase().as_str() {
        "critical" | "error" => Severity::Red,
        "high" | "warning" => Severity::Amber,
        _ => Severity::Green,
    }
}

// ─── Complexity bucketing (mirrors trusty-analyze ComplexityGrade) ───────────

/// Compute the cyclomatic-complexity distribution from per-hotspot cyclomatic
/// counts, using the same bands as trusty-analyze's `ComplexityGrade`.
///
/// Why: the §7 chart needs labelled buckets; trusty-analyze exposes no
/// full-corpus histogram, so the adapter buckets the hotspot sample client-side
/// against the canonical grade thresholds so the labels line up with the
/// analyzer's own A–F grading.
/// What (threshold table): `A: 0–5`, `B: 6–10`, `C: 11–15`, `D: 16–20`,
/// `F: >20` — one bucket per band, in ascending order, empty buckets omitted so
/// a sparse sample yields a compact chart.
/// Test: `buckets_follow_grade_thresholds` asserts each boundary lands in the
/// right band.
fn complexity_buckets(cyclomatics: &[u32]) -> ComplexityDistribution {
    // Ordered bands: (label, inclusive-lower, inclusive-upper).  u32::MAX is the
    // open upper bound for the F band.
    let bands: [(&str, u32, u32); 5] = [
        ("A: simple (0-5)", 0, 5),
        ("B: moderate (6-10)", 6, 10),
        ("C: elevated (11-15)", 11, 15),
        ("D: high (16-20)", 16, 20),
        ("F: very high (>20)", 21, u32::MAX),
    ];
    let buckets = bands
        .iter()
        .filter_map(|(label, lo, hi)| {
            let count = cyclomatics
                .iter()
                .filter(|c| **c >= *lo && **c <= *hi)
                .count() as u64;
            (count > 0).then(|| ComplexityBucket {
                label: (*label).to_string(),
                count,
            })
        })
        .collect();
    ComplexityDistribution { buckets }
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
/// diagnostic map. The human message is intentionally dropped (prose belongs to
/// M2 synthesis).
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
    })
}

/// Build a [`MetricFinding`] from a refactor suggestion, or `None` when it maps
/// to GREEN (omitted, as above).
///
/// Why: high/critical refactor suggestions are actionable maintainability
/// findings that belong in the RED/AMBER bands even with no synthesis.
/// What: `title` = the humanised refactor type plus the function name when
/// known (e.g. `"Extract method — parse_config"`); `category` =
/// `"maintainability"` (refactors are maintainability by construction);
/// `component` = the file; `severity` via the refactor map. Prose (`rationale`,
/// `suggested_action`) is dropped.
/// Test: `refactor_finding_synthesises_title`.
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

/// Map the three fetched analyze datasets onto a v0 [`AnalyzeMetrics`].
///
/// Why: a single pure function makes the whole adapter unit-testable against
/// fixture JSON with no live daemon — the HTTP layer only feeds it deserialized
/// wire values.
/// What: leaves `loc`/`counts` empty (the built-in scanner owns those);
/// computes `complexity.buckets` from the hotspot cyclomatic sample; builds
/// `findings` from RED/AMBER diagnostics then refactor suggestions.
/// `schema_version` is tagged so the JSON twin records its provenance.
/// Test: `map_metrics_populates_complexity_and_findings`.
fn map_metrics(
    hotspots: &[WireHotspot],
    diagnostics: &[WireDiagnostic],
    refactors: &[WireRefactor],
) -> AnalyzeMetrics {
    let cyclomatics: Vec<u32> = hotspots.iter().map(|h| h.cyclomatic).collect();
    let mut findings: Vec<MetricFinding> = Vec::new();
    findings.extend(diagnostics.iter().filter_map(diagnostic_finding));
    findings.extend(refactors.iter().filter_map(refactor_finding));

    AnalyzeMetrics {
        schema_version: "analyze-live-v0".to_string(),
        repository: String::new(),
        loc: Default::default(),
        counts: Default::default(),
        complexity: complexity_buckets(&cyclomatics),
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
    /// pull the three datasets, and map them. Returns `Err` on any failure; the
    /// public `fetch` swallows it to `None`.
    async fn try_fetch(&self, index_id: &str) -> AdapterResult<Option<AnalyzeMetrics>> {
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
        let hotspots: HotspotsEnvelope = self
            .get_json(&format!(
                "/indexes/{index_id}/complexity_hotspots?top_n={HOTSPOT_SAMPLE}"
            ))
            .await?;
        let diagnostics: DiagnosticsEnvelope = self
            .get_json(&format!("/indexes/{index_id}/diagnostics"))
            .await?;
        let refactors: RefactorEnvelope = self
            .get_json(&format!("/indexes/{index_id}/refactor-suggestions"))
            .await?;
        Ok(Some(map_metrics(
            &hotspots.hotspots,
            &diagnostics.diagnostics,
            &refactors.suggestions,
        )))
    }
}

#[async_trait]
impl AnalyzeMetricsSource for HttpAnalyzeMetricsSource {
    async fn fetch(&self, index_id: &str) -> Option<AnalyzeMetrics> {
        match self.try_fetch(index_id).await {
            Ok(m) => m,
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
                None
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

/// Fill live analyze metrics into a built [`ReportModel`], honouring the
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
        if let Some(metrics) = source.fetch(&index_id).await {
            eprintln!(
                "[trusty-review report] --analyze: populated metrics for '{}' from index '{index_id}'",
                repo.name
            );
            repo.metrics = Some(metrics);
        }
    }
}

#[cfg(test)]
#[path = "analyze_adapter_tests.rs"]
mod tests;
