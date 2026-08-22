//! Unit tests for the trusty-analyze → `AnalyzeMetrics` adapter (#2447).
//!
//! Why: the mapping must be provable against fixture JSON with NO live daemon —
//! these tests pin the envelope parsing, the severity convention, the
//! complexity-bucket thresholds, and the fail-open behaviour.
//! What: drives the private mapping helpers and the public fetch seam directly.
//! Test: this file.

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// A budget short enough to keep the transport tests fast and long enough that
/// a loopback stub always beats it. Only the timeout tests want it hit.
const CHEAP_TEST_BUDGET: Duration = Duration::from_secs(5);

// ─── Severity map ────────────────────────────────────────────────────────────

#[test]
fn severity_map_diagnostics() {
    assert_eq!(map_diagnostic_severity("error"), Severity::Red);
    assert_eq!(map_diagnostic_severity("warning"), Severity::Amber);
    assert_eq!(map_diagnostic_severity("info"), Severity::Green);
    assert_eq!(map_diagnostic_severity("hint"), Severity::Green);
    assert_eq!(map_diagnostic_severity("UNKNOWN"), Severity::Green);
}

#[test]
fn severity_map_refactors() {
    assert_eq!(map_refactor_severity("critical"), Severity::Amber);
    assert_eq!(map_refactor_severity("high"), Severity::Amber);
    assert_eq!(map_refactor_severity("medium"), Severity::Green);
    assert_eq!(map_refactor_severity("low"), Severity::Green);
}

/// Why (#5317): the report's most severe band is a statement about business
/// risk. A refactor suggestion's severity is derived from a complexity grade
/// alone, so `critical` there means "grade F" and must never be promoted to
/// RED — twenty "Extract method" entries did exactly that in two generated
/// due-diligence reports.
/// Test: itself.
#[test]
fn refactor_never_reaches_red() {
    for severity in [
        "critical", "CRITICAL", "error", "high", "warning", "low", "medium", "?",
    ] {
        assert_ne!(
            map_refactor_severity(severity),
            Severity::Red,
            "refactor severity {severity:?} reached the RED band"
        );
    }
}

// ─── Complexity distribution ─────────────────────────────────────────────────

/// Why (#5320): the distribution is rendered as a table whose percentage column
/// is a share of the bucket sum. It is only honest if the buckets are the whole
/// population — which means every band the daemon reports, zero-count bands
/// included, arrives intact.
/// Test: itself.
#[test]
fn distribution_maps_every_band() {
    let env: DistributionEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "total": 1000, "skipped_non_code": 7, "buckets": [
             { "grade": "A", "label": "A: simple (0-5)", "count": 800 },
             { "grade": "B", "label": "B: moderate (6-10)", "count": 150 },
             { "grade": "C", "label": "C: elevated (11-15)", "count": 0 },
             { "grade": "D", "label": "D: high (16-20)", "count": 30 },
             { "grade": "F", "label": "F: very high (>20)", "count": 20 }
           ]}"#,
    )
    .unwrap();
    let dist = map_distribution(&env);
    assert_eq!(dist.buckets.len(), 5, "no band is dropped");
    assert_eq!(dist.buckets[0].label, "A: simple (0-5)");
    assert_eq!(dist.buckets[0].count, 800);
    assert_eq!(dist.buckets[2].count, 0, "an empty band is a measurement");
    assert_eq!(
        dist.buckets.iter().map(|b| b.count).sum::<u64>(),
        env.total,
        "the rendered percentages must be shares of the counted population"
    );
}

#[test]
fn empty_distribution_maps_to_nothing() {
    let env: DistributionEnvelope =
        serde_json::from_str(r#"{ "total": 0, "buckets": [] }"#).unwrap();
    assert!(map_distribution(&env).buckets.is_empty());
}

// ─── Finding synthesis ───────────────────────────────────────────────────────

#[test]
fn diagnostic_finding_synthesises_title() {
    let d: WireDiagnostic = serde_json::from_str(
        r#"{ "tool": "clippy", "file": "src/a.rs", "line": 3, "col": 1,
             "severity": "error", "code": "clippy::needless_return",
             "message": "unneeded return statement" }"#,
    )
    .unwrap();
    let f = diagnostic_finding(&d).expect("error → red finding");
    assert_eq!(f.title, "clippy::needless_return");
    assert_eq!(f.severity, Severity::Red);
    assert_eq!(f.category, "clippy");
    assert_eq!(f.component, "src/a.rs");
}

