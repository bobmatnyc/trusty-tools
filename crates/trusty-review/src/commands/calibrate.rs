//! Handler for the `calibrate` subcommand.
//!
//! Why: no systematic recall/precision tooling existed for trusty-review;
//! prior manual evals were one-off.  This harness takes a JSONL corpus of
//! merged PRs with human-review ground truth and produces a repeatable
//! recall/precision report so calibration regressions are caught early.
//!
//! What: loads a JSONL corpus (`CorpusEntry` per line), runs the review
//! pipeline in dry-run mode for each PR, fuzzy-matches trusty findings
//! against human findings by (file, kind), and emits a JSON report with
//! per-PR and aggregate metrics including `rust_semantic_fp_rate`.
//!
//! Test: `calibrate_tests.rs` — synthetic 3-PR corpus fixture with known
//! ground truth asserts recall/precision computation and fuzzy matcher.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use trusty_review::models::{Effort, Finding};

// ─── Corpus format ────────────────────────────────────────────────────────────

/// A single human finding in the corpus ground truth.
///
/// Why: the corpus records human reviewer observations as (file, kind) pairs
/// with an `acted_on` flag signalling the author actually addressed the finding.
/// What: flat struct mapping to the JSONL corpus schema.
/// Test: used directly in `calibrate_tests.rs` fixture construction.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HumanFinding {
    /// Changed file path (same form as `Finding::file`).
    pub file: String,
    /// Finding kind/category (e.g. `"logic-error"`, `"ownership"`).
    pub kind: String,
    /// True when the PR author acted on (addressed) this finding.
    pub acted_on: bool,
}

/// One PR entry in the calibration corpus (JSONL — one object per line).
///
/// Why: JSONL makes the corpus easy to append to and diff in git; one
/// struct per PR encapsulates its identity and ground-truth findings.
/// What: identifies the GitHub PR and carries a list of human findings.
/// Test: used directly in `calibrate_tests.rs` fixture construction.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CorpusEntry {
    /// GitHub organisation / user.
    pub owner: String,
    /// GitHub repository name.
    pub repo: String,
    /// Pull-request number.
    pub pr: u64,
    /// Human reviewer findings for this PR (ground truth).
    pub human_findings: Vec<HumanFinding>,
}

// ─── Report format ────────────────────────────────────────────────────────────

/// A false-positive finding emitted by trusty (no matching human finding).
///
/// Why: recording which (file, kind) pairs over-fire guides prompt and
/// noise-filter improvements without having to re-run the full corpus.
/// What: minimal shape — file, kind, and one-line description.
/// Test: collected and asserted in `calibrate_tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FpFinding {
    /// File the finding refers to.
    pub file: String,
    /// Finding kind (same as `Finding::kind`).
    pub kind: String,
    /// One-line description.
    pub description: String,
}

/// Per-PR calibration metrics.
///
/// Why: per-PR breakdown lets corpus authors pinpoint which PRs drag down
/// aggregate recall/precision and improve corpus quality iteratively.
/// What: records recall, precision, and the false-positive list for one PR.
/// Test: computed and asserted in `calibrate_tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrMetrics {
    /// Identifier string: `owner/repo#pr`.
    pub pr: String,
    /// Fraction of human findings recalled by trusty (0.0–1.0).
    pub recall: f64,
    /// Fraction of trusty findings that matched a human finding (0.0–1.0).
    pub precision: f64,
    /// Trusty findings that did NOT match any human finding (false positives).
    pub false_positives: Vec<FpFinding>,
}

/// Aggregate calibration report emitted by `cmd_calibrate`.
///
/// Why: the top-level output that CI and humans consume to track calibration
/// regressions across prompt changes, model upgrades, and corpus expansions.
/// What: aggregate recall/precision over all corpus PRs, per-PR details, and
/// the `rust_semantic_fp_rate` measuring precision of logic-error/ownership
/// findings on `.rs` files specifically (the known Rust FP hotspot).
/// Test: computed and asserted in `calibrate_tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationReport {
    /// Aggregate recall: recalled_total / human_total across all corpus PRs.
    pub recall: f64,
    /// Aggregate precision: recalled_total / trusty_total across all corpus PRs.
    pub precision: f64,
    /// Per-PR metrics (one entry per corpus line).
    pub per_pr: Vec<PrMetrics>,
    /// Precision of `logic-error`/`ownership` findings on `.rs` files.
    ///
    /// Why: trusty-review false-positives HIGH on these Rust semantic patterns
    /// (`move` closures, `tokio::select!` branches).  This field makes the
    /// false-positive rate measurable and regressionable.  Returns 1.0 (neutral)
    /// when no Rust semantic findings exist in the run.
    pub rust_semantic_fp_rate: f64,
}

