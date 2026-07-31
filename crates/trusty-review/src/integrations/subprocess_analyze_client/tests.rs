//! Unit tests for the subprocess analyze client.
//!
//! Why: isolated here to keep client.rs and mod.rs under the 500-line cap
//! while preserving full test coverage.
//! What: exercises `SubprocessAnalyzeClient` construction, mapping logic,
//! async health probes, and the synchronous `spawn_analyze_review` helper.
//! Test: all tests in this file are self-contained; async tests use tokio.

use crate::integrations::analyze_client::AnalyzeClient;
use crate::integrations::analyze_client::AnalyzeClientError;

use super::client::{SubprocessAnalyzeClient, spawn_analyze_review};
use super::{
    SubprocessComplexity, SubprocessFileReview, SubprocessReviewReport, SubprocessSmellHit,
    map_report,
};

#[test]
fn subprocess_client_binary_accessor() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878")
        .expect("TLS init should succeed");
    assert_eq!(client.binary(), "trusty-analyze");
}

/// Verify health() returns Unavailable (not a panic) when trusty-search is down.
#[tokio::test]
async fn subprocess_client_health_check_fails_gracefully() {
    // Port 1 is always refused.
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:1")
        .expect("TLS init should succeed");
    let result = client.health().await;
    assert!(
        result.is_err(),
        "health must return Err when trusty-search is down"
    );
    assert!(
        matches!(result.unwrap_err(), AnalyzeClientError::Unavailable(_)),
        "expected Unavailable variant"
    );
}

/// has_analysis must return false (not panic) on transport error.
#[tokio::test]
async fn subprocess_client_has_analysis_returns_false_on_error() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:1")
        .expect("TLS init should succeed");
    assert!(
        !client.has_analysis("main").await,
        "has_analysis must return false on error"
    );
}

/// complexity_hotspots always returns empty for the subprocess model.
#[tokio::test]
async fn subprocess_client_hotspots_returns_empty() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878")
        .expect("TLS init should succeed");
    let result = client.complexity_hotspots("main", Some(10)).await.unwrap();
    assert!(
        result.is_empty(),
        "subprocess model always returns empty hotspots"
    );
}

/// smells always returns empty for the subprocess model.
#[tokio::test]
async fn subprocess_client_smells_returns_empty() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878")
        .expect("TLS init should succeed");
    let result = client.smells("main").await.unwrap();
    assert!(
        result.is_empty(),
        "subprocess model always returns empty smells"
    );
}

/// Verify the binary-not-found path gives an informative error.
#[tokio::test]
async fn subprocess_client_binary_not_found() {
    // "trusty-analyze-nonexistent-binary" is guaranteed not to be on PATH.
    let client =
        SubprocessAnalyzeClient::new("trusty-analyze-nonexistent-binary", "http://127.0.0.1:1")
            .expect("TLS init should succeed");
    // health() probes search first; search is down so it short-circuits with
    // Unavailable before reaching the binary check.  We test analyze_diff
    // directly for the binary-not-found path.
    let result = client
        .analyze_diff("+++ b/foo.rs\n@@ -0,0 +1,1 @@\n+fn f(){}\n", "idx")
        .await;
    assert!(result.is_err(), "missing binary must error");
    assert!(
        matches!(result.unwrap_err(), AnalyzeClientError::Unavailable(_)),
        "expected Unavailable for missing binary"
    );
}

// ─── Degraded-but-serving health (#4440) ───────────────────────────────────────