#[test]
fn diagnostic_finding_without_code_uses_tool_name() {
    let d: WireDiagnostic =
        serde_json::from_str(r#"{ "tool": "ruff", "file": "a.py", "severity": "warning" }"#)
            .unwrap();
    let f = diagnostic_finding(&d).expect("warning → amber");
    assert_eq!(f.title, "ruff diagnostic");
    assert_eq!(f.severity, Severity::Amber);
}

#[test]
fn diagnostic_finding_drops_green() {
    let d: WireDiagnostic =
        serde_json::from_str(r#"{ "tool": "ruff", "file": "a.py", "severity": "hint" }"#).unwrap();
    assert!(diagnostic_finding(&d).is_none());
}

#[test]
fn refactor_finding_synthesises_title() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "src/cfg.rs", "function_name": "parse_config",
             "refactor_type": "extract_method", "severity": "critical" }"#,
    )
    .unwrap();
    let f = refactor_finding(&r).expect("critical → amber");
    assert_eq!(f.title, "Extract method — parse_config");
    assert_eq!(f.severity, Severity::Amber);
    assert_eq!(f.category, "maintainability");
    assert_eq!(f.component, "src/cfg.rs");
}

// ─── Nameless regions are impl blocks, not functions (#6082) ─────────────────

/// The exact payload trusty-analyze returned for `impl HnswStore { … }` in the
/// dogfood run: a region with no function name, whose prose calls it a function.
fn nameless_region() -> WireRefactor {
    serde_json::from_str(
        r#"{ "file": "crates/trusty-common/src/memory_core/store/hnsw_store.rs",
             "refactor_type": "extract_method", "severity": "critical",
             "rationale": "cyclomatic complexity 140 (grade F); smells: long_function(603 lines), deep_nesting(depth 6)",
             "suggested_action": "Extract the body of 'this function' (lines 342–945) into 2–3 smaller functions" }"#,
    )
    .unwrap()
}

/// #6082: the top four maintainability findings of the dogfood report were
/// `impl` blocks titled "Extract method" and remediated by extracting a function
/// body. The region carries no function name, which is exactly what identifies
/// it, and the remediation must name an action a reader can take on an impl.
#[test]
fn a_nameless_region_is_labelled_an_impl_block() {
    let f = refactor_finding(&nameless_region()).expect("critical → amber");

    assert_eq!(f.title, "Split oversized impl block");
    assert!(f.remediation.contains("Split the impl block"), "{f:?}");
    assert!(f.remediation.contains("(lines 342–945)"), "{f:?}");
    assert!(
        f.description.contains("long_impl_block(603 lines)"),
        "the long_function smell is the same mislabel one level down: {f:?}"
    );
    assert!(!f.description.contains("long_function"), "{f:?}");
}

/// The literal placeholder must never reach the page — it appeared 17 times in
/// one report, and "the body of 'this function'" names nothing a reader can find.
#[test]
fn a_nameless_region_never_prints_the_this_function_placeholder() {
    let f = refactor_finding(&nameless_region()).expect("critical → amber");
    for field in [&f.title, &f.description, &f.remediation] {
        assert!(!field.contains("this function"), "{field}");
    }
}

/// With a function name in hand, the daemon's own placeholder is replaced by the
/// name rather than passed through.
#[test]
fn a_named_function_replaces_the_placeholder_with_its_name() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "src/cfg.rs", "function_name": "parse_config",
             "refactor_type": "extract_method", "severity": "critical",
             "suggested_action": "Extract the body of 'this function' into 2-3 smaller functions" }"#,
    )
    .unwrap();

    let f = refactor_finding(&r).expect("critical → amber");

    assert!(!f.remediation.contains("this function"), "{f:?}");
    assert!(f.remediation.contains("`parse_config`"), "{f:?}");
}

/// A nameless region with neither a line range nor a rationale states nothing
/// that identifies it. Suppressing beats mislabelling.
#[test]
fn an_unidentifiable_nameless_region_is_suppressed() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "src/cfg.rs", "refactor_type": "extract_method",
             "severity": "critical", "suggested_action": "Extract the body of 'this function'" }"#,
    )
    .unwrap();

    assert!(refactor_finding(&r).is_none());
}

// ─── Repo-relative components (#6082) ────────────────────────────────────────