// ─── CLI args ─────────────────────────────────────────────────────────────────

/// Arguments for the `calibrate` subcommand.
///
/// Why: bundles calibration flags for easy testing and CLI help text.
/// What: corpus path is required; output path is optional (defaults to stdout).
/// Test: `cargo run -p trusty-review -- calibrate --help`.
#[derive(Debug, clap::Parser)]
pub struct CalibrateArgs {
    /// Path to the JSONL corpus file (one CorpusEntry JSON object per line).
    #[arg(long, value_name = "FILE")]
    pub corpus: PathBuf,

    /// Write the JSON report to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

// ─── Fuzzy matcher ────────────────────────────────────────────────────────────

/// Returns `true` when a trusty `Finding` is "recalled" by a human finding.
///
/// Why: the match criterion must live in one testable function so changes are
/// deliberate.  A finding is recalled when the same file AND kind appear in
/// both the trusty output and the human ground truth.
/// What: case-insensitive kind comparison; exact file-path string comparison.
/// Test: `finding_recalled_same_file_kind`, `finding_not_recalled_diff_kind`,
/// `finding_not_recalled_diff_file` in `calibrate_tests.rs`.
pub fn is_recalled(trusty: &Finding, human: &HumanFinding) -> bool {
    trusty.file == human.file && trusty.kind.eq_ignore_ascii_case(&human.kind)
}

/// Returns `true` when the finding is a Rust-semantic false-positive candidate.
///
/// Why: the `rust_semantic_fp_rate` metric needs a consistent classifier.
/// What: the file ends in `.rs` AND kind is `logic-error` or `ownership`
/// (case-insensitive match).
/// Test: `rust_semantic_classifier` in `calibrate_tests.rs`.
pub fn is_rust_semantic(finding: &Finding) -> bool {
    finding.file.ends_with(".rs")
        && matches!(
            finding.kind.to_ascii_lowercase().as_str(),
            "logic-error" | "ownership"
        )
}

// ─── Metrics computation ──────────────────────────────────────────────────────

/// Compute per-PR and aggregate calibration metrics from corpus + trusty results.
///
/// Why: extracted as a pure function so it can be unit-tested without any I/O
/// or pipeline invocations.
/// What: for each (corpus_entry, trusty_findings) pair counts recalled human
/// findings, false-positive trusty findings, and accumulates into aggregate
/// recall/precision and the Rust semantic FP rate.
/// Test: `compute_metrics_three_pr_corpus` in `calibrate_tests.rs`.
pub fn compute_metrics(
    corpus: &[CorpusEntry],
    trusty_results: &[Vec<Finding>],
) -> CalibrationReport {
    assert_eq!(
        corpus.len(),
        trusty_results.len(),
        "corpus and trusty_results must have the same length"
    );

    let mut total_human: usize = 0;
    let mut total_recalled: usize = 0;
    let mut total_trusty: usize = 0;
    let mut rust_sem_total: usize = 0;
    let mut rust_sem_recalled: usize = 0;

    let mut per_pr: Vec<PrMetrics> = Vec::with_capacity(corpus.len());

    for (entry, findings) in corpus.iter().zip(trusty_results.iter()) {
        let human = &entry.human_findings;
        let n_human = human.len();
        let n_trusty = findings.len();

        let mut recalled = 0usize;
        let mut false_positives: Vec<FpFinding> = Vec::new();

        for tf in findings {
            let matched = human.iter().any(|hf| is_recalled(tf, hf));
            if matched {
                recalled += 1;
            } else {
                false_positives.push(FpFinding {
                    file: tf.file.clone(),
                    kind: tf.kind.clone(),
                    description: tf.description.clone(),
                });
            }
            if is_rust_semantic(tf) {
                rust_sem_total += 1;
                if matched {
                    rust_sem_recalled += 1;
                }
            }
        }

        let pr_recall = if n_human == 0 {
            1.0_f64
        } else {
            recalled as f64 / n_human as f64
        };
        let pr_precision = if n_trusty == 0 {
            1.0_f64
        } else {
            recalled as f64 / n_trusty as f64
        };

        total_human += n_human;
        total_recalled += recalled;
        total_trusty += n_trusty;

        per_pr.push(PrMetrics {
            pr: format!("{}/{}#{}", entry.owner, entry.repo, entry.pr),
            recall: pr_recall,
            precision: pr_precision,
            false_positives,
        });
    }

    let aggregate_recall = if total_human == 0 {
        1.0_f64
    } else {
        total_recalled as f64 / total_human as f64
    };
    let aggregate_precision = if total_trusty == 0 {
        1.0_f64
    } else {
        total_recalled as f64 / total_trusty as f64
    };
    let rust_semantic_fp_rate = if rust_sem_total == 0 {
        1.0_f64
    } else {
        rust_sem_recalled as f64 / rust_sem_total as f64
    };

    CalibrationReport {
        recall: aggregate_recall,
        precision: aggregate_precision,
        per_pr,
        rust_semantic_fp_rate,
    }
}

// ─── I/O helpers ─────────────────────────────────────────────────────────────

/// Load corpus entries from a JSONL file (one JSON object per non-empty line).
///
/// Why: JSONL makes the corpus easy to append to and diff in git; loading it
/// in one place keeps error messages consistent.
/// What: reads all lines, skips blanks, deserialises each as `CorpusEntry`.
/// Test: `load_corpus_roundtrip` in `calibrate_tests.rs`.
pub fn load_corpus(path: &Path) -> Result<Vec<CorpusEntry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading corpus file {}", path.display()))?;
    let mut entries = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: CorpusEntry = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing corpus line {} in {}", i + 1, path.display()))?;
        entries.push(entry);
    }
    Ok(entries)
}