/// The VERBATIM `/health` payload from the live trusty-search 0.39.1 daemon that
/// permanently blocked the review gate in #4440.
///
/// Why: `status` is `"degraded"` solely because four UNRELATED indexes timed out
/// at warm boot. trusty-search derives that flag from boot-time counters that are
/// never decremented, so it stays `"degraded"` for the daemon's whole process
/// lifetime no matter how healthy it becomes. The daemon is embedder-ready,
/// 12 indexes loaded, zero failures, and answering queries normally.
/// What: a `const` so the fixture is byte-identical to what the real daemon sent.
/// Test: used by `subprocess_client_degraded_search_still_has_analysis` and
/// `subprocess_client_health_preserves_degraded_status_string`.
const LIVE_DEGRADED_HEALTH: &str = r#"{"status":"degraded","version":"0.39.1","indexes":7,
    "uptime_secs":18279,"embedder":"ready","embedder_last_ok_secs_ago":11909,
    "embedder_recent_timeout_count":0,"rss_mb":247,
    "embedder_info":{"dimension":384,"provider":"MPS","quantized":false,
        "model":"all-MiniLM-L6-v2","backend":"python"},
    "warmboot_summary":{"indexes_loaded":12,"indexes_skipped_tcc":0,
        "indexes_skipped_timeout":4,"warm_boot_degraded":true,"indexes_lazy":0,
        "indexes_failed":0,"indexes_corpus_failed":0,"indexes_health_scan_skipped":0},
    "embedder_bootstrap":"ready"}"#;

/// A daemon that is genuinely NOT serving: `status` is neither `"ok"` nor
/// `"degraded"`, so no query can succeed against it.
///
/// Why: the #4440 fix must narrow the false-positive WITHOUT turning the gate
/// into an always-pass. This is the control payload that must still fail.
/// What: `status: "starting"` with a ready embedder and a spotless warm boot —
/// so the ONLY thing distinguishing it from the passing case is the status.
/// Test: `subprocess_client_not_serving_search_has_no_analysis`.
const NOT_SERVING_HEALTH: &str = r#"{"status":"starting","version":"0.39.1","embedder":"ready",
    "warmboot_summary":{"indexes_loaded":12,"indexes_skipped_tcc":0,
        "indexes_skipped_timeout":0,"warm_boot_degraded":false,
        "indexes_failed":0,"indexes_corpus_failed":0}}"#;

/// Bind a one-shot stub HTTP server that answers the next request with `body`.
///
/// Why: the #4440 regression is entirely about how a specific `/health` JSON
/// document is interpreted, so the test must drive the REAL `health()` code path
/// — HTTP fetch, deserialise, classify — rather than assert on a hand-built
/// struct. The crate has no mock-server dev-dependency, and this mirrors the
/// raw-`TcpListener` stub already used by `llm/openrouter_tests.rs`.
/// What: binds an ephemeral loopback port, spawns a task that accepts exactly one
/// connection, drains the request, and writes `body` as a 200 JSON response.
/// Returns the base URL plus the task handle so the test can join it.
/// Test: used by the three `#4440` tests below.
async fn stub_health_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral loopback port");
    let addr = listener.local_addr().expect("stub server local_addr");
    let handle = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), handle)
}

/// REGRESSION (#4440): a `"degraded"`-but-serving trusty-search must NOT block
/// the review gate.
///
/// Why: `health()` tested `status == "ok"` as a literal string, so this exact
/// payload made `has_analysis` return `false` — permanently, because
/// trusty-search never clears the flag within a process lifetime — and
/// `context_gate` skipped EVERY review with "trusty-analyze unreachable/not-ready"
/// against a daemon that was up and answering queries. This is the assertion that
/// would have caught it.
/// What: serves the verbatim live payload and asserts `has_analysis()` is `true`.
/// The binary is `echo` (always present, exits 0 on `--version`) so the probe's
/// binary half is satisfied and the payload is the only variable under test.
/// Test: this test.
#[tokio::test]
async fn subprocess_client_degraded_search_still_has_analysis() {
    let (base_url, server) = stub_health_server(LIVE_DEGRADED_HEALTH).await;
    let client = SubprocessAnalyzeClient::new("echo", base_url).expect("TLS init should succeed");

    assert!(
        client.has_analysis("any-index").await,
        "a trusty-search reporting status=\"degraded\" purely because 4 UNRELATED indexes timed \
         out at warm boot is still SERVING — it must not block the review gate (#4440)"
    );

    server.await.expect("stub server task must not panic");
}

