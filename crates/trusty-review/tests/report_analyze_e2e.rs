//! End-to-end `report --analyze` against an in-process trusty-analyze mock (#2449).
//!
//! Why: proves the deterministic analyze integration composes end to end — a
//! bare report with NO hand-authored metrics and NO `--synthesize`, fed by a
//! STUBBED analyze responder (never a real daemon), populates the
//! complexity-distribution chart AND the RED/AMBER finding bands; and a fetch
//! failure falls through cleanly to scan-only output without aborting.
//! What: stands up a tiny in-process JSON-RPC mock on a Unix socket that serves
//! the four analyze methods from fixture JSON, builds a model over a real local
//! checkout, runs `enrich_with_analyze`, and asserts the rendered markdown.
//!
//! #6287 (ADR-0032): the mock was an HTTP/1.1 listener on loopback. trusty-analyze
//! serves JSON-RPC over a Unix socket now, so the mock speaks that instead —
//! the fixture bodies and every assertion below are unchanged.
//! Test: this file (only compiled with the default `report` feature).
#![cfg(feature = "report")]

use std::path::PathBuf;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use trusty_review::report::{
    HttpAnalyzeMetricsSource, Reporter, TemplateLoader, enrich_with_analyze, load_manifest,
    model::ReportModel,
};

// ─── In-process analyze mock ─────────────────────────────────────────────────

/// Basename of the fixture checkout.
///
/// #6149: this is no longer the index id. The id is derived from the checkout's
/// CANONICAL PATH, which is inside a per-run tempdir, so the mock is told which
/// id to serve rather than knowing one at compile time.
const REPO_DIR: &str = "acme-core";

/// Body for `analyze.list_indexes` — one served index (array of objects).
fn indexes_body(index_id: &str) -> String {
    format!(r#"[{{"id":"{index_id}","root_path":null}}]"#)
}

/// Body for `analyze.complexity_distribution` — the whole-corpus A-F histogram (#5320).
fn distribution_body() -> String {
    r#"{"index_id":"acme-core","total":3,"skipped_non_code":1,"buckets":[
        {"grade":"A","label":"A: simple (0-5)","count":1},
        {"grade":"B","label":"B: moderate (6-10)","count":0},
        {"grade":"C","label":"C: elevated (11-15)","count":1},
        {"grade":"D","label":"D: high (16-20)","count":0},
        {"grade":"F","label":"F: very high (>20)","count":1}
    ]}"#
    .to_string()
}

/// Body for `analyze.diagnostics` — one error (RED) + one hint (dropped GREEN).
fn diagnostics_body() -> String {
    r#"{"index_id":"acme-core","total":2,"tools_run":["clippy"],"diagnostics":[
        {"tool":"clippy","file":"src/a.rs","line":1,"col":1,"severity":"error","code":"E0001","message":"boom"},
        {"tool":"clippy","file":"src/b.rs","line":2,"col":1,"severity":"hint","code":"H1","message":"meh"}
    ]}"#
    .to_string()
}

/// Body for `analyze.refactor_suggestions` — one high (AMBER) suggestion.
fn refactor_body() -> String {
    r#"{"index_id":"acme-core","count":1,"suggestions":[
        {"chunk_id":"a:1:9","file":"src/a.rs","line_start":1,"line_end":9,"function_name":"a","refactor_type":"extract_method","severity":"high","rationale":"x","suggested_action":"y","complexity_before":25,"complexity_after":8,"smells":[]}
    ]}"#
    .to_string()
}

/// Route the requested JSON-RPC method to the right fixture body.
fn body_for(request: &str, index_id: &str) -> String {
    if request.contains("analyze.complexity_distribution") {
        distribution_body()
    } else if request.contains("analyze.diagnostics") {
        diagnostics_body()
    } else if request.contains("analyze.refactor_suggestions") {
        refactor_body()
    } else if request.contains("analyze.list_indexes") {
        indexes_body(index_id)
    } else {
        "{}".to_string()
    }
}