fn metrics_with_components(components: &[&str]) -> AnalyzeMetrics {
    AnalyzeMetrics {
        findings: components
            .iter()
            .map(|c| MetricFinding {
                title: "t".to_string(),
                severity: Severity::Amber,
                category: "maintainability".to_string(),
                component: (*c).to_string(),
                description: "d".to_string(),
                remediation: "r".to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

/// #6082: the daemon reports absolute paths, so 21 `**Component:**` lines
/// carried the auditor's own filesystem layout into a document written for
/// someone outside that machine. The investigation pass cites repo-relative
/// paths, so one report used two path vocabularies for the same files.
#[test]
fn components_are_made_repo_relative() {
    let root = Path::new("/Users/x/repos/local/trusty-tools");
    let mut m = metrics_with_components(&[
        "/Users/x/repos/local/trusty-tools/crates/trusty-common/src/tickets/server.rs",
        "/Users/x/repos/local/trusty-tools/crates/trusty-search/src/main.rs:1117",
    ]);

    relativize_components(&mut m, root);

    assert_eq!(
        m.findings[0].component,
        "crates/trusty-common/src/tickets/server.rs"
    );
    assert_eq!(
        m.findings[1].component, "crates/trusty-search/src/main.rs:1117",
        "a trailing :line suffix survives the strip"
    );
}

/// Normalising known paths must never rewrite one it does not recognise.
#[test]
fn a_component_outside_the_checkout_is_left_alone() {
    let root = Path::new("/Users/x/repos/local/trusty-tools");
    let mut m = metrics_with_components(&["/opt/vendor/lib.rs", "crates/already/relative.rs", ""]);

    relativize_components(&mut m, root);

    assert_eq!(m.findings[0].component, "/opt/vendor/lib.rs");
    assert_eq!(m.findings[1].component, "crates/already/relative.rs");
    assert_eq!(m.findings[2].component, "");
}

/// Why (#5317): every field but the component rendered as
/// `not stated in source data`, because the adapter dropped the rationale and
/// the suggested action the daemon had already returned. A finding that states
/// neither an observation nor an action is not worth a numbered slot.
/// Test: itself.
#[test]
fn refactor_finding_carries_rationale_and_action() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "src/cfg.rs", "function_name": "parse_config",
             "refactor_type": "extract_method", "severity": "critical",
             "rationale": "cyclomatic complexity 31 (grade F)",
             "suggested_action": "Extract the body of 'parse_config' into 2-3 smaller functions" }"#,
    )
    .unwrap();
    let f = refactor_finding(&r).expect("critical → amber");
    assert_eq!(f.description, "cyclomatic complexity 31 (grade F)");
    assert!(f.remediation.starts_with("Extract the body of"));
    assert!(!f.is_contentless());
}

/// Why (#5317): a finding carrying only a title and a path renders as three
/// honesty markers in a row. Dropping it is the honest outcome; the count that
/// remains is what the reader can act on.
/// Test: itself.
#[test]
fn contentless_findings_are_dropped() {
    let refactors: Vec<WireRefactor> = serde_json::from_str(
        r#"[
             { "file": "a.rs", "refactor_type": "extract_method", "severity": "critical" },
             { "file": "b.rs", "refactor_type": "extract_method", "severity": "critical",
               "rationale": "cyclomatic complexity 40 (grade F)",
               "suggested_action": "Split b" }
           ]"#,
    )
    .unwrap();
    let m = map_metrics(None, &[], &refactors);
    assert_eq!(m.findings.len(), 1, "the bare-title entry is dropped");
    assert_eq!(m.findings[0].component, "b.rs");
}

#[test]
fn refactor_finding_drops_green() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "a.rs", "refactor_type": "reduce_nesting", "severity": "medium" }"#,
    )
    .unwrap();
    assert!(refactor_finding(&r).is_none());
}

// ─── Envelope + full mapping ─────────────────────────────────────────────────

