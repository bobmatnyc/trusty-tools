//! Tests for the DD-report redaction boundary (#5323).
//!
//! Why: the guarantee under test is a disclosure property of an acquirer-facing
//! artifact, so it is proven at the boundary AND at each of the three producers
//! that cross it — a pure-function test alone would pass just as happily with
//! every call site removed, which is exactly the state this ticket found.
//! What: the pure scrub tests need no environment; the three wiring tests put a
//! recognisable fake credential in `GITHUB_TOKEN` (tier 1 of the same resolver
//! `report_secrets` walks) and assert it does not survive into the model.
//! Test: included as `#[cfg(test)] mod tests` from `redact.rs`.

use super::*;
use crate::report::investigate::{
    Budget, Coverage, Investigation, InvestigationStatus, RepoInvestigation, VerifiedFinding,
    apply_investigation, merge_investigation_prose,
};
use crate::report::metrics::{
    ComplexityBucket, ComplexityDistribution, LanguageLoc, LocMetrics, Severity,
};
use crate::report::model::ReportModel;

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A credential-shaped value long enough to clear `scrub_secrets`' 8-character
/// floor, and distinctive enough that a leak is unambiguous in an assertion.
const FAKE_TOKEN: &str = "ghp_5323FAKETOKENvalue0001"; // pragma: allowlist secret

/// The registry provider whose env var the wiring tests drive.
const TOKEN_ENV: &str = "GITHUB_TOKEN";

/// Set `GITHUB_TOKEN` for the duration of a test and restore it after.
///
/// Why: the wiring tests must prove the real resolver path runs, not a seam, so
/// they mutate process-global state; every one of them carries `#[serial]`.
struct TokenEnvGuard(Option<String>);

impl TokenEnvGuard {
    fn set() -> Self {
        let prev = std::env::var(TOKEN_ENV).ok();
        // SAFETY: `#[serial]` serialises every test that touches this variable,
        // and `Drop` restores the prior value before the lock is released.
        unsafe { std::env::set_var(TOKEN_ENV, FAKE_TOKEN) };
        TokenEnvGuard(prev)
    }
}

impl Drop for TokenEnvGuard {
    fn drop(&mut self) {
        // SAFETY: as above — still inside the `#[serial]` critical section.
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var(TOKEN_ENV, v) },
            None => unsafe { std::env::remove_var(TOKEN_ENV) },
        }
    }
}

fn leaky_finding() -> MetricFinding {
    MetricFinding {
        title: format!("auth failure using {FAKE_TOKEN}"),
        severity: Severity::Red,
        category: format!("linter-{FAKE_TOKEN}"),
        component: format!("src/{FAKE_TOKEN}.rs"),
        description: format!("request rejected: token {FAKE_TOKEN} is expired"),
        remediation: format!("rotate {FAKE_TOKEN}"),
    }
}

fn leaky_metrics() -> AnalyzeMetrics {
    AnalyzeMetrics {
        // #5747: `declared_metrics_file_findings_are_scrubbed` writes this
        // struct out as a declared metrics FILE, and `load_metrics` now refuses
        // a tag whose major it cannot read. `analyze-live-v0` is the live HTTP
        // path's in-memory tag, which never reaches a file; `v0` is what a
        // declared artifact carries.
        schema_version: "v0".to_string(),
        repository: format!("repo-{FAKE_TOKEN}"),
        loc: LocMetrics {
            total: 10,
            by_language: vec![LanguageLoc {
                language: format!("Rust {FAKE_TOKEN}"),
                loc: 10,
            }],
        },
        counts: Default::default(),
        complexity: ComplexityDistribution {
            buckets: vec![ComplexityBucket {
                label: format!("A {FAKE_TOKEN}"),
                count: 3,
            }],
        },
        findings: vec![leaky_finding()],
    }
}

/// Every string an [`AnalyzeMetrics`] carries, for a leak sweep.
fn all_strings(m: &AnalyzeMetrics) -> Vec<String> {
    let mut out = vec![m.repository.clone(), m.schema_version.clone()];
    out.extend(m.loc.by_language.iter().map(|l| l.language.clone()));
    out.extend(m.complexity.buckets.iter().map(|b| b.label.clone()));
    for f in &m.findings {
        out.extend([
            f.title.clone(),
            f.category.clone(),
            f.component.clone(),
            f.description.clone(),
            f.remediation.clone(),
        ]);
    }
    out
}