/// The degraded status string must survive the probe unaltered.
///
/// Why: the fix must not "solve" the block by laundering `"degraded"` into
/// `"ok"`. Anything that displays the analyze health must still see what
/// trusty-search actually said; only the PASS/FAIL decision changed.
/// What: asserts `status` is still `"degraded"` while `search_reachable` is
/// `true` — the two facts that used to be forced to agree.
/// Test: this test.
#[tokio::test]
async fn subprocess_client_health_preserves_degraded_status_string() {
    let (base_url, server) = stub_health_server(LIVE_DEGRADED_HEALTH).await;
    let client = SubprocessAnalyzeClient::new("echo", base_url).expect("TLS init should succeed");

    let health = client
        .health()
        .await
        .expect("degraded-but-serving must not error");
    assert_eq!(
        health.status, "degraded",
        "trusty-search's own status string must pass through verbatim, never be rewritten to \"ok\""
    );
    assert!(
        health.search_reachable,
        "search_reachable must reflect is_serving(), not status == \"ok\" (#4440)"
    );

    server.await.expect("stub server task must not panic");
}

/// NEGATIVE (#4440): a trusty-search that genuinely is not serving must STILL
/// fail the gate.
///
/// Why: the whole risk of this fix is over-correcting into an always-pass gate
/// that lets reviews run with no static-analysis context at all. This payload
/// differs from the passing one ONLY in its status (`"starting"`, with a ready
/// embedder and a clean warm boot), so a `false` here proves the probe still
/// discriminates rather than having been blanket-disabled.
/// What: asserts `has_analysis()` is `false` and `health()` reports `Unavailable`.
/// Test: this test.
#[tokio::test]
async fn subprocess_client_not_serving_search_has_no_analysis() {
    let (base_url, server) = stub_health_server(NOT_SERVING_HEALTH).await;
    let client = SubprocessAnalyzeClient::new("echo", base_url).expect("TLS init should succeed");

    assert!(
        !client.has_analysis("any-index").await,
        "a trusty-search that is not serving must still block the gate — the #4440 fix narrows \
         the false-positive, it must not weaken the gate into always-pass"
    );

    server.await.expect("stub server task must not panic");
}

/// A not-serving daemon must surface as `Unavailable`, not `Parse` or `Ok`.
///
/// Why: `context_gate` and `probe_deps` branch on the error variant; a wrong
/// variant would misreport a live-but-broken daemon.
/// What: asserts the `NOT_SERVING_HEALTH` payload yields
/// `AnalyzeClientError::Unavailable` whose text names the real reason.
/// Test: this test.
#[tokio::test]
async fn subprocess_client_not_serving_search_reports_unavailable() {
    let (base_url, server) = stub_health_server(NOT_SERVING_HEALTH).await;
    let client = SubprocessAnalyzeClient::new("echo", base_url).expect("TLS init should succeed");

    let err = client
        .health()
        .await
        .expect_err("a not-serving trusty-search must error");
    match err {
        AnalyzeClientError::Unavailable(msg) => assert!(
            msg.contains("not serving"),
            "message must name the real reason; got: {msg}"
        ),
        other => panic!("expected Unavailable, got {other:?}"),
    }

    server.await.expect("stub server task must not panic");
}

// ─── Map report tests ──────────────────────────────────────────────────────────