/// Spawn a JSON-RPC mock on a Unix socket; returns the socket path.
///
/// The `TempDir` holding the socket is returned with it, because dropping it
/// unlinks the socket out from under the accept loop.
fn spawn_mock(index_id: String, dir: &std::path::Path) -> PathBuf {
    let socket = dir.join("analyze.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the mock socket");
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let index_id = index_id.clone();
            tokio::spawn(async move {
                // The client half-closes after one frame, so reading to EOF is
                // the whole request.
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let request = String::from_utf8_lossy(&sink).into_owned();
                // Compacted, not interpolated raw: the fixtures are
                // pretty-printed and a frame is newline-terminated, so
                // embedding one verbatim would end the frame at its first line
                // break.
                let value: serde_json::Value =
                    serde_json::from_str(&body_for(&request, &index_id)).expect("a valid fixture");
                let reply = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{value}}}"#);
                let _ = conn.write_all(reply.as_bytes()).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            });
        }
    });
    socket
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// Create a real local checkout named `acme-core` with one source file so the
/// built-in scan is non-empty and `local_path` resolves. Returns the temp dir
/// (kept alive) and the manifest path.
fn write_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = dir.join(REPO_DIR);
    std::fs::create_dir_all(repo.join("src")).expect("mkdir repo");
    std::fs::write(repo.join("src").join("a.rs"), "fn a() {}\n").expect("write src");

    let manifest_toml = format!(
        r#"
        [report]
        title = "Acme Technical DD"
        template = "report-technical-dd"

        [[repositories]]
        name = "Acme Core"
        path = "{}"
    "#,
        repo.display()
    );
    let manifest_path = dir.join("manifest.toml");
    std::fs::write(&manifest_path, manifest_toml).expect("write manifest");
    manifest_path
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// Why: prove a bare `--analyze` run (no metrics file, no synthesis) fills the
/// complexity chart AND RED/AMBER finding bands from the mocked daemon.
/// What: builds the model, enriches from the mock source, renders, asserts.
/// Test: this test itself.
#[tokio::test]
async fn analyze_populates_complexity_and_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest_path = write_fixture(tmp.path());
    // #6149: the id is derived from the checkout's canonical path, so the mock
    // is told which index to serve — deriving it here through the same public
    // function the enrichment calls is also what proves the two agree.
    let index_id = trusty_review::report::derive_index_id(&tmp.path().join(REPO_DIR))
        .expect("the fixture checkout has a final path component");
    assert!(
        index_id.starts_with("acme-core-"),
        "readable, and per-checkout: {index_id}"
    );
    let socket = spawn_mock(index_id, tmp.path());

    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build");

    // Precondition: no declared metrics — the chart/findings are empty pre-fetch.
    assert!(model.repositories[0].metrics.is_none());

    let source = HttpAnalyzeMetricsSource::new(socket).expect("client");
    enrich_with_analyze(&mut model, &source).await;

    // The live fetch populated metrics.
    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("analyze filled metrics");
    // loc/counts stay empty (scanner owns them).
    assert_eq!(metrics.loc.total, 0);
    // Every band from the daemon's full histogram (#5320) — not a bucketed
    // top-N sample.
    let labels: Vec<&str> = metrics
        .complexity
        .buckets
        .iter()
        .map(|b| b.label.as_str())
        .collect();
    assert_eq!(labels.len(), 5, "every band renders: {labels:?}");
    assert!(labels.contains(&"F: very high (>20)"), "{labels:?}");
    assert!(labels.contains(&"C: elevated (11-15)"), "{labels:?}");
    assert_eq!(
        metrics
            .complexity
            .buckets
            .iter()
            .map(|b| b.count)
            .sum::<u64>(),
        3,
        "the percentage denominator is the counted population"
    );

    let reporter = Reporter::new(tmp.path().join("reports"));
    let md = reporter.render(&model, &template);

    // §7 complexity-distribution table + mermaid chart populated.
    assert!(
        md.contains("F: very high (>20)"),
        "complexity table missing bucket"
    );
    assert!(md.contains("```mermaid"), "mermaid chart missing");
    // RED finding (error diagnostic) and AMBER finding (high refactor) rendered.
    assert!(md.contains("E0001"), "RED finding title missing");
    assert!(
        md.contains("Extract method — a"),
        "AMBER refactor finding missing"
    );
    assert_green_diagnostic_dropped(metrics, &md);
}

