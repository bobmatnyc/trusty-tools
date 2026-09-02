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
// #4440: the single, shared interpretation of a trusty-search /health payload.
// This module must CONSUME it rather than re-deriving its own — see `health`.
use crate::integrations::health::{HealthResponse, ServingState};

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

    /// Whether trusty-search knows an index by this id (#6687).
    ///
    /// Why: `has_analysis` used to ignore its `index_id` argument entirely, so
    /// the analyze side reported itself ready for an index that does not exist —
    /// the same swallowed failure the search side had. Asking the daemon is the
    /// whole fix.
    /// What: `GET /indexes/{id}/status`; `false` ONLY on `404`, which is the
    /// daemon saying it has never heard of this index. Any other outcome —
    /// `200`, a `503` residency miss, or a probe that could not complete —
    /// answers `true`, because `health()` has already established the daemon is
    /// up and an indeterminate probe must not manufacture an analyze outage.
    /// Test: `subprocess_client_has_no_analysis_for_an_unknown_index`.
    async fn search_index_exists(&self, index_id: &str) -> bool {
        let url = format!(
            "{}/indexes/{index_id}/status",
            self.search_url.trim_end_matches('/')
        );
        match self.probe_http.get(&url).send().await {
            Ok(resp) => {
                let known = resp.status() != reqwest::StatusCode::NOT_FOUND;
                if !known {
                    tracing::warn!(
                        index = %index_id,
                        "trusty-search has no index `{index_id}` — trusty-analyze has nothing to \
                         analyse for it (#6687)"
                    );
                }
                known
            }
            Err(e) => {
                tracing::debug!("index existence probe failed for `{index_id}` (optional): {e}");
                true
            }
        }
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
    /// "can we run an analysis?", which requires both trusty-search (the data
    /// source `trusty-analyze` itself queries) AND a runnable binary.
    ///
    /// Issue #4440: this probe used to deserialise the search payload into a
    /// private one-field struct and test `status == "ok"` as a literal string —
    /// a second, cruder copy of the decision
    /// [`HealthResponse::serving_state`] already makes. trusty-search latches
    /// `status: "degraded"` for its ENTIRE process lifetime once warm boot skips
    /// any index (`degraded_by_timeout` / `degraded_by_tcc` are boot-time
    /// counters that are never decremented), so on a daemon that was up,
    /// embedder-ready and answering queries normally, the string test pinned
    /// `has_analysis` at `false` and `context_gate` skipped every single review
    /// with "trusty-analyze unreachable/not-ready". The search-side gate in
    /// `pipeline::context_gate` received exactly this fix under #4086; this
    /// duplicated twin did not. It now CONSUMES `serving_state` so there is one
    /// place — and only one — that decides what a trusty-search health payload
    /// means.
    ///
    /// What: GETs `<search_url>/health`, deserialises the full
    /// [`HealthResponse`], and refuses ONLY on [`ServingState::NotServing`]
    /// (embedder down, or a status that is neither `"ok"` nor `"degraded"`).
    /// A `Degraded` daemon is answering queries and so passes, carrying
    /// trusty-search's own status string through verbatim. Then verifies the
    /// binary executes with `--version`. A genuinely dead dependency — either
    /// half — still fails: this narrows the false-positive, it does not turn the
    /// probe into an always-pass.
    /// Test: `subprocess_client_health_check_fails_gracefully`,
    /// `subprocess_client_degraded_search_still_has_analysis`,
    /// `subprocess_client_not_serving_search_has_no_analysis`,
    /// `subprocess_client_health_preserves_degraded_status_string`.
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

        // #4440: parse the FULL trusty-search health payload and delegate the
        // verdict to the shared `serving_state()`, instead of re-testing
        // `status == "ok"` on a locally-declared one-field struct. The old
        // private struct is gone deliberately: keeping it is what let this copy
        // drift out of sync with the #4086 fix in the first place.
        let sh: HealthResponse = serde_json::from_str(&body)
            .map_err(|e| AnalyzeClientError::Parse(format!("search health parse: {e}")))?;

        // Refuse ONLY when trusty-search genuinely cannot answer queries.
        // `Degraded` means "serving, with a named capability gap" — a review can
        // still be produced against it, and blocking on it is the #4440 bug.
        if let ServingState::NotServing(reason) = sh.serving_state() {
            return Err(AnalyzeClientError::Unavailable(format!(
                "trusty-search at {url} is not serving: {reason}"
            )));
        }

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

        // #4440: `search_reachable` is `is_serving()`, NOT `status == "ok"`.
        // The `NotServing` case already returned `Err` above, so reaching here
        // means trusty-search is answering queries — possibly degraded, which is
        // still analysable. `status` carries trusty-search's own verdict through
        // verbatim so anything that displays it is never told a degraded daemon
        // said "ok".
        Ok(AnalyzeHealthResponse {
            status: sh.status.clone(),
            search_reachable: sh.is_serving(),
        })
    }

    /// Two-step readiness probe: trusty-search reachable AND binary resolvable.
    ///
    /// Why: spec REV-441 applies to the subprocess model too — both the data
    /// source (trusty-search) and the analysis runtime (the binary) must be
    /// confirmed before the pipeline marks analyze available.
    /// What: calls `health()` — which already enforces BOTH preconditions
    /// (trusty-search serving, binary runnable) — and gates on
    /// `search_reachable`.
    ///
    /// Issue #4440: this gated on `AnalyzeHealthResponse::is_healthy()`, i.e.
    /// `status == "ok" && search_reachable`. `is_healthy` is the strict
    /// "fully nominal" gate, and trusty-search latches `"degraded"` for its whole
    /// process lifetime after any warm-boot skip, so this returned `false`
    /// forever on a perfectly serving daemon and `context_gate` skipped every
    /// review. `search_reachable` is now derived from
    /// [`HealthResponse::is_serving`], the same three-state classification the
    /// search-side gate uses, so degraded-but-serving proceeds while a daemon
    /// that genuinely cannot answer still returns `false`.
    ///
    /// #6687: `index_id` is now read, not discarded. The subprocess model reads
    /// its data OUT of trusty-search, so an index trusty-search has never heard
    /// of means `trusty-analyze` has nothing to analyse either — yet this probe
    /// answered `true` for any index name whatsoever, so the analyze side could
    /// not catch a missing index that the search side had also swallowed. It now
    /// asks the daemon whether the index exists and answers `false` when it does
    /// not. A probe that cannot reach the status endpoint at all does NOT make
    /// this `false`: `health()` already established the daemon is up, so an
    /// indeterminate answer is treated as "index present" rather than
    /// manufacturing an analyze outage out of a flaky probe.
    /// Test: `subprocess_client_has_analysis_returns_false_on_error`,
    /// `subprocess_client_degraded_search_still_has_analysis`,
    /// `subprocess_client_not_serving_search_has_no_analysis`,
    /// `subprocess_client_has_no_analysis_for_an_unknown_index`.
    async fn has_analysis(&self, index_id: &str) -> bool {
        match self.health().await {
            Ok(h) => h.search_reachable && self.search_index_exists(index_id).await,
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
