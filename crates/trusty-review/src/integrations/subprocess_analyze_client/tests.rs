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
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878");
    assert_eq!(client.binary(), "trusty-analyze");
}

/// Verify health() returns Unavailable (not a panic) when trusty-search is down.
#[tokio::test]
async fn subprocess_client_health_check_fails_gracefully() {
    // Port 1 is always refused.
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:1");
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
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:1");
    assert!(
        !client.has_analysis("main").await,
        "has_analysis must return false on error"
    );
}

/// complexity_hotspots always returns empty for the subprocess model.
#[tokio::test]
async fn subprocess_client_hotspots_returns_empty() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878");
    let result = client.complexity_hotspots("main", Some(10)).await.unwrap();
    assert!(
        result.is_empty(),
        "subprocess model always returns empty hotspots"
    );
}

/// smells always returns empty for the subprocess model.
#[tokio::test]
async fn subprocess_client_smells_returns_empty() {
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878");
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
        SubprocessAnalyzeClient::new("trusty-analyze-nonexistent-binary", "http://127.0.0.1:1");
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
    let client = SubprocessAnalyzeClient::new("trusty-analyze", "http://127.0.0.1:7878");
    _accepts_dyn(&client);
}
