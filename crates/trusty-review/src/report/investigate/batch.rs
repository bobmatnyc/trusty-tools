//! Multi-batch investigation requests (wave-3 hardening, #2357 follow-up).
//!
//! Why: a live-QA incident reproduced twice on a real repo — a single unbatched
//! investigation request on a 175-file selection sent 282 KB of file content;
//! the findings JSON hit the output-token ceiling mid-array, `finish_reason =
//! length` correctly failed closed, but that discarded ALL findings for a fully
//! readable repository, regenerating the exact "no evidence-based conclusions
//! can be drawn" exec-summary lament the tool exists to make impossible. Bumping
//! the token ceiling alone only postpones the same collapse on a larger repo.
//! The structural fix is to never send more file content than fits comfortably
//! in one bounded response: partition the (already budget-capped, relevance-
//! ranked) selection into multiple requests, each small enough that truncation
//! is structurally unlikely, and treat a batch's failure as a failure of THAT
//! BATCH ONLY — other batches' findings must survive.
//! What: [`partition_batches`] splits a selection's files into size-bounded,
//! order-preserving [`FileBatch`]es; [`run_batches`] drives one LLM request per
//! batch (retrying once, more concisely, on truncation), verifies each batch's
//! findings against the FULL selection (not just that batch, so a stray
//! cross-batch citation still resolves correctly), and merges + dedupes the
//! survivors across batches by `(file, title)` — keeping the higher-severity
//! (or first) copy.  [`BatchOutcome`] records each batch's fate for the coverage
//! section.
//! Test: `batch_tests.rs` covers partition sizing/ordering/edge-cases, merge +
//! dedupe (including severity-upgrade), and the retry-once-then-fail-closed
//! state machine with a scripted mock provider.

use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::llm::LlmProvider;
use crate::report::metrics::Severity;

use super::analyze::{self, MAX_FINDINGS_PER_BATCH, RETRY_MAX_FINDINGS};
use super::select::{SelectedFile, Selection};
use super::verify::{self, VerifiedFinding};

/// Maximum file-content bytes sent in one investigation LLM request.
///
/// Why: this is the structural bound that replaces "hope a bigger token ceiling
/// is enough" — a request built from at most this much source text, combined
/// with the output-shape caps in `analyze.rs` (`maxItems`/`maxLength`), keeps
/// every request's expected response comfortably under
/// [`analyze::INVESTIGATION_MAX_TOKENS`] regardless of how large the repository
/// is.  Chosen well below the (already conservative) 400 KB total-selection
/// budget so a typical selection needs only a handful of batches.
pub const BATCH_MAX_BYTES: usize = 90 * 1024;

/// Wall-clock ceiling for one batch attempt (initial or retry).
const BATCH_TIMEOUT: Duration = Duration::from_secs(180);

/// One partitioned, order-preserving slice of the selection's files.
///
/// Why: batches must stay in relevance-ranked order (highest-value files first)
/// so a truncation-driven drop always sacrifices the least valuable content.
/// What: `index` is the 1-based batch position (used in prompts + coverage
/// notes); `files` is a contiguous, cloned slice of the ranked selection.
#[derive(Debug, Clone)]
pub struct FileBatch {
    /// 1-based batch position.
    pub index: usize,
    /// This batch's files, in relevance-ranked order.
    pub files: Vec<SelectedFile>,
}

impl FileBatch {
    /// Total content bytes in this batch (test-only budget assertion helper).
    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.files.iter().map(|f| f.content.len()).sum()
    }
}

/// Partition `files` (already relevance-ranked) into batches of at most
/// [`BATCH_MAX_BYTES`] content bytes each, preserving order.
///
/// Why: this is the core structural fix — bounding request size makes output
/// truncation structurally unlikely rather than merely less likely.
/// What: greedily accumulates files into the current batch; when the next file
/// would push the running total over the cap AND the current batch is
/// non-empty, the current batch closes and a new one starts.  A single file
/// already larger than the cap (should not happen — `select.rs` truncates
/// individual files to ~24 KiB, well under this cap) still gets its own batch
/// rather than being silently dropped.  Empty input yields no batches.
/// Test: `batch_tests::{partitions_by_size, oversize_file_gets_own_batch,
/// empty_input_yields_no_batches}`.
pub fn partition_batches(files: &[SelectedFile]) -> Vec<FileBatch> {
    let mut batches: Vec<FileBatch> = Vec::new();
    let mut current: Vec<SelectedFile> = Vec::new();
    let mut current_bytes = 0usize;

    for f in files {
        let len = f.content.len();
        if !current.is_empty() && current_bytes + len > BATCH_MAX_BYTES {
            batches.push(FileBatch {
                index: batches.len() + 1,
                files: std::mem::take(&mut current),
            });
            current_bytes = 0;
        }
        current_bytes += len;
        current.push(f.clone());
    }
    if !current.is_empty() {
        batches.push(FileBatch {
            index: batches.len() + 1,
            files: current,
        });
    }
    batches
}