/// Core mapping logic: a `ReviewReport`-shaped JSON is correctly projected
/// onto hotspots and smells.
#[test]
fn map_report_to_hotspots_and_smells() {
    let report = SubprocessReviewReport {
        files: vec![
            SubprocessFileReview {
                path: "src/foo.rs".to_string(),
                complexity: SubprocessComplexity {
                    cyclomatic: 12,
                    cognitive: 8,
                },
                smells: vec![
                    SubprocessSmellHit {
                        category: "long_method".to_string(),
                        line: 42,
                        severity: "medium".to_string(),
                    },
                    SubprocessSmellHit {
                        category: "deep_nesting".to_string(),
                        line: 55,
                        severity: "high".to_string(),
                    },
                ],
            },
            SubprocessFileReview {
                path: "src/bar.rs".to_string(),
                complexity: SubprocessComplexity {
                    cyclomatic: 3,
                    cognitive: 2,
                },
                smells: vec![],
            },
        ],
    };

    let (hotspots, smells) = map_report(&report);

    // Two files → two hotspots (both have non-zero complexity).
    assert_eq!(hotspots.len(), 2);
    assert_eq!(hotspots[0].file, "src/foo.rs");
    assert_eq!(hotspots[0].cyclomatic, 12);
    assert_eq!(hotspots[0].cognitive, 8);
    assert_eq!(hotspots[1].file, "src/bar.rs");
    assert_eq!(hotspots[1].cyclomatic, 3);

    // Two smells from foo.rs, none from bar.rs.
    assert_eq!(smells.len(), 2);
    assert_eq!(smells[0].file, "src/foo.rs");
    assert_eq!(smells[0].category, "long_method");
    assert_eq!(smells[0].line, Some(42));
    assert_eq!(smells[0].severity, "medium");
    assert_eq!(smells[1].category, "deep_nesting");
    assert_eq!(smells[1].line, Some(55));
    assert_eq!(smells[1].severity, "high");
}

/// Files with zero complexity are not emitted as hotspots.
#[test]
fn map_report_skips_zero_complexity_hotspots() {
    let report = SubprocessReviewReport {
        files: vec![SubprocessFileReview {
            path: "src/trivial.rs".to_string(),
            complexity: SubprocessComplexity {
                cyclomatic: 0,
                cognitive: 0,
            },
            smells: vec![],
        }],
    };
    let (hotspots, smells) = map_report(&report);
    assert!(hotspots.is_empty(), "zero-complexity files emit no hotspot");
    assert!(smells.is_empty());
}

/// An empty `ReviewReport` (empty diff) maps to empty vecs.
#[test]
fn map_empty_report() {
    let report = SubprocessReviewReport { files: vec![] };
    let (hotspots, smells) = map_report(&report);
    assert!(hotspots.is_empty());
    assert!(smells.is_empty());
}

/// Round-trip: JSON matching the trusty-analyze wire format deserialises correctly.
#[test]
fn subprocess_review_report_deserialises_from_wire_json() {
    let json = r#"{
        "files": [
            {
                "path": "src/main.rs",
                "grade": "B",
                "complexity": { "cyclomatic": 7, "cognitive": 4 },
                "smells": [
                    { "category": "too_many_params", "line": 10, "severity": "medium" }
                ],
                "recommendations": [],
                "source": { "kind": "indexed", "modified_chunks": 2 }
            }
        ],
        "overall_grade": "B",
        "changed_lines": 20,
        "smell_count": 1,
        "summary": "1 file analyzed (1 indexed, 0 new); 1 smell found; overall grade B"
    }"#;

    let report: SubprocessReviewReport = serde_json::from_str(json).unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, "src/main.rs");
    assert_eq!(report.files[0].complexity.cyclomatic, 7);
    assert_eq!(report.files[0].smells.len(), 1);
    assert_eq!(report.files[0].smells[0].category, "too_many_params");
}

/// Subprocess error exit (exit code 1 = search down) surfaces as Unavailable.
#[test]
fn spawn_analyze_review_with_fake_binary_that_fails() {
    // Use `false` (always exits 1) or `sh -c "exit 1"` as a fake binary.
    // On all POSIX systems, `false` is a valid binary that exits 1.
    let result = spawn_analyze_review("false", "main", "+++ b/x.rs\n");
    assert!(result.is_err(), "exit-1 binary must return Err");
    assert!(
        matches!(result.unwrap_err(), AnalyzeClientError::Unavailable(_)),
        "exit-1 maps to Unavailable"
    );
}

/// `SubprocessAnalyzeClient` implements the `AnalyzeClient` trait object.
#[test]
fn subprocess_client_trait_object_compiles() {
    fn _accepts_dyn(_c: &dyn AnalyzeClient) {}
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878")
        .expect("TLS init should succeed");
    _accepts_dyn(&client);
}