#[test]
fn map_metrics_populates_complexity_and_findings() {
    // Real-shaped envelopes: the distribution is the whole-corpus histogram.
    let distribution: DistributionEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "total": 2, "skipped_non_code": 0, "buckets": [
             { "grade": "A", "label": "A: simple (0-5)", "count": 1 },
             { "grade": "B", "label": "B: moderate (6-10)", "count": 0 },
             { "grade": "C", "label": "C: elevated (11-15)", "count": 0 },
             { "grade": "D", "label": "D: high (16-20)", "count": 0 },
             { "grade": "F", "label": "F: very high (>20)", "count": 1 }
           ]}"#,
    )
    .unwrap();
    let diagnostics: DiagnosticsEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "total": 2, "diagnostics": [
             { "tool": "clippy", "file": "a.rs", "line": 1, "col": 1,
               "severity": "error", "code": "E0001", "message": "boom" },
             { "tool": "clippy", "file": "b.rs", "line": 2, "col": 1,
               "severity": "hint", "code": "H1", "message": "meh" }
           ]}"#,
    )
    .unwrap();
    let refactors: RefactorEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "count": 1, "suggestions": [
             { "chunk_id": "a:1:9", "file": "a.rs", "line_start": 1, "line_end": 9,
               "function_name": "a", "refactor_type": "extract_method",
               "severity": "high", "rationale": "x", "suggested_action": "y",
               "complexity_before": 25, "complexity_after": 8, "smells": [] }
           ]}"#,
    )
    .unwrap();

    let m = map_metrics(
        Some(&distribution),
        &diagnostics.diagnostics,
        &refactors.suggestions,
    );

    // loc/counts stay empty — the scanner owns them.
    assert_eq!(m.loc.total, 0);
    assert_eq!(m.counts.files, 0);

    // Every band from the daemon's histogram, in its order.
    let labels: Vec<&str> = m
        .complexity
        .buckets
        .iter()
        .map(|b| b.label.as_str())
        .collect();
    assert_eq!(labels.len(), 5);
    assert!(labels.contains(&"F: very high (>20)"));
    assert!(labels.contains(&"A: simple (0-5)"));

    // Findings: the error diagnostic (RED) + the high refactor (AMBER); the
    // hint diagnostic is dropped (GREEN).
    assert_eq!(m.findings.len(), 2);
    let red = m
        .findings
        .iter()
        .find(|f| f.severity == Severity::Red)
        .unwrap();
    assert_eq!(red.title, "E0001");
    let amber = m
        .findings
        .iter()
        .find(|f| f.severity == Severity::Amber)
        .unwrap();
    assert_eq!(amber.title, "Extract method — a");
}

// ─── Fail-open fetch ─────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_returns_none_on_unreachable_daemon() {
    // Port 1 is never listening; the probe fails and fetch swallows it.
    let src = HttpAnalyzeMetricsSource::new("http://127.0.0.1:1").unwrap();
    assert!(src.fetch("demo").await.is_none());
}

/// A loopback stub that closes a connection part-way through a request instead
/// of answering it, and counts how many connections it accepted.
///
/// Why (#6038): this is the failure the field hit. HTTP/1.1 keep-alive lets the
/// server close a connection the client still believes is reusable, and the
/// close races the client's next write — so the client sees the socket go away
/// after it has already committed the request. Closing on the Nth request of a
/// connection (rather than right after a response) makes the race
/// deterministic: the connection is provably still in the client's pool when
/// the next GET is checked out.
/// What: binds `127.0.0.1:0` and serves `[]` to every request except the
/// `poison_at`-th on each connection, which it answers by shutting the socket
/// down. Returns `host:port` plus the accepted-connection counter.
/// Test: `transport_failure_on_a_reused_connection_retries_once`,
/// `a_connection_that_keeps_dying_is_retried_exactly_once`.
async fn closing_stub(poison_at: u32) -> (String, std::sync::Arc<AtomicU32>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback stub");
    let addr = listener.local_addr().expect("stub addr").to_string();
    let connections = std::sync::Arc::new(AtomicU32::new(0));
    let counter = std::sync::Arc::clone(&connections);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut seen = 0u32;
                let mut buf = [0u8; 1024];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    seen += 1;
                    if seen == poison_at {
                        // The close the client cannot anticipate: the request
                        // is already on the wire, and no response follows it.
                        let _ = stream.shutdown().await;
                        return;
                    }
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]")
                        .await;
                }
            });
        }
    });
    (addr, connections)
}

/// Why (#6038): every `--analyze` render degraded to scan because one closed
/// pooled connection was surfaced as a terminal transport error. The adapter
/// issues three GETs back to back on one keep-alive connection, and the second
/// one landing on a connection the daemon had closed collapsed the whole fetch.
/// What: drives two `get_json` calls against a stub that closes the connection
/// under the second; the first must succeed, and the second must succeed too —
/// on the fresh connection the retry opens.
/// Test: This is the test. Without the retry the second call returns
/// `AnalyzeAdapterError::Transport`.
#[tokio::test]
async fn transport_failure_on_a_reused_connection_retries_once() {
    let (addr, connections) = closing_stub(2).await;
    let src = HttpAnalyzeMetricsSource::new(format!("http://{addr}")).expect("client builds");

    let first: serde_json::Value = src
        .get_json("/indexes", CHEAP_TEST_BUDGET)
        .await
        .expect("the first GET opens a connection and is answered");
    assert_eq!(first, serde_json::json!([]));

    let second: serde_json::Value = src
        .get_json("/indexes/demo/diagnostics", CHEAP_TEST_BUDGET)
        .await
        .expect("a closed pooled connection must be retried on a fresh one");
    assert_eq!(second, serde_json::json!([]));
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "the retry must open exactly one replacement connection"
    );
}

