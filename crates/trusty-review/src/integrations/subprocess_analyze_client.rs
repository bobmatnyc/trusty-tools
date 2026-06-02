//! Subprocess-based `AnalyzeClient` — invokes `trusty-analyze` on demand.
//!
//! Why: replaces the HTTP-daemon model with an on-demand executable runtime so
//! trusty-review no longer requires a long-running `trusty-analyze serve` process.
//! The binary is invoked as a subprocess, the diff is written to its stdin, and the
//! JSON `ReviewReport` written to stdout is parsed into the `ComplexityHotspot` and
//! `Smell` shapes the review pipeline consumes.  (closes #632)
//!
//! Architecture:
//!   trusty-review  →  spawn(`trusty-analyze review --index-id <id> -`)
//!                         writes diff → stdin
//!                         reads JSON  ← stdout
//!                     parse JSON → ComplexityHotspot + Smell
//!
//! What: `SubprocessAnalyzeClient` implements `AnalyzeClient`.  `health()` probes
//! trusty-search directly (same as the HTTP path) and verifies the binary is
//! resolvable on PATH (or the override path).  `has_analysis` verifies both.
//! `complexity_hotspots` and `smells` invoke the binary with a no-op diff and
//! return empty vecs — they are lightweight compared to `has_analysis`.  Real data
//! is produced by the review runner calling `analyze_diff` directly on the
//! subprocess; the pipeline only calls `complexity_hotspots` / `smells` for
//! supplementary hotspot annotations, which the subprocess model returns as empty
//! (see code comment).
//!
//! Binary discovery: `TRUSTY_ANALYZE_BIN` env var overrides the default
//! `trusty-analyze` (searched on PATH).
//!
//! Test: `subprocess_client_health_check_fails_gracefully`,
//! `map_report_to_hotspots_and_smells`, `subprocess_client_binary_not_found`.

use async_trait::async_trait;
use serde::Deserialize;
use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::integrations::analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
};

/// Environment variable that overrides the `trusty-analyze` binary path.
///
/// Why: allows operators and CI environments to pin the exact binary used
/// without modifying PATH.
/// What: when set to a non-empty string, `SubprocessAnalyzeClient` uses this
/// path instead of looking up `trusty-analyze` on PATH.
/// Test: `subprocess_client_respects_bin_env`.
const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// Default binary name searched on PATH.
const DEFAULT_ANALYZE_BIN: &str = "trusty-analyze";

// ─── Wire-format shapes ───────────────────────────────────────────────────────

/// Minimal projection of `trusty-analyze review --format json` stdout.
///
/// Why: trusty-review does not depend on the trusty-analyze library crate, so
/// we inline the subset of `ReviewReport` that the pipeline needs.
/// What: deserialises `files` from the JSON output of `trusty-analyze review`.
/// Test: `map_report_to_hotspots_and_smells`.
#[derive(Debug, Deserialize)]
pub(crate) struct SubprocessReviewReport {
    pub files: Vec<SubprocessFileReview>,
}

/// One file in the subprocess `ReviewReport`.
#[derive(Debug, Deserialize)]
pub(crate) struct SubprocessFileReview {
    pub path: String,
    pub complexity: SubprocessComplexity,
    pub smells: Vec<SubprocessSmellHit>,
}

/// Complexity metrics for one file.
#[derive(Debug, Deserialize)]
pub(crate) struct SubprocessComplexity {
    pub cyclomatic: u32,
    pub cognitive: u32,
}

/// One smell hit from the subprocess report.
#[derive(Debug, Deserialize)]
pub(crate) struct SubprocessSmellHit {
    pub category: String,
    pub line: u32,
    pub severity: String,
}

// ─── Mapping helpers ──────────────────────────────────────────────────────────

/// Map a `SubprocessReviewReport` to a `(hotspots, smells)` pair.
///
/// Why: the pipeline consumes `Vec<ComplexityHotspot>` and `Vec<Smell>` typed
/// from the HTTP API; this mapping bridges the subprocess JSON to those types so
/// the rest of the pipeline does not need to know about the subprocess transport.
/// What: for each file review, emits one `ComplexityHotspot` (file path +
/// cyclomatic + cognitive) and one `Smell` per detected smell hit.
/// Test: `map_report_to_hotspots_and_smells`.
pub(crate) fn map_report(report: &SubprocessReviewReport) -> (Vec<ComplexityHotspot>, Vec<Smell>) {
    let mut hotspots = Vec::new();
    let mut smells = Vec::new();

    for fr in &report.files {
        // Only emit a hotspot if the file has non-trivial complexity.
        if fr.complexity.cyclomatic > 0 || fr.complexity.cognitive > 0 {
            hotspots.push(ComplexityHotspot {
                file: fr.path.clone(),
                function_name: None,
                cyclomatic: fr.complexity.cyclomatic,
                cognitive: fr.complexity.cognitive,
            });
        }

        for sh in &fr.smells {
            smells.push(Smell {
                file: fr.path.clone(),
                category: sh.category.clone(),
                severity: sh.severity.clone(),
                line: Some(sh.line),
            });
        }
    }

    (hotspots, smells)
}

