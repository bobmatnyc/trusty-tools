//! Unit tests for the trusty-analyze → `AnalyzeMetrics` adapter (#2447).
//!
//! Why: the mapping must be provable against fixture JSON with NO live daemon —
//! these tests pin the envelope parsing, the severity convention, the
//! complexity-bucket thresholds, and the fail-open behaviour.
//! What: drives the private mapping helpers and the public fetch seam directly.
//! Test: this file.

use super::*;

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

/// A socket path nothing has ever bound.
///
/// #6287: the pre-migration equivalent was `http://127.0.0.1:1` — a port
/// guaranteed refused. A path inside a fresh `TempDir` is the same guarantee on
/// the socket transport, and the `TempDir` is returned so the caller keeps it
/// alive for the duration of the call.
fn dead_socket() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("absent-analyze.sock");
    (tmp, path)
}

#[tokio::test]
async fn fetch_returns_none_on_unreachable_daemon() {
    let (_tmp, socket) = dead_socket();
    let src = HttpAnalyzeMetricsSource::new(socket).unwrap();
    assert!(src.fetch("demo").await.is_none());
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

/// A JSON-RPC result frame carrying `body`, which must itself be valid JSON.
///
/// #6287: the predecessor built an HTTP/1.1 response with a `Content-Length`.
/// A frame is newline-terminated, so `body` is re-serialised compactly first —
/// a pretty-printed fixture embedded verbatim would end the frame at its first
/// line break.
fn rpc_result(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("a valid fixture");
    format!(r#"{{"jsonrpc":"2.0","id":1,"result":{value}}}"#)
}

/// A JSON-RPC error frame carrying `code` and `message`.
fn rpc_error(code: i64, message: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":{code},"message":"{message}"}}}}"#)
}

/// A Unix-socket stub that answers each request from a method → response table.
///
/// Why (#6041): the defect is about what happens when endpoints disagree — one
/// answers in milliseconds and another does not answer at all — so the stub has
/// to route per method rather than reply uniformly.
/// What: binds a socket under a fresh `TempDir` and replies with the first route
/// whose method name appears in the request frame (so order the table
/// most-specific first). An empty response body is the "never answer" route: the
/// client must hit its own budget. An unmatched method gets `method_not_found`.
/// The `TempDir` is returned because dropping it unlinks the socket.
/// Test: `a_failing_endpoint_keeps_what_the_others_returned`,
/// `a_fetch_where_no_endpoint_answered_falls_back_to_scan`,
/// `a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error`,
/// `a_daemon_side_deadline_is_a_timeout_not_a_rejection`.
async fn routing_stub(routes: Vec<(&'static str, String)>) -> (tempfile::TempDir, PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = tmp.path().join("analyze.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the stub socket");
    let routes = std::sync::Arc::new(routes);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let routes = std::sync::Arc::clone(&routes);
            tokio::spawn(async move {
                // The client writes one frame then half-closes, so reading to
                // EOF is the whole request.
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let request = String::from_utf8_lossy(&sink).into_owned();
                let reply = routes
                    .iter()
                    .find(|(method, _)| request.contains(method))
                    .map(|(_, body)| body.clone());
                match reply {
                    Some(r) if r.is_empty() => std::future::pending::<()>().await,
                    Some(r) => {
                        let _ = conn.write_all(r.as_bytes()).await;
                        let _ = conn.write_all(b"\n").await;
                        let _ = conn.flush().await;
                    }
                    None => {
                        let _ = conn
                            .write_all(rpc_error(-32601, "no such method").as_bytes())
                            .await;
                        let _ = conn.write_all(b"\n").await;
                        let _ = conn.flush().await;
                    }
                }
            });
        }
    });
    (tmp, socket)
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
/// list normally while returning the daemon's own deadline error for
/// diagnostics. Asserts the histogram survives, that exactly one caveat is
/// raised, and that the caveat names diagnostics and says it ran out of time.
/// Test: This is the test. Pre-fix the deadline error propagated through `?` and
/// `fetch_named` returned `Missing(Unreachable)` with no metrics at all.
#[tokio::test]
async fn a_failing_endpoint_keeps_what_the_others_returned() {
    let (_tmp, socket) = routing_stub(vec![
        (
            "analyze.complexity_distribution",
            rpc_result(DISTRIBUTION_BODY),
        ),
        (
            "analyze.diagnostics",
            // What trusty-analyze answers when its own deadline is hit.
            rpc_error(-32005, "deadline exceeded"),
        ),
        (
            "analyze.refactor_suggestions",
            rpc_result(r#"{"suggestions":[]}"#),
        ),
        (
            "analyze.list_indexes",
            rpc_result(r#"[{"id":"demo","root_path":"/tmp/demo"}]"#),
        ),
    ])
    .await;
    let src = HttpAnalyzeMetricsSource::new(socket).expect("client builds");

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
/// What: a stub whose index probe succeeds and whose every dataset method
/// answers `internal_error`; the fetch must fall back to scan, not to empty
/// metrics. The routes are ordered so `analyze.list_indexes` is matched before
/// the catch-all rows that refuse the three datasets.
/// Test: This is the test.
#[tokio::test]
async fn a_fetch_where_no_endpoint_answered_falls_back_to_scan() {
    let (_tmp, socket) = routing_stub(vec![
        ("analyze.list_indexes", rpc_result(r#"[{"id":"demo"}]"#)),
        (
            "analyze.complexity_distribution",
            rpc_error(-32603, "index is not loaded"),
        ),
        (
            "analyze.diagnostics",
            rpc_error(-32603, "index is not loaded"),
        ),
        (
            "analyze.refactor_suggestions",
            rpc_error(-32603, "index is not loaded"),
        ),
    ])
    .await;
    let src = HttpAnalyzeMetricsSource::new(socket).expect("client builds");

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
/// What: points `call` at a stub that never replies, with a budget short enough
/// to be hit, and asserts the failure is a `Timeout` classified as `TimedOut` —
/// not the `Transport` variant a dead peer produces.
/// Test: This is the test. Pre-fix the call took no budget at all.
#[tokio::test]
async fn a_request_that_outlives_its_budget_is_a_timeout_not_a_transport_error() {
    let (_tmp, socket) = routing_stub(vec![("analyze.list_indexes", String::new())]).await;
    let src = HttpAnalyzeMetricsSource::new(socket).expect("client builds");

    let err = src
        .call::<serde_json::Value>(
            AnalyzeEndpoint::IndexList,
            "demo",
            Duration::from_millis(150),
        )
        .await
        .expect_err("a stub that never replies cannot answer inside 150ms");

    assert!(
        matches!(err, AnalyzeAdapterError::Timeout(_)),
        "expected a timeout, got {err:?}"
    );
    assert_eq!(classify_failure(&err), EndpointFailure::TimedOut);
}

/// Why (#6287): the client-side and daemon-side deadlines are the same fact to
/// a report reader — the analysis ran out of time — and a different fact from a
/// daemon that answered with something unusable. Under HTTP that split was
/// 504-versus-4xx; on the socket it is `CODE_DEADLINE_EXCEEDED` versus every
/// other JSON-RPC code, and reading the CODE is what keeps
/// [`classify_failure`]'s own doc honest: a message can quote a path and can be
/// reworded, a code cannot.
/// What: a stub that answers the diagnostics call with the daemon's own
/// `-32005` frame, and asserts the adapter classifies it `TimedOut` rather than
/// `Rejected` — the bucket every other `Rpc` code lands in.
/// Test: This is the test. Against a `classify_failure` with no `-32005` arm the
/// assertion reads `Rejected`, which is the sentence the report would print.
#[tokio::test]
async fn a_daemon_side_deadline_is_a_timeout_not_a_rejection() {
    let (_tmp, socket) = routing_stub(vec![(
        "analyze.diagnostics",
        rpc_error(CODE_DEADLINE_EXCEEDED, "deadline exceeded over 900 files"),
    )])
    .await;
    let src = HttpAnalyzeMetricsSource::new(socket).expect("client builds");

    let err = src
        .call::<serde_json::Value>(AnalyzeEndpoint::Diagnostics, "demo", Duration::from_secs(5))
        .await
        .expect_err("an error frame is never a result");

    let AnalyzeAdapterError::Rpc { code, .. } = &err else {
        panic!("a JSON-RPC error frame must arrive as Rpc, got {err:?}");
    };
    assert_eq!(
        *code, -32005,
        "the code this crate copies must be the one trusty-analyze sends"
    );
    assert_eq!(
        classify_failure(&err),
        EndpointFailure::TimedOut,
        "a daemon that ran out of time points at a deadline to raise, not at a \
         daemon to start"
    );

    // The mirror, and what keeps the assertion above from passing vacuously:
    // every OTHER code is still a rejection.
    let internal = AnalyzeAdapterError::Rpc {
        code: -32603,
        message: "index is not loaded".into(),
    };
    assert_eq!(classify_failure(&internal), EndpointFailure::Rejected);
}

/// #6287: the constructor took a base URL it had to normalise; it takes a
/// socket path, which has no trailing-slash form to trim and no client to fail
/// building. Its predecessor here was `new_trims_trailing_slash`.
#[test]
fn new_accepts_a_socket_path() {
    let (_tmp, socket) = dead_socket();
    let src = HttpAnalyzeMetricsSource::new(&socket).expect("infallible since #6287");
    assert_eq!(src.socket, socket);
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
    let e = AnalyzeAdapterError::Rpc {
        code: -32603,
        message: "index is not loaded".into(),
    };
    let shown = e.to_string();
    assert!(shown.contains("-32603"), "{shown}");
    assert!(shown.contains("index is not loaded"), "{shown}");
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

// ── #6177: the Python class-body region ─────────────────────────────────────

/// A Python class body the daemon named as one.
fn python_class_body(function_name: &str) -> WireRefactor {
    let name = if function_name.is_empty() {
        "null".to_string()
    } else {
        format!("\"{function_name}\"")
    };
    serde_json::from_str(&format!(
        r#"{{ "file": "app/models.py", "function_name": {name},
             "region_kind": "class_body", "refactor_type": "extract_method",
             "severity": "critical",
             "rationale": "cyclomatic complexity 118 (grade F); smells: long_function(603 lines)",
             "suggested_action": "Extract the body of 'this function' (lines 12-615) into 2-3 smaller functions" }}"#
    ))
    .expect("wire refactor")
}

/// The Python half of the #6082 impl-block relabel. Extracting the body of a
/// class is not an action a reader can take; moving its members out is.
#[test]
fn a_python_class_body_is_labelled_a_class_body() {
    let f = refactor_finding(&python_class_body("")).expect("critical → amber");

    assert_eq!(f.title, "Split oversized class body");
    assert!(f.remediation.contains("Split the class body"), "{f:?}");
    assert!(f.remediation.contains("(lines 12–615)"), "{f:?}");
    assert!(
        f.description.contains("long_class_body(603 lines)"),
        "{f:?}"
    );
    assert!(!f.description.contains("long_function"), "{f:?}");
    for field in [&f.title, &f.description, &f.remediation] {
        assert!(!field.contains("this function"), "{field}");
    }
}

/// A class body CAN carry a name, unlike the Rust impl-block case, so the
/// daemon's answer about the region has to outrank the name-absent inference —
/// a named class body would otherwise render as "Extract method — Order".
#[test]
fn a_named_python_class_body_is_named_in_its_remediation() {
    let f = refactor_finding(&python_class_body("Order")).expect("critical → amber");
    assert_eq!(f.title, "Split oversized class body");
    assert!(f.remediation.contains("`Order`"), "{f:?}");
}

/// With neither a line range nor a rationale nothing distinguishes the region,
/// so it is suppressed rather than mislabelled — the same rule
/// `impl_block_finding` follows.
#[test]
fn an_unidentifiable_class_body_is_suppressed() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "app/models.py", "region_kind": "class_body",
             "refactor_type": "extract_method", "severity": "critical",
             "rationale": "", "suggested_action": "Extract the body of 'this function'" }"#,
    )
    .expect("wire refactor");
    assert!(refactor_finding(&r).is_none());
}

/// A daemon predating the field, or any non-Python language, renders exactly as
/// it did before #6177.
#[test]
fn an_absent_region_kind_renders_as_before() {
    let f = refactor_finding(&nameless_region()).expect("critical → amber");
    assert_eq!(f.title, "Split oversized impl block");
}
