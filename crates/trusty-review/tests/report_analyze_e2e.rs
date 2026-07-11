//! End-to-end `report --analyze` against an in-process trusty-analyze mock (#2449).
//!
//! Why: proves the deterministic analyze integration composes end to end — a
//! bare report with NO hand-authored metrics and NO `--synthesize`, fed by a
//! STUBBED analyze responder (never a real daemon), populates the
//! complexity-distribution chart AND the RED/AMBER finding bands; and a fetch
//! failure falls through cleanly to scan-only output without aborting.
//! What: stands up a tiny in-process HTTP/1.1 mock that serves the four analyze
//! endpoints from fixture JSON, builds a model over a real local checkout,
//! runs `enrich_with_analyze`, and asserts the rendered markdown.
//! Test: this file (only compiled with the default `report` feature).
#![cfg(feature = "report")]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;

use trusty_review::report::{
    HttpAnalyzeMetricsSource, Reporter, TemplateLoader, enrich_with_analyze, load_manifest,
    model::ReportModel,
};

// ─── In-process analyze mock ─────────────────────────────────────────────────

/// The index id the mock serves; must equal the repo checkout basename so
/// `derive_index_id` resolves to it.
const INDEX_ID: &str = "acme-core";

/// Body for `GET /indexes` — one served index (analyze shape: array of objects).
fn indexes_body() -> String {
    format!(r#"[{{"id":"{INDEX_ID}","root_path":null}}]"#)
}

/// Body for `/complexity_hotspots` — hotspots carrying the #2446 numbers.
fn hotspots_body() -> String {
    r#"{"index_id":"acme-core","top_n":1000,"hotspots":[
        {"id":"a:1:9","file":"src/a.rs","start_line":1,"end_line":9,"content":"fn a(){}","cyclomatic":25,"cognitive":30},
        {"id":"b:1:9","file":"src/b.rs","start_line":1,"end_line":9,"content":"fn b(){}","cyclomatic":12,"cognitive":8},
        {"id":"c:1:9","file":"src/c.rs","start_line":1,"end_line":9,"content":"fn c(){}","cyclomatic":3,"cognitive":1}
    ]}"#
    .to_string()
}

/// Body for `/diagnostics` — one error (RED) + one hint (dropped GREEN).
fn diagnostics_body() -> String {
    r#"{"index_id":"acme-core","total":2,"diagnostics":[
        {"tool":"clippy","file":"src/a.rs","line":1,"col":1,"severity":"error","code":"E0001","message":"boom"},
        {"tool":"clippy","file":"src/b.rs","line":2,"col":1,"severity":"hint","code":"H1","message":"meh"}
    ]}"#
    .to_string()
}

/// Body for `/refactor-suggestions` — one high (AMBER) suggestion.
fn refactor_body() -> String {
    r#"{"index_id":"acme-core","count":1,"suggestions":[
        {"chunk_id":"a:1:9","file":"src/a.rs","line_start":1,"line_end":9,"function_name":"a","refactor_type":"extract_method","severity":"high","rationale":"x","suggested_action":"y","complexity_before":25,"complexity_after":8,"smells":[]}
    ]}"#
    .to_string()
}

/// Route the request path to the right fixture body.
fn body_for(path: &str) -> String {
    if path.starts_with("/indexes/") && path.contains("/complexity_hotspots") {
        hotspots_body()
    } else if path.contains("/diagnostics") {
        diagnostics_body()
    } else if path.contains("/refactor-suggestions") {
        refactor_body()
    } else if path == "/indexes" || path.starts_with("/indexes?") {
        indexes_body()
    } else {
        "{}".to_string()
    }
}

/// Spawn a blocking HTTP/1.1 mock serving the analyze endpoints; returns its
/// base URL. The listener thread runs for the process lifetime (daemonised).
fn spawn_mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            // Request line: `GET <path> HTTP/1.1`.
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let body = body_for(&path);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// Create a real local checkout named `acme-core` with one source file so the
/// built-in scan is non-empty and `local_path` resolves. Returns the temp dir
/// (kept alive) and the manifest path.
fn write_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = dir.join(INDEX_ID);
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
    let base_url = spawn_mock();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let manifest_path = write_fixture(tmp.path());

    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build");

    // Precondition: no declared metrics — the chart/findings are empty pre-fetch.
    assert!(model.repositories[0].metrics.is_none());

    let source = HttpAnalyzeMetricsSource::new(base_url).expect("client");
    enrich_with_analyze(&mut model, &source).await;

    // The live fetch populated metrics.
    let metrics = model.repositories[0]
        .metrics
        .as_ref()
        .expect("analyze filled metrics");
    // loc/counts stay empty (scanner owns them).
    assert_eq!(metrics.loc.total, 0);
    // Buckets computed client-side: F (25), C (12), A (3).
    let labels: Vec<&str> = metrics
        .complexity
        .buckets
        .iter()
        .map(|b| b.label.as_str())
        .collect();
    assert!(labels.contains(&"F: very high (>20)"), "{labels:?}");
    assert!(labels.contains(&"C: elevated (11-15)"), "{labels:?}");

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
    // The GREEN-mapped hint diagnostic is NOT rendered.
    assert!(
        !md.contains("H1"),
        "green-mapped diagnostic should be dropped"
    );
}

/// Why: a fetch failure must fall through cleanly to scan-only output, never
/// abort. What: points the source at a dead port; asserts metrics stays None
/// and the report still renders. Test: this test itself.
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

    // Port 1 is never listening — the probe fails, fetch is fail-open.
    let source = HttpAnalyzeMetricsSource::new("http://127.0.0.1:1").expect("client");
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