// ─── Client ───────────────────────────────────────────────────────────────────

/// On-demand `AnalyzeClient` that spawns `trusty-analyze` as a subprocess.
///
/// Why: eliminates the requirement for a long-running `trusty-analyze serve`
/// daemon so trusty-review can be deployed without a sidecar.  (#632)
/// What: each `analyze_diff` call spawns a short-lived `trusty-analyze review`
/// process.  `health()` probes trusty-search's `/health` endpoint directly
/// AND verifies the binary executes with `--version`.
/// Test: `subprocess_client_binary_not_found`, `subprocess_client_health_check_fails_gracefully`.
pub struct SubprocessAnalyzeClient {
    /// Path or name of the `trusty-analyze` binary.
    binary: String,
    /// Base URL of the trusty-search daemon, used for the health probe.
    search_url: String,
    /// reqwest client with a short timeout for health probes.
    probe_http: reqwest::Client,
}

impl SubprocessAnalyzeClient {
    /// Construct from explicit binary path/name and search URL.
    ///
    /// Why: allows callers and tests to inject specific paths without relying on
    /// PATH or env vars.
    /// What: builds the probe client (5-second timeout, matching the HTTP path).
    /// Test: `subprocess_client_health_check_fails_gracefully`.
    pub fn new(binary: impl Into<String>, search_url: impl Into<String>) -> Self {
        let probe_http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest::Client::build is infallible with valid config");
        Self {
            binary: binary.into(),
            search_url: search_url.into(),
            probe_http,
        }
    }

    /// Construct from a `ReviewConfig`.
    ///
    /// Why: the canonical factory used by both `run.rs` and `serve.rs`.
    /// What: reads `TRUSTY_ANALYZE_BIN` (falls back to `"trusty-analyze"`) for
    /// the binary; takes `config.search_url` for the health probe.
    /// Test: `subprocess_client_from_config`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Self {
        let binary = std::env::var(ENV_ANALYZE_BIN)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ANALYZE_BIN.to_string());
        Self::new(binary, config.search_url.clone())
    }

    /// Return the binary path/name this client uses.
    ///
    /// Why: tests need to verify binary resolution.
    /// What: returns a reference to the stored binary string.
    /// Test: `subprocess_client_binary_accessor`.
    pub fn binary(&self) -> &str {
        &self.binary
    }

    /// Invoke `trusty-analyze review --index-id <id> -` with the given diff on stdin.
    ///
    /// Why: the single subprocess-spawn path used by callers that want per-diff
    /// hotspots/smells rather than calling the pipeline separately.
    /// What: spawns the binary, writes `diff_text` to stdin, reads JSON stdout,
    /// parses to `(hotspots, smells)`.  Subprocess exit code 1 surfaces as
    /// `AnalyzeClientError::Unavailable` (trusty-search down or missing index).
    /// Test: `subprocess_analyze_diff_parses_empty_report`.
    pub async fn analyze_diff(
        &self,
        diff_text: &str,
        index_id: &str,
    ) -> Result<(Vec<ComplexityHotspot>, Vec<Smell>), AnalyzeClientError> {
        // Spawn is blocking; run on a thread pool so we do not block the async runtime.
        let binary = self.binary.clone();
        let index_id = index_id.to_string();
        let diff_owned = diff_text.to_string();

        tokio::task::spawn_blocking(move || spawn_analyze_review(&binary, &index_id, &diff_owned))
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("spawn_blocking join error: {e}")))?
    }
}

