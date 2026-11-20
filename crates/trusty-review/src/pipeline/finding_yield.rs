//! Finding yield per review: posted vs verifier-refuted counts (#11525).
//!
//! Why: tuning the verifier and the hygiene gates needs a number. The review
//! log JSON holds every count, but nothing summed them, so "how many findings
//! does a review post, and how many did the verifier refute?" had no answer
//! per PR or per repo (owner ruling D52; the Cloudflare signal-over-noise
//! measure in the source brief).
//! What: [`append_yield_row`] appends one [`YieldRow`] per written review to
//! `<log_dir>/finding_yield.jsonl`; [`summarize_yield`] reads that file and
//! returns totals plus per-repo and per-PR aggregates. `review_health` reports
//! the summary under `finding_yield`.
//! Test: `finding_yield_tests.rs`.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::models::ReviewResult;

/// File name of the per-review yield ledger inside the review log directory.
pub const YIELD_LEDGER_FILE: &str = "finding_yield.jsonl";

/// Most PR aggregates `summarize_yield` returns, most recent review first.
///
/// Why: the PR list grows without bound; a health call must stay small.
pub const MAX_PR_AGGREGATES: usize = 50;

/// One review's yield, as persisted in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YieldRow {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Pull request number.
    pub pr: u64,
    /// Head SHA the review ran against.
    pub head_sha: String,
    /// RFC 3339 review timestamp.
    pub timestamp: String,
    /// Findings posted (`findings_count`).
    pub posted: usize,
    /// Findings the verifier refuted (`withheld_by_reason["refuted"]`).
    pub refuted: usize,
}

impl YieldRow {
    /// The yield row for `result`.
    ///
    /// What: `posted` is the posted-finding count; `refuted` is the withheld
    /// count whose reason class is `refuted`, which only the verifier round
    /// writes (`withheld_contract::REFUTED_REASON`).
    /// Test: `yield_row_reads_posted_and_refuted`.
    pub fn from_result(result: &ReviewResult) -> Self {
        Self {
            owner: result.owner.clone(),
            repo: result.repo.clone(),
            pr: result.pr_number,
            head_sha: result.head_sha.clone(),
            timestamp: result.timestamp.clone(),
            posted: result.findings_count,
            refuted: result
                .withheld_by_reason
                .get("refuted")
                .copied()
                .unwrap_or(0),
        }
    }
}

/// Totals over a set of reviews.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct YieldAggregate {
    /// `owner/repo`, or `owner/repo#pr` for a PR aggregate; empty for the total.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// Reviews counted.
    pub reviews: usize,
    /// Findings posted across those reviews.
    pub posted: usize,
    /// Findings the verifier refuted across those reviews.
    pub refuted: usize,
    /// `posted / reviews`, the per-review finding yield.
    pub posted_per_review: f64,
    /// `refuted / (posted + refuted)`, or 0 when both are 0.
    pub refuted_share: f64,
    /// Latest review timestamp counted (used to order PR aggregates).
    #[serde(skip)]
    latest: String,
}

impl YieldAggregate {
    fn add(&mut self, row: &YieldRow) {
        self.reviews += 1;
        self.posted += row.posted;
        self.refuted += row.refuted;
        if row.timestamp > self.latest {
            self.latest = row.timestamp.clone();
        }
    }

    fn finish(mut self, key: String) -> Self {
        self.key = key;
        self.posted_per_review = ratio(self.posted, self.reviews);
        self.refuted_share = ratio(self.refuted, self.posted + self.refuted);
        self
    }
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

/// The yield report `review_health` returns.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct YieldSummary {
    /// Totals over every recorded review.
    pub total: YieldAggregate,
    /// One aggregate per `owner/repo`, sorted by key.
    pub by_repo: Vec<YieldAggregate>,
    /// One aggregate per PR, most recent first, at most [`MAX_PR_AGGREGATES`].
    pub by_pr: Vec<YieldAggregate>,
    /// Ledger lines that did not parse and were skipped.
    pub skipped_lines: usize,
}

/// Path of the yield ledger for `log_dir`.
pub fn ledger_path(log_dir: &Path) -> PathBuf {
    log_dir.join(YIELD_LEDGER_FILE)
}

/// Append `result`'s yield row to the ledger.
///
/// Why: a per-review row is the persisted count the aggregates are built from.
/// What: one JSON line, appended with `O_APPEND`, so concurrent reviews do not
/// interleave within a line. A failure is logged and never fails the review,
/// matching `write_review_log`.
/// Test: `append_then_summarize_aggregates_per_pr_and_repo`.
pub fn append_yield_row(result: &ReviewResult, log_dir: &Path) {
    let row = YieldRow::from_result(result);
    let path = ledger_path(log_dir);
    let write = || -> std::io::Result<()> {
        let mut line = serde_json::to_string(&row).map_err(std::io::Error::other)?;
        line.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), "failed to append finding-yield row: {e}");
    }
}

/// Read the ledger under `log_dir` and aggregate it per repo and per PR.
///
/// What: a missing ledger is an empty summary; an unparsable line is skipped
/// and counted in `skipped_lines`. Re-reviews of one PR each count as a review.
/// Test: `append_then_summarize_aggregates_per_pr_and_repo`,
/// `summarize_missing_ledger_is_empty`.
pub fn summarize_yield(log_dir: &Path) -> std::io::Result<YieldSummary> {
    let file = match std::fs::File::open(ledger_path(log_dir)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(YieldSummary::default()),
        Err(e) => return Err(e),
    };
    let mut rows = Vec::new();
    let mut skipped_lines = 0;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<YieldRow>(&line) {
            Ok(row) => rows.push(row),
            Err(_) => skipped_lines += 1,
        }
    }
    Ok(summarize_rows(&rows, skipped_lines))
}

/// Aggregate `rows` into a [`YieldSummary`].
pub fn summarize_rows(rows: &[YieldRow], skipped_lines: usize) -> YieldSummary {
    let mut total = YieldAggregate::default();
    let mut by_repo: BTreeMap<String, YieldAggregate> = BTreeMap::new();
    let mut by_pr: BTreeMap<String, YieldAggregate> = BTreeMap::new();
    for row in rows {
        total.add(row);
        let repo = format!("{}/{}", row.owner, row.repo);
        by_pr
            .entry(format!("{repo}#{}", row.pr))
            .or_default()
            .add(row);
        by_repo.entry(repo).or_default().add(row);
    }
    let mut prs: Vec<YieldAggregate> = by_pr.into_iter().map(|(k, a)| a.finish(k)).collect();
    prs.sort_by(|a, b| b.latest.cmp(&a.latest).then(a.key.cmp(&b.key)));
    prs.truncate(MAX_PR_AGGREGATES);
    YieldSummary {
        total: total.finish(String::new()),
        by_repo: by_repo.into_iter().map(|(k, a)| a.finish(k)).collect(),
        by_pr: prs,
        skipped_lines,
    }
}

#[cfg(test)]
#[path = "finding_yield_tests.rs"]
mod tests;