fn assert_no_leak(strings: &[String], context: &str) {
    for s in strings {
        assert!(
            !s.contains(FAKE_TOKEN),
            "{context}: credential survived into `{s}`"
        );
    }
}

// ── Pure scrub ───────────────────────────────────────────────────────────────

/// Why (#5323): the ticket named `description` and `remediation`, but `title`,
/// `category`, and `component` are copied from the same producer's wire JSON.
/// Covering two of five would close one instance and leave the shape open.
/// Test: itself.
#[test]
fn scrub_finding_covers_every_producer_supplied_field() {
    let mut f = leaky_finding();
    scrub_finding(&mut f, &[FAKE_TOKEN.to_string()]);
    for (name, value) in [
        ("title", &f.title),
        ("category", &f.category),
        ("component", &f.component),
        ("description", &f.description),
        ("remediation", &f.remediation),
    ] {
        assert!(
            !value.contains(FAKE_TOKEN),
            "{name} still carries the credential: {value}"
        );
        assert!(
            value.contains("[REDACTED]"),
            "{name} must show the redaction, not silently drop: {value}"
        );
    }
    assert!(
        f.description.starts_with("request rejected:"),
        "surrounding prose must survive: {}",
        f.description
    );
    assert_eq!(
        f.severity,
        Severity::Red,
        "the band is not text and is kept"
    );
}

/// Why: a bucket label and a language name reach the same rendered page as a
/// finding; scrubbing the findings alone would leave a second door open.
/// Test: itself.
#[test]
fn scrub_metrics_reaches_every_string_field() {
    let mut m = leaky_metrics();
    scrub_metrics(&mut m, &[FAKE_TOKEN.to_string()]);
    assert_no_leak(&all_strings(&m), "scrub_metrics");
    assert_eq!(m.loc.total, 10, "numeric measurements are untouched");
    assert_eq!(m.complexity.buckets[0].count, 3);
}

/// Why: a process holding no resolvable credential must leave the document
/// byte-identical rather than subtly rewritten — the report is diffed run to run.
/// Test: itself.
#[test]
fn scrub_is_a_noop_with_no_needles() {
    let before = leaky_metrics();
    let mut after = leaky_metrics();
    scrub_metrics(&mut after, &[]);
    assert_eq!(all_strings(&before), all_strings(&after));
}

/// Why: [`report_secrets`] exists only to feed the scrub, so what it owes is a
/// usable needle set — never that any particular provider is configured, which
/// depends on the machine running the test.
/// Test: itself.
#[test]
#[serial_test::serial(credential_env)]
fn report_secrets_yields_a_usable_needle_set() {
    let _guard = TokenEnvGuard::set();
    let secrets = report_secrets();
    assert!(
        secrets.iter().any(|s| s == FAKE_TOKEN),
        "the env tier must reach the needle set"
    );
    let mut f = leaky_finding();
    scrub_finding(&mut f, &secrets);
    assert!(!f.description.contains(FAKE_TOKEN), "{}", f.description);
}

// ── Producer wiring ──────────────────────────────────────────────────────────

/// A source that answers one fixed `Fetched` outcome, so the enrichment walk
/// runs without a daemon.
struct LeakySource;

#[async_trait::async_trait]
impl crate::report::analyze_adapter::AnalyzeMetricsSource for LeakySource {
    async fn fetch(&self, _index_id: &str) -> Option<AnalyzeMetrics> {
        Some(leaky_metrics())
    }
}

fn model_with_local_repo() -> ReportModel {
    let manifest = crate::report::manifest::parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"Northwind\"\npath = \".\"\n",
        std::path::Path::new("m.toml"),
    )
    .expect("fixture manifest parses");
    let mut model = ReportModel::build(
        &manifest,
        std::path::Path::new("m.toml"),
        "report-technical-dd",
        None,
    )
    .expect("model builds");
    model.repositories[0].local_path = Some(std::path::PathBuf::from("/tmp/northwind"));
    model
}