/// Why (#6038): "retry once" is a bound, not a loop. A peer that closes every
/// connection must still fail-open promptly rather than spinning; the report
/// falls back to scan either way and an unbounded retry only delays it.
/// What: points the adapter at a stub that closes every connection on its first
/// request, and asserts the call fails after exactly two connection attempts.
/// Test: This is the test.
#[tokio::test]
async fn a_connection_that_keeps_dying_is_retried_exactly_once() {
    let (addr, connections) = closing_stub(1).await;
    let src = HttpAnalyzeMetricsSource::new(format!("http://{addr}")).expect("client builds");

    let err = src
        .get_json::<serde_json::Value>("/indexes", CHEAP_TEST_BUDGET)
        .await
        .expect_err("a peer that answers nothing cannot succeed");
    assert!(
        matches!(err, AnalyzeAdapterError::Transport(_)),
        "expected a transport error, got {err:?}"
    );
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "one original attempt plus one retry, and no more"
    );
}

// ─── Per-endpoint budgets and independence (#6041) ───────────────────────────

/// Why (#6041): the adapter used one 15 s budget for every endpoint while
/// trusty-analyze runs project-scoped clippy under a 180 s deadline, so the
/// client gave up on a request the daemon answered 200 at 142 s. A client that
/// gives up before the server's outermost rung turns every structured answer —
/// including the daemon's own cutoff report — into a bare transport error.
/// What: walks the configurable deadline range and asserts the client budget
/// outlives the daemon's router rung (deadline + 30 s handler grace + 30 s
/// router margin, from `trusty-analyze/src/core/deadlines.rs`) at every value.
/// Test: This is the test. Fails against the old flat 15 s constant at every
/// deadline.
#[test]
fn diagnostics_budget_outlives_the_daemon_deadline_ladder() {
    for secs in [1u64, 60, 180, 270, 600, 3600] {
        let deadline = Duration::from_secs(secs);
        let router_rung = deadline + Duration::from_secs(60);
        let budget = diagnostics_budget_for(deadline);
        assert!(
            budget > router_rung,
            "deadline={secs}s: client budget {budget:?} must outlive the daemon's \
             outermost responding rung {router_rung:?}, or the daemon's answer \
             never reaches the report"
        );
    }
    assert!(
        AnalyzeEndpoint::Diagnostics.budget() > AnalyzeEndpoint::ComplexityDistribution.budget(),
        "the endpoint that runs external tooling must not share a budget with a \
         memory read"
    );
}

/// Build a raw HTTP/1.1 response with a correct `Content-Length`.
fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A loopback stub that answers each request from a path → response table.
///
/// Why (#6041): the defect is about what happens when endpoints disagree — one
/// answers in milliseconds and another does not answer at all — so the stub has
/// to route per path rather than reply uniformly.
/// What: binds `127.0.0.1:0` and replies with the first route whose path is a
/// substring of the request (so order the table most-specific first). An empty
/// response body is the "never answer" route: the client must hit its own
/// budget. An unmatched path gets a 404.
/// Test: `a_failing_endpoint_keeps_what_the_others_returned`,
/// `a_fetch_where_no_endpoint_answered_falls_back_to_scan`,
/// `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`.
async fn routing_stub(routes: Vec<(&'static str, String)>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback stub");
    let addr = listener.local_addr().expect("stub addr").to_string();
    let routes = std::sync::Arc::new(routes);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let routes = std::sync::Arc::clone(&routes);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let reply = routes
                        .iter()
                        .find(|(path, _)| req.contains(path))
                        .map(|(_, body)| body.clone());
                    match reply {
                        Some(r) if r.is_empty() => std::future::pending::<()>().await,
                        Some(r) => {
                            let _ = stream.write_all(r.as_bytes()).await;
                        }
                        None => {
                            let _ = stream
                                .write_all(http_response("404 Not Found", "{}").as_bytes())
                                .await;
                        }
                    }
                }
            });
        }
    });
    addr
}

/// A five-band histogram, the shape the §7 table renders from.
const DISTRIBUTION_BODY: &str = r#"{"index_id":"demo","total":100,"buckets":[
    {"grade":"A","label":"A: simple (0-5)","count":60},
    {"grade":"B","label":"B: moderate (6-10)","count":20},
    {"grade":"C","label":"C: elevated (11-15)","count":10},
    {"grade":"D","label":"D: high (16-20)","count":7},
    {"grade":"F","label":"F: very high (>20)","count":3}]}"#;