/// The fate of one batch's LLM analysis attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchStatus {
    /// The batch's response parsed cleanly (findings may still be empty).
    Completed,
    /// The batch's response was truncated even after the concise retry.
    Truncated,
    /// The batch failed for a non-truncation reason (provider/parse error).
    Unavailable(String),
}

impl BatchStatus {
    /// True when this batch contributed no verified findings by design failure
    /// (as opposed to `Completed` with zero findings, which is a real result).
    pub fn is_failure(&self) -> bool {
        !matches!(self, BatchStatus::Completed)
    }

    /// A short, report-ready reason string for a failed batch.
    pub fn reason(&self) -> String {
        match self {
            BatchStatus::Completed => String::new(),
            BatchStatus::Truncated => "truncated (even after concise retry)".to_string(),
            BatchStatus::Unavailable(reason) => reason.clone(),
        }
    }
}

/// One batch's outcome, retained for the coverage section (#2357 wave-3.1).
#[derive(Debug, Clone)]
pub struct BatchOutcome {
    /// 1-based batch position.
    pub index: usize,
    /// Total batch count for this repository (for "batch N of M" notes).
    pub batch_count: usize,
    /// Repository-relative paths of the files in this batch.
    pub files: Vec<String>,
    /// The attempt's fate.
    pub status: BatchStatus,
}

/// Drive one LLM request per batch (with a one-shot concise retry on
/// truncation), verify each batch's findings, and merge + dedupe survivors.
///
/// Why: the single entry point [`super::run_investigation`] awaits per
/// repository; a batch's failure must never take down the others' findings —
/// that is the whole point of batching over one large request.
/// What: for each batch, calls the provider under [`BATCH_TIMEOUT`]; on
/// `finish_reason = length`/`max_tokens`, retries ONCE with
/// [`RETRY_MAX_FINDINGS`] and a concise directive; a still-truncated retry, a
/// provider error, or an unparseable response fails THAT BATCH closed
/// ([`BatchStatus::Truncated`] / [`BatchStatus::Unavailable`]) while later
/// batches still run.  Findings are verified against `selection` (the full
/// per-repo file set, not just the batch) so any well-formed cross-batch
/// citation still resolves.  Survivors are merged and deduped by `(file,
/// title)`, keeping the higher-severity (or, on a tie, first-seen) copy.
/// Test: `batch_tests::{one_truncated_batch_others_survive, retry_recovers,
/// merge_dedupes_keeping_higher_severity, all_batches_can_fail}`.
pub async fn run_batches(
    provider: Arc<dyn LlmProvider>,
    llm_model: &str,
    app_name: &str,
    batches: &[FileBatch],
    total_files: usize,
    instructions: Option<&str>,
    selection: &Selection,
) -> (Vec<VerifiedFinding>, usize, Vec<BatchOutcome>) {
    let batch_count = batches.len();
    let mut all_verified: Vec<VerifiedFinding> = Vec::new();
    let mut total_rejected = 0usize;
    let mut outcomes = Vec::with_capacity(batch_count);

    for batch in batches {
        let (status, verified, rejected) = run_one_batch(
            provider.clone(),
            llm_model,
            app_name,
            batch,
            batch_count,
            total_files,
            instructions,
            selection,
        )
        .await;
        total_rejected += rejected;
        all_verified.extend(verified);
        outcomes.push(BatchOutcome {
            index: batch.index,
            batch_count,
            files: batch.files.iter().map(|f| f.path.clone()).collect(),
            status,
        });
    }

    (merge_dedupe(all_verified), total_rejected, outcomes)
}