/// Why (#5323): producer 1 — the live analyze daemon. Its findings were copied
/// verbatim onto the model and from there into the rendered bands, the exec
/// summary, and the synthesis digest that is sent to an LLM provider. This is
/// the test that fails on the pre-fix tree.
/// Test: itself.
#[tokio::test]
#[serial_test::serial(credential_env)]
async fn enrich_scrubs_configured_credentials_from_findings() {
    let _guard = TokenEnvGuard::set();
    let mut model = model_with_local_repo();

    crate::report::analyze_adapter::enrich_with_analyze_gaps(&mut model, &LeakySource).await;

    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("the stub always fetches");
    assert_no_leak(&all_strings(metrics), "enrich_with_analyze_gaps");
    assert!(
        metrics.findings[0].description.contains("[REDACTED]"),
        "the finding must survive with the value removed, not be dropped: {}",
        metrics.findings[0].description
    );
}

/// An investigation whose every verified-finding string carries the credential.
fn leaky_investigation(slug: &str) -> Investigation {
    Investigation {
        repos: vec![RepoInvestigation {
            verdicts: None,
            slug: slug.to_string(),
            name: "Northwind".to_string(),
            status: InvestigationStatus::Unavailable(format!("provider rejected {FAKE_TOKEN}")),
            findings: vec![VerifiedFinding {
                trace_verdict: String::new(),
                cwe_id: Vec::new(),
                title: format!("hardcoded {FAKE_TOKEN}"),
                severity: Severity::Red,
                dimension: format!("authentication & secrets {FAKE_TOKEN}"),
                file: "src/auth.rs".to_string(),
                line: Some(12),
                evidence_quote: format!("const KEY: &str = \"{FAKE_TOKEN}\";"),
                description: format!("the handler embeds {FAKE_TOKEN}"),
                business_impact: format!("anyone with {FAKE_TOKEN} can act as the service"),
                remediation: format!("revoke {FAKE_TOKEN}"),
                cost_effort: format!("low — rotate {FAKE_TOKEN}"),
            }],
            deps: Default::default(),
            traces: None,
            coverage: Coverage {
                budget: Budget::default(),
                dimensions_covered: vec![format!("secrets {FAKE_TOKEN}")],
                ..Default::default()
            },
            exposure: Vec::new(),
        }],
    }
}

/// Why (#5323): producer 2 — the investigation twin. Its prose is LLM-authored
/// over repository text, making it the likeliest of the three to quote something
/// raw, and it took the identical unscrubbed path onto the model.
///
/// This asserts on the metrics document AND on `model.investigation`, which
/// `reporter.rs` serialises into the JSON twin — the metrics assertion alone
/// would pass with the record itself left raw.
/// Test: itself.
#[test]
#[serial_test::serial(credential_env)]
fn apply_investigation_scrubs_configured_credentials() {
    let _guard = TokenEnvGuard::set();
    let mut model = model_with_local_repo();
    let inv = leaky_investigation(&model.repositories[0].slug.clone());

    apply_investigation(&mut model, &inv);

    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("apply_investigation creates the metrics document");
    assert_no_leak(&all_strings(metrics), "apply_investigation");
    assert!(
        metrics.findings[0].description.contains("[REDACTED]"),
        "{}",
        metrics.findings[0].description
    );
    let recorded = serde_json::to_string(
        model
            .investigation
            .as_ref()
            .expect("the record is stored on the model"),
    )
    .expect("investigation serialises");
    assert!(
        !recorded.contains(FAKE_TOKEN),
        "the record reaches the JSON twin: {recorded}"
    );
}

/// Why (#5323, code-critic round 2): the assertion surface that actually matters.
/// The first round scrubbed the metrics route and stopped there, and every wiring
/// test passed — because `merge_investigation_prose` builds a `FindingProse`
/// straight from the raw `VerifiedFinding`, and `FindingRow::merge_prose`
/// overwrites the scrubbed metrics prose with it unconditionally. Both functions
/// run in the same `cli_report.rs` branch, so the scrub was discarded before
/// render on every real invocation that produced a RED/AMBER finding.
///
/// This drives the pipeline exactly as `cli_report::run_synthesis` does and
/// asserts on the two artifacts a reader receives — the rendered markdown and the
/// JSON twin — rather than on an intermediate that a later stage overwrites.
/// Test: itself.
#[test]
#[serial_test::serial(credential_env)]
fn investigation_credentials_never_reach_the_rendered_report() {
    let _guard = TokenEnvGuard::set();
    let mut model = model_with_local_repo();
    let inv = leaky_investigation(&model.repositories[0].slug.clone());

    // The cli_report.rs order: inject findings, synthesize, overlay prose.
    apply_investigation(&mut model, &inv);
    let mut synthesis = crate::report::synthesize::Synthesis::default();
    merge_investigation_prose(&mut synthesis, &inv);
    model.synthesis = Some(synthesis);

    let template = crate::report::template::TemplateLoader::new()
        .load("report-technical-dd")
        .expect("bundled template loads");
    let dir = tempfile::tempdir().expect("tempdir");
    let markdown = crate::report::reporter::Reporter::new(dir.path()).render(&model, &template);

    assert!(
        !markdown.contains(FAKE_TOKEN),
        "credential rendered into the report markdown"
    );
    assert!(
        markdown.contains("[REDACTED]"),
        "the finding must render with the value removed, not be dropped"
    );

    let twin = serde_json::to_string(&model).expect("model serialises");
    assert!(
        !twin.contains(FAKE_TOKEN),
        "credential reached the JSON twin"
    );
}