/// Assert the GREEN-mapped hint diagnostic reached neither the metrics nor the
/// page.
///
/// Why: #4385/#4387 reported this check failing three times on unrelated PRs,
/// and read it as a timing flake in the mock. It is not timing. The check was
/// `!md.contains("H1")` over the WHOLE rendered report, and the report embeds
/// the manifest's absolute path — `…/T/.tmpXYZabc/manifest.toml`, whose six
/// random characters come from the full alphanumeric alphabet and land on the
/// two-character sequence `H1` roughly once in eight hundred renders. Every
/// sighting was that collision: a passing render and a failing one differed
/// only in the name the OS handed the temp directory.
/// What: checks the structured findings for the dropped code, then the two
/// shapes the renderer would print it in — the risk-table row `H1 — meh` and
/// the detail heading `**H1**`. Neither can appear in a filesystem path.
/// Test: called by `analyze_populates_complexity_and_findings` and
/// `green_diagnostic_stays_dropped_under_a_path_that_spells_its_code`.
fn assert_green_diagnostic_dropped(metrics: &trusty_review::report::AnalyzeMetrics, md: &str) {
    assert!(
        !metrics.findings.iter().any(|f| f.title == "H1"),
        "a hint diagnostic maps to GREEN and must never become a finding: {:?}",
        metrics.findings
    );
    assert!(
        !md.contains("H1 — meh"),
        "the dropped diagnostic must not reach the risk table"
    );
    assert!(
        !md.contains("**H1**"),
        "the dropped diagnostic must not reach the findings detail"
    );
}

/// REGRESSION (#4387, recurrence of #4385): the drop check survives a workspace
/// path that happens to spell the dropped diagnostic's code.
///
/// Why: this is the flake, made deterministic. Three CI failures on unrelated
/// PRs were read as a race in the mock; the mock had already been rewritten
/// from a raw `TcpListener` to a Unix socket (#6287) and the failures continued
/// in the same shape, because the cause was never the transport. It was
/// `md.contains("H1")` run over a document that embeds a random temp path. This
/// test pins the workspace under a directory literally named `runH1x` — the
/// collision the OS produced by chance — and asserts the check still holds.
/// What: same fixture, mock, enrichment and render as
/// `analyze_populates_complexity_and_findings`, rooted one directory deeper.
/// Under the old whole-document substring check this fails every run.
/// Test: this test itself.
#[tokio::test]
async fn green_diagnostic_stays_dropped_under_a_path_that_spells_its_code() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // The name is the point: `H1` appears in the path the report prints.
    let workspace = tmp.path().join("runH1x");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");

    let manifest_path = write_fixture(&workspace);
    let index_id = trusty_review::report::derive_index_id(&workspace.join(REPO_DIR))
        .expect("the fixture checkout has a final path component");
    let socket = spawn_mock(index_id, &workspace);

    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build");

    let source = HttpAnalyzeMetricsSource::new(socket).expect("client");
    enrich_with_analyze(&mut model, &source).await;

    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("analyze filled metrics");
    let reporter = Reporter::new(workspace.join("reports"));
    let md = reporter.render(&model, &template);

    assert!(
        md.contains("runH1x"),
        "the rendered report must embed the workspace path — otherwise this test \
         proves nothing about the collision"
    );
    assert_green_diagnostic_dropped(metrics, &md);
}

/// Why: a fetch failure must fall through cleanly to scan-only output, never
/// abort. What: points the source at a socket nothing bound; asserts metrics
/// stays None and the report still renders. Test: this test itself.
#[tokio::test]
async fn analyze_fetch_failure_falls_through_to_scan() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest_path = write_fixture(tmp.path());
    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build");

    // Nothing ever bound this path — the probe fails, fetch is fail-open.
    let source =
        HttpAnalyzeMetricsSource::new(tmp.path().join("absent-analyze.sock")).expect("client");
    enrich_with_analyze(&mut model, &source).await;

    assert!(
        model.repositories[0].metrics.is_none(),
        "failed fetch must leave metrics unset"
    );
    // The report still renders (scan-only) — no panic, no abort.
    let reporter = Reporter::new(tmp.path().join("reports"));
    let md = reporter.render(&model, &template);
    assert!(md.contains("Acme Technical DD"));
    // No fabricated complexity buckets from an empty fetch.
    assert!(!md.contains("F: very high (>20)"));
}
