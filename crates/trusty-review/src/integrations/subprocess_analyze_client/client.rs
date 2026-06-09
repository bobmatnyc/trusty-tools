//! `SubprocessAnalyzeClient` — the concrete client type.
//!
//! Why: isolated here so the wire-format types and mapping logic (mod.rs)
//! and the unit tests (tests.rs) can each stay under the 500-line cap.
//! What: implements `AnalyzeClient` by spawning `trusty-analyze` on demand.
//! Test: see `tests.rs` for all unit and async tests.

use async_trait::async_trait;
use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::integrations::analyze_client::{
    AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
};

use super::{DEFAULT_ANALYZE_BIN, ENV_ANALYZE_BIN, SubprocessReviewReport, map_report};

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
    pub(super) binary: String,
    /// Base URL of the trusty-search daemon, used for the health probe.
    pub(super) search_url: String,
    /// reqwest client with a short timeout for health probes.
    pub(super) probe_http: reqwest::Client,
}

impl SubprocessAnalyzeClient {
    /// Construct from explicit binary path/name and search URL.
    ///
    /// Why: allows callers and tests to inject specific paths without relying on
    /// PATH or env vars.
    /// What: builds the probe client (5-second timeout, matching the HTTP path).
    /// Returns `Err(AnalyzeClientError::ClientInit)` if the TLS backend cannot
    /// be initialised — surfaces the failure to the caller rather than panicking
    /// at daemon startup (closes #953).
    /// Test: `subprocess_client_health_check_fails_gracefully`.
    pub fn new(
        binary: impl Into<String>,
        search_url: impl Into<String>,
    ) -> Result<Self, AnalyzeClientError> {
        let probe_http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| AnalyzeClientError::ClientInit(e.to_string()))?;
        Ok(Self {
            binary: binary.into(),
            search_url: search_url.into(),
            probe_http,
        })
    }

    /// Construct from a `ReviewConfig`.
    ///
    /// Why: the canonical factory used by both `run.rs` and `serve.rs`.
    /// What: reads `TRUSTY_ANALYZE_BIN` (falls back to `"trusty-analyze"`) for
    /// the binary; takes `config.search_url` for the health probe.  Propagates
    /// any TLS-backend init failure as `Err`.
    /// Test: `subprocess_client_from_config`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Result<Self, AnalyzeClientError> {
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
pub(super) fn spawn_analyze_review(
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
    //
    // BrokenPipe (EPIPE) is intentionally ignored here: it means the child
    // process exited before reading all of stdin (e.g. `false`, or a
    // trusty-analyze process that failed before reaching the stdin-read loop).
    // The real failure signal is the child's non-zero exit status, which is
    // surfaced as `Unavailable` below.  Treating EPIPE as `Transport` would
    // mask the actual cause and break the exit-code → Unavailable mapping on
    // Linux where the OS can deliver SIGPIPE before the write returns.
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe always present");
        match stdin.write_all(diff.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                // Child exited early; fall through to wait_with_output so the
                // non-zero exit code surfaces as Unavailable.
            }
            Err(e) => {
                return Err(AnalyzeClientError::Transport(format!(
                    "write to stdin: {e}"
                )));
            }
        }
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
        #[derive(serde::Deserialize)]
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