/// Why (#6041): the per-repo fetch was all-or-nothing. Diagnostics is the slow
/// endpoint, so its timeout discarded a complexity histogram that had already
/// arrived in milliseconds and dropped the whole repository back to scan — the
/// report lost data it was holding.
/// What: a stub that answers the index probe, the histogram, and the refactor
/// list normally while returning the daemon's own deadline 504 for diagnostics.
/// Asserts the histogram survives, that exactly one caveat is raised, and that
/// the caveat names diagnostics and says it ran out of time.
/// Test: This is the test. Pre-fix the 504 propagated through `?` and
/// `fetch_named` returned `Missing(Unreachable)` with no metrics at all.
#[tokio::test]
async fn a_failing_endpoint_keeps_what_the_others_returned() {
    let addr = routing_stub(vec![
        (
            "/indexes/demo/complexity_distribution",
            http_response("200 OK", DISTRIBUTION_BODY),
        ),
        (
            "/indexes/demo/diagnostics",
            // What trusty-analyze answers when its own deadline is hit.
            http_response("504 Gateway Timeout", r#"{"error":"deadline exceeded"}"#),
        ),
        (
            "/indexes/demo/refactor-suggestions",
            http_response("200 OK", r#"{"suggestions":[]}"#),
        ),
        (
            "/indexes",
            http_response("200 OK", r#"[{"id":"demo","root_path":"/tmp/demo"}]"#),
        ),
    ])
    .await;
    let src = HttpAnalyzeMetricsSource::new(format!("http://{addr}")).expect("client builds");

    match src.fetch_named("demo").await {
        AnalyzeFetch::Fetched { metrics, caveats } => {
            assert_eq!(
                metrics.complexity.buckets.len(),
                5,
                "the histogram that answered in milliseconds must survive a slow \
                 sibling endpoint"
            );
            let line = caveats
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            assert_eq!(caveats.len(), 1, "only diagnostics dropped out: {line}");
            assert!(line.contains("diagnostics"), "the gap must name it: {line}");
            assert!(
                line.contains("did not answer within the time allowed"),
                "the gap must say why: {line}"
            );
            assert!(
                line.contains("unassessed, not clean"),
                "an emptied defect band must not read as a clean pass: {line}"
            );
        }
        AnalyzeFetch::Missing(gap) => {
            panic!("one slow endpoint must not discard the whole fetch: {gap:?}")
        }
    }
}

/// Why (#6041): per-endpoint independence must not become fail-SILENT. If every
/// dataset drops out there is nothing partial to render, and reporting empty
/// metrics would put an empty §7 and an empty findings table on the page — which
/// reads as a clean pass.
/// What: a stub whose index probe succeeds and whose every dataset endpoint
/// answers 503; the fetch must fall back to scan, not to empty metrics.
/// Test: This is the test.
#[tokio::test]
async fn a_fetch_where_no_endpoint_answered_falls_back_to_scan() {
    let addr = routing_stub(vec![
        (
            "/indexes/demo/",
            http_response("503 Service Unavailable", "{}"),
        ),
        ("/indexes", http_response("200 OK", r#"[{"id":"demo"}]"#)),
    ])
    .await;
    let src = HttpAnalyzeMetricsSource::new(format!("http://{addr}")).expect("client builds");

    match src.fetch_named("demo").await {
        AnalyzeFetch::Missing(gap) => assert_eq!(gap, AnalyzeGap::Unreachable),
        AnalyzeFetch::Fetched { .. } => {
            panic!("a fetch that assessed nothing must not render as assessed")
        }
    }
}

/// Why (#6041): the gap line distinguishes "ran out of time" from "nothing was
/// listening" because the remedies differ, and that distinction has to come from
/// the error class rather than from matching a message that can quote a URL.
/// What: points `get_json` at a stub that never replies, with a budget short
/// enough to be hit, and asserts the failure is a `Timeout` classified as
/// `TimedOut` — not the `Transport` variant a dead peer produces.
/// Test: This is the test. Pre-fix `get_json` took no budget at all.
#[tokio::test]
async fn a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error() {
    let addr = routing_stub(vec![("/indexes", String::new())]).await;
    let src = HttpAnalyzeMetricsSource::new(format!("http://{addr}")).expect("client builds");

    let err = src
        .get_json::<serde_json::Value>("/indexes", Duration::from_millis(150))
        .await
        .expect_err("a stub that never replies cannot answer inside 150ms");

    assert!(
        matches!(err, AnalyzeAdapterError::Timeout(_)),
        "expected a timeout, got {err:?}"
    );
    assert_eq!(classify_failure(&err), EndpointFailure::TimedOut);
}

#[test]
fn new_trims_trailing_slash() {
    let src = HttpAnalyzeMetricsSource::new("http://127.0.0.1:7879/").unwrap();
    assert_eq!(src.base_url, "http://127.0.0.1:7879");
}

/// #6149: the renderer and the audit are separate processes agreeing on one id.
/// Two checkouts of one repository must not derive the same one — that is the
/// collision that had this crate reading another tree's measurements.
#[test]
fn derive_index_id_distinguishes_same_named_checkouts() {
    let engagement = std::path::Path::new("/w/dogfood/repos/local/northwind-web");
    let working = std::path::Path::new("/home/me/northwind-web");

    let a = derive_index_id(engagement).expect("id");
    let b = derive_index_id(working).expect("id");
    assert_ne!(a, b, "{a} vs {b}");
    assert!(b.starts_with("northwind-web-"), "still readable: {b}");
    assert_eq!(derive_index_id(std::path::Path::new("/")), None);
}

/// The agreement is a call, not a copy: this crate's id IS trusty-common's, so
/// the audit that indexed under it and this renderer cannot drift.
#[test]
fn derive_index_id_is_the_shared_derivation() {
    for path in ["/home/me/northwind-web", "/w/repos/acme-api", "/"] {
        let path = std::path::Path::new(path);
        assert_eq!(
            derive_index_id(path),
            trusty_common::derive_checkout_index_id(path),
            "{}",
            path.display()
        );
    }
}

#[test]
fn error_display() {
    let e = AnalyzeAdapterError::Api {
        status: 503,
        body: "down".into(),
    };
    assert!(e.to_string().contains("503"));
}

// ─── Named gaps (#5239) ──────────────────────────────────────────────────────

/// A source that answers with a fixed outcome, so the enrichment walk can be
/// driven without a daemon.
struct StubSource(fn() -> AnalyzeFetch);

#[async_trait::async_trait]
impl AnalyzeMetricsSource for StubSource {
    async fn fetch(&self, _index_id: &str) -> Option<AnalyzeMetrics> {
        match (self.0)() {
            AnalyzeFetch::Fetched { metrics, .. } => Some(*metrics),
            AnalyzeFetch::Missing(_) => None,
        }
    }

    async fn fetch_named(&self, _index_id: &str) -> AnalyzeFetch {
        (self.0)()
    }
}

/// A source that implements ONLY `fetch`, exercising the trait's default
/// `fetch_named` — the shape an out-of-crate implementor keeps compiling with.
struct MinimalSource;

#[async_trait::async_trait]
impl AnalyzeMetricsSource for MinimalSource {
    async fn fetch(&self, _index_id: &str) -> Option<AnalyzeMetrics> {
        None
    }
}

/// Build a one-repository model whose single entry is an unpopulated local
/// checkout — the only shape the enrichment walk acts on.
fn model_with_local_repo(name: &str) -> crate::report::model::ReportModel {
    let manifest = crate::report::manifest::parse_manifest(
        &format!("[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"{name}\"\npath = \".\"\n"),
        std::path::Path::new("m.toml"),
    )
    .expect("fixture manifest parses");
    let mut model = crate::report::model::ReportModel::build(
        &manifest,
        std::path::Path::new("m.toml"),
        "report-technical-dd",
        None,
    )
    .expect("model builds");
    // `.` always resolves to a directory, so `local_path` is populated; pin it
    // to a stable name so the derived index id does not depend on the CWD.
    model.repositories[0].local_path = Some(std::path::PathBuf::from("/tmp/northwind-web"));
    model
}

/// Why: the gap phrasing reaches a third party's desk; it must be fixed prose,
/// never a variant name and never run-specific detail.
/// Test: itself.
#[test]
fn gap_labels_are_stable() {
    assert_eq!(
        AnalyzeGap::NotIndexed.as_str(),
        "trusty-analyze index not built"
    );
    assert_eq!(
        AnalyzeGap::Unreachable.as_str(),
        "trusty-analyze unreachable"
    );
    assert_eq!(
        AnalyzeGap::Unavailable.as_str(),
        "trusty-analyze data unavailable"
    );
}

/// Why (#5317, #5320): the caveat phrasing reaches a third party's desk on the
/// same page as the gap lines; it must be fixed prose and must say what the
/// incompleteness means for the section it affects.
/// Test: itself.
#[test]
fn caveat_labels_are_stable() {
    let dist = AnalyzeCaveat::EndpointUnavailable(
        AnalyzeEndpoint::ComplexityDistribution,
        EndpointFailure::Rejected,
    )
    .to_string();
    assert!(dist.contains("complexity distribution"), "{dist}");
    assert!(dist.contains("not a distribution"), "{dist}");
    assert!(dist.contains("omitted"), "{dist}");

    // #6041: the line must name the endpoint AND why it dropped out, because
    // "raise the deadline" and "start the daemon" are different remedies.
    let diag =
        AnalyzeCaveat::EndpointUnavailable(AnalyzeEndpoint::Diagnostics, EndpointFailure::TimedOut)
            .to_string();
    assert!(diag.contains("diagnostics"), "{diag}");
    assert!(
        diag.contains("did not answer within the time allowed"),
        "{diag}"
    );
    assert!(diag.contains("unassessed, not clean"), "{diag}");

    let tools = AnalyzeCaveat::NoStaticAnalysisTools.to_string();
    assert!(tools.contains("unassessed, not clean"), "{tools}");
}

/// Why (#5320): a fetch that returns metrics is not a fetch that answered
/// everything. When the daemon serves no full-corpus histogram the §7 table is
/// left out — and a table that is simply absent reads as a rendering slip
/// unless the report says why.
/// Test: itself.
#[tokio::test]
async fn enrich_reports_caveats_for_partially_answered_repositories() {
    let mut model = model_with_local_repo("Northwind Web");
    let source = StubSource(|| AnalyzeFetch::Fetched {
        metrics: Box::new(map_metrics(None, &[], &[])),
        caveats: vec![
            AnalyzeCaveat::EndpointUnavailable(
                AnalyzeEndpoint::ComplexityDistribution,
                EndpointFailure::Rejected,
            ),
            AnalyzeCaveat::NoStaticAnalysisTools,
        ],
    });

    let gaps = enrich_with_analyze_gaps(&mut model, &source).await;

    assert_eq!(gaps.len(), 2, "one line per caveat kind: {gaps:?}");
    assert!(gaps.iter().all(|g| g.contains("Northwind Web")), "{gaps:?}");
    assert!(
        gaps.iter().any(|g| g.contains("not a distribution")),
        "the truncated-distribution caveat must be stated: {gaps:?}"
    );
    assert!(
        gaps.iter().any(|g| g.contains("unassessed, not clean")),
        "an empty RED band must not read as a clean pass: {gaps:?}"
    );
    assert!(
        model.repositories[0].metrics.is_some(),
        "a caveat does not discard the metrics that did arrive"
    );
}

/// Why: the trait's default `fetch_named` is what keeps every existing
/// implementor compiling; it must still produce a NAMED outcome rather than
/// silently dropping the fact that nothing was fetched.
/// Test: itself.
#[tokio::test]
async fn default_fetch_named_reports_unavailable() {
    match MinimalSource.fetch_named("demo").await {
        AnalyzeFetch::Missing(gap) => assert_eq!(gap, AnalyzeGap::Unavailable),
        AnalyzeFetch::Fetched { .. } => panic!("MinimalSource never fetches"),
    }
}

/// Why: #5239's core claim — a repository the daemon could not serve is named
/// in the report, and the line says "not assessed", not nothing at all.
/// Test: itself.
#[tokio::test]
async fn enrich_names_unreachable_repositories() {
    let mut model = model_with_local_repo("Northwind Web");
    let source = StubSource(|| AnalyzeFetch::Missing(AnalyzeGap::Unreachable));

    let gaps = enrich_with_analyze_gaps(&mut model, &source).await;

    assert_eq!(gaps.len(), 1, "one line per gap kind: {gaps:?}");
    assert!(
        gaps[0].starts_with("trusty-analyze unreachable"),
        "{}",
        gaps[0]
    );
    assert!(gaps[0].contains("Northwind Web"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("not assessed, not clean"),
        "the line must refuse to read as a clean pass: {}",
        gaps[0]
    );
    assert!(model.repositories[0].metrics.is_none());
}

/// Why: the fail-open contract is unchanged — a populated repo yields metrics
/// and NO gap line, so a clean report stays clean.
/// Test: itself.
#[tokio::test]
async fn enrich_reports_no_gaps_when_every_repo_is_populated() {
    let mut model = model_with_local_repo("Northwind Web");
    let source = StubSource(|| AnalyzeFetch::Fetched {
        metrics: Box::new(map_metrics(None, &[], &[])),
        caveats: Vec::new(),
    });

    let gaps = enrich_with_analyze_gaps(&mut model, &source).await;

    assert!(gaps.is_empty(), "populated repo is not a gap: {gaps:?}");
    assert!(model.repositories[0].metrics.is_some());
}

/// Why: a remote entry was never eligible for a local index, so calling it an
/// unassessed gap would be a false alarm in every report with a remote repo.
/// Test: itself.
#[tokio::test]
async fn enrich_ignores_repositories_with_no_local_checkout() {
    let mut model = model_with_local_repo("Northwind Web");
    model.repositories[0].local_path = None;
    let source = StubSource(|| AnalyzeFetch::Missing(AnalyzeGap::Unreachable));

    assert!(
        enrich_with_analyze_gaps(&mut model, &source)
            .await
            .is_empty()
    );
}