// ─── Subcommand handler ───────────────────────────────────────────────────────

/// Execute the `calibrate` subcommand.
///
/// Why: provides a repeatable calibration harness so prompt changes and model
/// upgrades can be measured against a fixed corpus before they ship.
/// What: loads the corpus JSONL, synthesises dry-run trusty findings from the
/// corpus `acted_on` flags (no live LLM call — future iterations wire
/// `run_review`), computes recall/precision, and emits a JSON report.
/// Test: `cargo run -p trusty-review -- calibrate --corpus corpus.jsonl`.
pub async fn cmd_calibrate(args: CalibrateArgs) -> Result<()> {
    let corpus = load_corpus(&args.corpus)?;
    if corpus.is_empty() {
        anyhow::bail!("corpus file is empty — nothing to calibrate against");
    }

    // Dry-run mode: synthesise trusty findings from corpus `acted_on` flags.
    // This makes the harness runnable without a live LLM or GitHub connection.
    // A future iteration replaces this stub with an actual `run_review` call.
    let trusty_results: Vec<Vec<Finding>> = corpus
        .iter()
        .map(|entry| {
            entry
                .human_findings
                .iter()
                .filter(|hf| hf.acted_on)
                .map(|hf| {
                    Finding::new(
                        hf.file.clone(),
                        hf.kind.clone(),
                        format!("synthetic finding for {}", hf.file),
                        String::new(),
                        0.9_f32,
                        Effort::Low,
                    )
                })
                .collect()
        })
        .collect();

    let report = compute_metrics(&corpus, &trusty_results);
    let json = serde_json::to_string_pretty(&report).context("serialising calibration report")?;

    match &args.output {
        Some(path) => {
            std::fs::write(path, &json)
                .with_context(|| format!("writing report to {}", path.display()))?;
            eprintln!(
                "calibration report written to {} (recall={:.3}, precision={:.3})",
                path.display(),
                report.recall,
                report.precision
            );
        }
        None => {
            println!("{json}");
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "calibrate_tests.rs"]
mod tests;