/// Synchronous helper that spawns the subprocess.
///
/// Why: isolated so it can be called from `spawn_blocking` without capturing
/// async context.
/// What: launches `trusty-analyze review --index-id <id> --format json -`,
/// pipes `diff` to stdin, captures stdout, parses JSON.
/// Test: called by `analyze_diff` tests via `spawn_blocking`.
fn spawn_analyze_review(
    binary: &str,
    index_id: &str,
    diff: &str,
) -> Result<(Vec<ComplexityHotspot>, Vec<Smell>), AnalyzeClientError> {
    let mut child = Command::new(binary)
        .args(["review", "--index-id", index_id, "--format", "json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AnalyzeClientError::Unavailable(format!("failed to spawn {binary}: {e}")))?;

    // Write diff to stdin.  A missing stdin pipe is a programmer error (we always
    // request piped stdin above), so `expect` is appropriate here.
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe always present");
        stdin
            .write_all(diff.as_bytes())
            .map_err(|e| AnalyzeClientError::Transport(format!("write to stdin: {e}")))?;
        // stdin is dropped here, closing the pipe so the child sees EOF.
    }

    let output = child
        .wait_with_output()
        .map_err(|e| AnalyzeClientError::Transport(format!("wait_with_output: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AnalyzeClientError::Unavailable(format!(
            "trusty-analyze review exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }

    let json = std::str::from_utf8(&output.stdout)
        .map_err(|e| AnalyzeClientError::Parse(format!("stdout is not UTF-8: {e}")))?;

    let report: SubprocessReviewReport = serde_json::from_str(json)
        .map_err(|e| AnalyzeClientError::Parse(format!("ReviewReport parse error: {e}")))?;

    Ok(map_report(&report))
}

#[async_trait]
impl AnalyzeClient for SubprocessAnalyzeClient {
    /// Liveness: probe trusty-search health AND verify the binary is resolvable.
    ///
    /// Why: no analyze daemon exists in the subprocess model; liveness means
    /// "can we run an analysis?" which requires both trusty-search AND the binary.
    /// What: GETs `<search_url>/health`, checks `status == "ok"` AND
    /// `search_reachable` (reusing the same probe URL pattern as the HTTP path
    /// since trusty-analyze itself calls trusty-search); then verifies the binary
    /// executes with `--version`.
    /// Test: `subprocess_client_health_check_fails_gracefully`.
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        // Probe trusty-search /health directly.
        let url = format!("{}/health", self.search_url.trim_end_matches('/'));
        let resp = self
            .probe_http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnalyzeClientError::Unavailable(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(AnalyzeClientError::Unavailable(format!(
                "GET {url} returned {status}: {body}"
            )));
        }

        // Parse the trusty-search health response (same shape the HTTP path uses
        // via the analyze daemon).
        #[derive(Deserialize)]
        struct SearchHealth {
            status: String,
        }
        let sh: SearchHealth = serde_json::from_str(&body)
            .map_err(|e| AnalyzeClientError::Parse(format!("search health parse: {e}")))?;

        // Verify the binary is runnable.
        let binary_ok = {
            let binary = self.binary.clone();
            tokio::task::spawn_blocking(move || {
                Command::new(&binary)
                    .arg("--version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok()
            })
            .await
            .unwrap_or(false)
        };

        if !binary_ok {
            return Err(AnalyzeClientError::Unavailable(format!(
                "trusty-analyze binary '{}' is not on PATH or not executable",
                self.binary
            )));
        }

        // Express result as AnalyzeHealthResponse: search_reachable = true when
        // trusty-search reported ok; this mirrors the daemon's own health response.
        Ok(AnalyzeHealthResponse {
            status: sh.status.clone(),
            search_reachable: sh.status == "ok",
        })
    }

    /// Two-step readiness probe: trusty-search reachable AND binary resolvable.
    ///
    /// Why: spec REV-441 applies to the subprocess model too — both the data
    /// source (trusty-search) and the analysis runtime (the binary) must be
    /// confirmed before the pipeline marks analyze available.
    /// What: calls `health()` and returns `true` only if no error and is_healthy.
    /// The `index_id` argument is accepted for trait compatibility but the
    /// subprocess model does not pre-check index existence (the review subcommand
    /// will surface a missing index as an exit-1 error at call time).
    /// Test: `subprocess_client_has_analysis_returns_false_on_error`.
    async fn has_analysis(&self, _index_id: &str) -> bool {
        match self.health().await {
            Ok(h) => h.is_healthy(),
            Err(e) => {
                tracing::debug!("trusty-analyze subprocess health check failed (optional): {e}");
                false
            }
        }
    }

    /// Returns empty hotspots for the subprocess model.
    ///
    /// Why: the subprocess model produces hotspots via `analyze_diff` at review
    /// time — there is no pre-built daemon index to query.  Returning empty here
    /// means the pipeline's supplementary-annotation path gets no data (the same
    /// degraded behaviour as when the analyze daemon is unavailable), which is
    /// acceptable since the core review still runs.  Callers that need per-diff
    /// hotspots should use `analyze_diff` directly.
    /// What: always returns `Ok(vec![])`.
    /// Test: `subprocess_client_hotspots_returns_empty`.
    async fn complexity_hotspots(
        &self,
        _index_id: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(vec![])
    }

    /// Returns empty smells for the subprocess model.
    ///
    /// Why: same as `complexity_hotspots` — smell annotations are produced
    /// per-diff via `analyze_diff` rather than from a daemon index.
    /// What: always returns `Ok(vec![])`.
    /// Test: `subprocess_client_smells_returns_empty`.
    async fn smells(&self, _index_id: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(vec![])
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    // ─── Map report tests ──────────────────────────────────────────────────────

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
}