/// Run one batch to completion: initial attempt, then (on truncation only) one
/// concise retry, returning its status + verified findings + rejected count.
#[allow(clippy::too_many_arguments)]
async fn run_one_batch(
    provider: Arc<dyn LlmProvider>,
    llm_model: &str,
    app_name: &str,
    batch: &FileBatch,
    batch_count: usize,
    total_files: usize,
    instructions: Option<&str>,
    selection: &Selection,
) -> (BatchStatus, Vec<VerifiedFinding>, usize) {
    match try_batch(
        provider.clone(),
        llm_model,
        app_name,
        batch,
        batch_count,
        total_files,
        instructions,
        MAX_FINDINGS_PER_BATCH,
        false,
    )
    .await
    {
        Attempt::Ok(raw) => {
            let outcome = verify::verify_findings(raw, selection);
            (BatchStatus::Completed, outcome.verified, outcome.rejected)
        }
        Attempt::Truncated => {
            warn!(
                batch = batch.index,
                total = batch_count,
                "investigation: batch truncated — retrying once, concise"
            );
            match try_batch(
                provider,
                llm_model,
                app_name,
                batch,
                batch_count,
                total_files,
                instructions,
                RETRY_MAX_FINDINGS,
                true,
            )
            .await
            {
                Attempt::Ok(raw) => {
                    let outcome = verify::verify_findings(raw, selection);
                    (BatchStatus::Completed, outcome.verified, outcome.rejected)
                }
                Attempt::Truncated => {
                    warn!(
                        batch = batch.index,
                        "investigation: batch still truncated after retry — failing this batch closed"
                    );
                    (BatchStatus::Truncated, Vec::new(), 0)
                }
                Attempt::Failed(reason) => (BatchStatus::Unavailable(reason), Vec::new(), 0),
            }
        }
        Attempt::Failed(reason) => (BatchStatus::Unavailable(reason), Vec::new(), 0),
    }
}

/// The outcome of one provider call attempt for a batch (pre-verification).
enum Attempt {
    /// Parsed cleanly; carries the raw (unverified) findings.
    Ok(Vec<analyze::RawFinding>),
    /// The response was truncated at the output-token ceiling.
    Truncated,
    /// A provider/timeout/parse error — not a truncation.
    Failed(String),
}

/// Make one provider call for `batch` and classify the result.
#[allow(clippy::too_many_arguments)]
async fn try_batch(
    provider: Arc<dyn LlmProvider>,
    llm_model: &str,
    app_name: &str,
    batch: &FileBatch,
    batch_count: usize,
    total_files: usize,
    instructions: Option<&str>,
    max_findings: usize,
    retry_concise: bool,
) -> Attempt {
    let req = analyze::build_request(
        app_name,
        &batch.files,
        batch.index,
        batch_count,
        total_files,
        instructions,
        llm_model,
        max_findings,
        retry_concise,
    );
    let resp = match tokio::time::timeout(BATCH_TIMEOUT, provider.complete(req)).await {
        Err(_) => return Attempt::Failed("provider timeout".to_string()),
        Ok(Err(e)) => return Attempt::Failed(format!("provider error: {e}")),
        Ok(Ok(r)) => r,
    };

    if matches!(
        resp.finish_reason.as_deref(),
        Some("length") | Some("max_tokens")
    ) {
        return Attempt::Truncated;
    }

    match analyze::parse_findings(&resp.text) {
        Some(raw) => Attempt::Ok(raw.findings),
        None => Attempt::Failed("unparseable response".to_string()),
    }
}

/// Merge findings across batches, deduping by `(file, title)`.
///
/// Why: a repository property (e.g. "no test directory") or an evidence pattern
/// occurring in more than one batch could otherwise surface as duplicate rows;
/// keeping the higher-severity copy (ties broken by first-seen) is the more
/// conservative — and simpler — choice than trying to merge prose.
/// What: walks the findings in batch order, keyed by `(file, title)`; a
/// duplicate key upgrades the kept copy only if the new one has strictly higher
/// severity (RED > AMBER > GREEN); otherwise the first-seen copy is kept.
/// Test: `batch_tests::merge_dedupes_keeping_higher_severity`.
fn merge_dedupe(all: Vec<VerifiedFinding>) -> Vec<VerifiedFinding> {
    fn rank(s: Severity) -> u8 {
        match s {
            Severity::Red => 2,
            Severity::Amber => 1,
            Severity::Green => 0,
        }
    }

    let mut out: Vec<VerifiedFinding> = Vec::new();
    for f in all {
        match out
            .iter_mut()
            .find(|e| e.file == f.file && e.title == f.title)
        {
            Some(existing) if rank(f.severity) > rank(existing.severity) => *existing = f,
            Some(_) => {}
            None => out.push(f),
        }
    }
    out
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