/// Why (#5323): the synthesis sink in isolation. `evidence` has no metrics route
/// at all — `raw_evidence` renders it byte-for-byte verbatim inside a fence — so
/// it is only ever scrubbed here.
/// Test: itself.
#[test]
fn scrub_prose_covers_evidence_and_every_narrative_field() {
    let mut prose = crate::report::synthesize::FindingProse {
        trace_verdict: String::new(),
        cwe_id: Vec::new(),
        app_slug: "northwind".to_string(),
        title: format!("t {FAKE_TOKEN}"),
        severity: "RED".to_string(),
        description: format!("d {FAKE_TOKEN}"),
        evidence: format!("e {FAKE_TOKEN}"),
        component: format!("c {FAKE_TOKEN}"),
        business_impact: format!("b {FAKE_TOKEN}"),
        remediation: format!("r {FAKE_TOKEN}"),
        cost_effort: format!("x {FAKE_TOKEN}"),
        evidence_measured: true,
    };
    scrub_prose(&mut prose, &[FAKE_TOKEN.to_string()]);
    for (name, value) in [
        ("title", &prose.title),
        ("description", &prose.description),
        ("evidence", &prose.evidence),
        ("component", &prose.component),
        ("business_impact", &prose.business_impact),
        ("remediation", &prose.remediation),
        ("cost_effort", &prose.cost_effort),
    ] {
        assert!(!value.contains(FAKE_TOKEN), "{name} leaked: {value}");
        assert!(value.contains("[REDACTED]"), "{name}: {value}");
    }
    assert_eq!(prose.severity, "RED", "the band label is report-authored");
    assert_eq!(prose.app_slug, "northwind");
}

/// Why (#5323): the record reaches the JSON twin and the rendered coverage
/// sections whole, so the status reason and the coverage lists must be scrubbed
/// too — a failure reason can quote a provider's error body verbatim.
/// Test: itself.
#[test]
fn scrub_investigation_reaches_the_whole_tree() {
    let mut inv = leaky_investigation("northwind");
    scrub_investigation(&mut inv, &[FAKE_TOKEN.to_string()]);
    let serialised = serde_json::to_string(&inv).expect("serialises");
    assert!(!serialised.contains(FAKE_TOKEN), "{serialised}");
    assert!(serialised.contains("[REDACTED]"), "{serialised}");
}

/// Why (#5323): producer 3 — a metrics JSON declared in the manifest. The ticket
/// named only the two live producers, but this path lands the same struct on the
/// same model and would have re-opened the gap for anyone generating that file
/// from a tool that quotes its own environment.
/// Test: itself.
#[test]
#[serial_test::serial(credential_env)]
fn declared_metrics_file_findings_are_scrubbed() {
    let _guard = TokenEnvGuard::set();
    let dir = tempfile::tempdir().expect("tempdir");
    let metrics_path = dir.path().join("metrics.json");
    std::fs::write(
        &metrics_path,
        serde_json::to_string(&leaky_metrics()).expect("fixture serialises"),
    )
    .expect("write metrics fixture");

    let manifest_path = dir.path().join("m.toml");
    let manifest = crate::report::manifest::parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"Northwind\"\npath = \".\"\nmetrics = \"metrics.json\"\n",
        &manifest_path,
    )
    .expect("fixture manifest parses");

    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");

    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("declared metrics load");
    assert_no_leak(&all_strings(metrics), "ReportModel::build");
}
