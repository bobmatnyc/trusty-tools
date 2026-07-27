//! Deterministic citation verification for diff-provable findings (#2881).
//!
//! Why: a `code_provable: true` finding (or a `code:`-cited finding) drives the
//! deterministic BLOCK / REQUEST_CHANGES floor unconditionally
//! (`grade::drives_block_floor` = `cited || code_provable`).  The reviewer LLM can
//! confabulate such a finding — the #2881 repro emitted a high-severity
//! `code_provable: true` finding claiming a new vitest test file's content had been
//! "prepended" into a large regenerated bundle (`api/chat.js`), content that lives
//! in a DIFFERENT changed file and never appears in the cited file at all.  The
//! grade floor trusted the flag at face value and fail-CLOSED a clean PR to
//! D+/REQUEST_CHANGES.  Diff-hunk attribution through the parse → split pipeline is
//! correct (verified by the `mapreduce`/`diff_analyzer` tests); the missing layer
//! is a check that a diff-provable CLAIM is actually grounded in the cited file.
//!
//! What: [`DiffContentIndex`] indexes the surviving diff by file (the exact content
//! the reviewer saw, built from the post-filter [`FilteredDiff`]).
//! [`downgrade_uncitable_findings`] scans findings and, for each finding that claims
//! to be provable from the diff, verifies the cited file exists in the diff AND that
//! the concrete code fragments the finding quotes actually appear in that file.  A
//! finding that fails verification is DOWNGRADED (never silently dropped): its
//! `code_provable` flag is cleared, a `code:` citation to the unverifiable location
//! is removed, and its confidence is lowered to the refuted floor so it can no longer
//! force any verdict floor — it survives as an advisory note.  The check is
//! FAIL-OPEN: a finding that quotes nothing concrete, or whose quote is grounded in
//! the cited file, is left untouched.
//!
//! Test: `citation_check_tests.rs`.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use tracing::warn;

use crate::models::Finding;
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff};

/// The four inline bracket-citation forms the system prompt mandates
/// (`[code: … ]`, `[jira: … ]`, `[gh: … ]`, `[apex: … ]`).
///
/// Why: the reviewer prompt REQUIRES a finding to cite grounding context with one
/// of these bracket forms (see `assets/prompts/system_prompt_stock.md`), and the
/// `[code: `path:line` — "brief excerpt"]` form carries a `path:line` token plus a
/// deliberately-paraphrased excerpt — neither appears verbatim in diff content.
/// Treating those as verifiable quotes would FALSE-downgrade a correctly-cited
/// `code_provable` finding; stripping the whole bracket before quote extraction
/// keeps only the finding's standalone code quotes (raw diff lines) for grounding.
/// What: matches a bracket beginning with one of the four labels through its
/// closing `]`, case-insensitively.
/// Test: `extract_spans_skips_prompt_mandated_citation_grammar`,
/// `keeps_finding_using_only_bracket_citation_grammar`.
static BRACKET_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[(?:code|jira|gh|apex):[^\]]*\]")
        .expect("bracket-citation regex is a valid literal")
});

/// Confidence a downgraded finding is clamped to — mirrors the verifier-refuted
/// floor so a citation-refuted finding is treated as the noise it is by every
/// verdict floor (`grade::is_substantive`, `derive_verdict`'s low-confidence
/// override, and the synthesis floor's confidence gate).
const DOWNGRADED_CITATION_CONFIDENCE: f32 = 0.10;

/// Minimum normalized length for a quoted fragment to count as verifiable evidence.
///
/// Why: short quotes (a bare identifier, a single keyword) coincidentally appear in
/// many files and would drive false-positive downgrades; a finding must quote a
/// substantial fragment before we hold it to the "must appear in the cited file"
/// bar.  Below this length we FAIL-OPEN (leave the finding untouched).
const MIN_SPAN_LEN: usize = 12;

// ─── Diff content index ─────────────────────────────────────────────────────────

/// Per-file index of the surviving diff content, for grounding finding citations.
///
/// Why: verifying a finding's quoted content requires the exact per-file text the
/// reviewer saw.  Building it once from the [`FilteredDiff`] (the post-Stage-A/B
/// content that actually reached the prompt) and reusing it across all findings
/// keeps the check O(findings) rather than re-scanning the raw diff per finding.
/// What: `by_file` maps a normalized file path to that file's normalized diff
/// content (hunk line bodies, marker-stripped); `all` is every file's content
/// concatenated, used to detect content that belongs to a DIFFERENT changed file.
/// Test: `index_from_filtered_indexes_each_file`, `contains_is_whitespace_tolerant`.
pub struct DiffContentIndex {
    by_file: HashMap<String, String>,
    all: String,
}

impl DiffContentIndex {
    /// Build an index from a post-filter [`FilteredDiff`].
    ///
    /// Why: the `FilteredDiff` is the authoritative record of what the reviewer
    /// saw — `Kept` files carry their surviving hunk lines and `SummaryOnly` files
    /// their summary line; `Dropped` files never reached the prompt and are omitted
    /// so a finding citing one is correctly treated as ungrounded.
    /// What: for each surviving file, joins its hunk line bodies (or summary line)
    /// into one normalized string keyed by the normalized path; also builds the
    /// concatenated `all` blob.
    /// Test: `index_from_filtered_indexes_each_file`,
    /// `index_omits_dropped_files`.
    pub fn from_filtered(filtered: &FilteredDiff) -> Self {
        let mut by_file: HashMap<String, String> = HashMap::new();
        let mut all = String::new();

        for file in &filtered.files {
            let content = match file.disposition {
                FileDisposition::Kept => {
                    let mut buf = String::new();
                    for hunk in &file.hunks {
                        for line in &hunk.lines {
                            // Strip the unified-diff marker (`+`, `-`, or space) so
                            // the comparison is against code text, not diff framing.
                            let body = line.strip_prefix(['+', '-', ' ']).unwrap_or(line);
                            buf.push_str(body);
                            buf.push('\n');
                        }
                    }
                    buf
                }
                FileDisposition::SummaryOnly => file.summary_line.clone().unwrap_or_default(),
                FileDisposition::Dropped => continue,
            };
            let norm = normalize(&content);
            all.push_str(&norm);
            all.push(' ');
            by_file.insert(normalize_path(&file.filename), norm);
        }

        Self { by_file, all }
    }

    /// Look up the normalized content of the file a finding cites, if present.
    ///
    /// Why: findings may cite a path with an `a/`/`b/` prefix or by basename; the
    /// lookup normalizes and falls back to a UNIQUE basename match so trivial path
    /// spelling differences do not defeat grounding.
    /// What: exact normalized-path match first, else a basename match when exactly
    /// one indexed file shares that basename.
    /// Test: `lookup_matches_by_basename`, `lookup_ambiguous_basename_is_none`.
    fn lookup(&self, cited_path: &str) -> Option<&str> {
        let key = normalize_path(cited_path);
        if let Some(c) = self.by_file.get(&key) {
            return Some(c.as_str());
        }
        let base = basename(&key);
        let mut hit: Option<&str> = None;
        for (path, content) in &self.by_file {
            if basename(path) == base {
                if hit.is_some() {
                    return None; // ambiguous basename — refuse to guess
                }
                hit = Some(content.as_str());
            }
        }
        hit
    }
}

// ─── Public entry point ─────────────────────────────────────────────────────────

/// Downgrade findings whose diff-provable citation is not grounded in the cited
/// file, returning the number downgraded.
///
/// Why: this is the deterministic backstop the #2881 repro needed — the reviewer
/// LLM tagged a confabulated finding `code_provable: true`, and the grade floor
/// trusted it.  Verifying the citation against the actual diff before the floor
/// runs prevents a fabricated finding from ever forcing a verdict.
/// What: for each finding that claims diff-provability (`code_provable` or a
/// `code:` citation), verifies the cited file is in the diff and that at least one
/// substantial code fragment the finding quotes appears in that file.  A finding
/// that fails is downgraded via [`apply_downgrade`] (advisory, non-blocking) rather
/// than dropped.  FAIL-OPEN: findings with no verifiable quote, or whose quote is
/// grounded, are left exactly as-is.
/// Test: `downgrades_cross_file_misattribution`, `keeps_grounded_code_provable`,
/// `fail_open_when_cited_file_not_indexed`, `fail_open_when_no_quote`.
pub fn downgrade_uncitable_findings(findings: &mut [Finding], index: &DiffContentIndex) -> usize {
    let mut downgraded = 0usize;
    for f in findings.iter_mut() {
        if !is_diff_grounded(f) {
            continue;
        }
        let Some(reason) = uncitable_reason(f, index) else {
            continue;
        };
        warn!(
            file = %f.file,
            line = ?f.line,
            kind = %f.kind,
            reason,
            "citation-check: downgrading unverifiable diff-provable finding (#2881)"
        );
        apply_downgrade(f);
        downgraded += 1;
    }
    if downgraded > 0 {
        warn!(count = downgraded, "citation-check: findings downgraded");
    }
    downgraded
}

/// Decide whether a diff-grounded finding's citation is unverifiable, and why.
///
/// Why: isolates the decision from the mutation so it is unit-testable and the
/// FAIL-OPEN boundaries are explicit.
/// What: returns `Some(reason)` ONLY when the cited file IS present in the diff
/// AND the finding quotes ≥1 substantial fragment of which NONE appears in that
/// file (distinguishing content that belongs to another changed file from content
/// absent everywhere, for logging only).  Every other case FAILS OPEN and returns
/// `None`:
///  - the cited file is not indexed (outside the diff, or a header the parser
///    could not attribute) — we have no content to check against, so we never
///    penalise a finding merely for citing a file we did not index; and
///  - the finding quotes nothing concrete, or a quote is found in the cited file.
///
/// This keeps the check high-precision: a legitimate diff-provable finding is
/// never downgraded, and only a demonstrable content mismatch (the #2881 shape)
/// is caught.
///
/// Test: covered by the `downgrade_*` / `keeps_*` / `fail_open_*` tests.
fn uncitable_reason(f: &Finding, index: &DiffContentIndex) -> Option<&'static str> {
    // FAIL OPEN when the cited file is not indexed: without the file's actual
    // content there is nothing to verify against, and citing a file outside the
    // reviewed diff is not, by itself, proof of fabrication.
    let content = index.lookup(&f.file)?;

    let spans = extract_quoted_spans(f);
    if spans.is_empty() {
        return None; // nothing concrete to verify — fail open
    }
    if spans.iter().any(|s| content.contains(s.as_str())) {
        return None; // a quote is grounded in the cited file — keep
    }
    // None of the finding's substantial quotes appear in the cited file.
    let elsewhere = spans.iter().any(|s| index.all.contains(s.as_str()));
    Some(if elsewhere {
        "content-belongs-to-another-file"
    } else {
        "cited-content-absent-from-diff"
    })
}

/// Return `true` when a finding claims to be provable from the diff itself.
///
/// Why: only diff-provable claims are verifiable against the diff — a finding
/// grounded in an external spec/ticket (`jira:`/`gh:`/`apex:` citation) is out of
/// scope for this check and must be left untouched.
/// What: true iff `code_provable` is set OR the `source_citation` is a `code:`
/// location.
/// Test: `is_diff_grounded_detects_code_provable_and_code_citation`.
fn is_diff_grounded(f: &Finding) -> bool {
    f.code_provable || has_code_citation(f)
}

/// Return `true` when the finding carries a `code:`-prefixed source citation.
fn has_code_citation(f: &Finding) -> bool {
    f.source_citation
        .as_deref()
        .map(|c| c.trim_start().to_ascii_lowercase().starts_with("code:"))
        .unwrap_or(false)
}

/// Neutralize a finding whose diff citation could not be verified.
///
/// Why: an unverifiable diff-provable claim must never force a verdict floor, but
/// dropping it outright would hide a possible (unproven) concern from the author —
/// so we downgrade to advisory instead (honest partial signal, mirroring the
/// verifier-refuted treatment).
/// What: clears `code_provable`; removes ANY `source_citation` (not just a `code:`
/// one); and lowers confidence to the refuted floor so every verdict floor treats
/// it as non-evidence.  Stripping the whole citation is deliberate: a finding whose
/// diff-provable factual basis was just disproven cannot be trusted to have
/// correctly grounded a co-attached spec/ticket citation either, and leaving a
/// non-`code:` citation in place would keep `grade::is_escalation_eligible` true
/// (the citation-grammar shape check) — so `drives_block_floor` would still fire
/// and the downgrade would be a no-op (code-critic WARN, #2881).
/// Test: `apply_downgrade_clears_provability_and_all_citations`,
/// `downgrade_neutralizes_surviving_non_code_citation`.
fn apply_downgrade(f: &mut Finding) {
    f.code_provable = false;
    f.source_citation = None;
    f.confidence = f.confidence.min(DOWNGRADED_CITATION_CONFIDENCE);
}

// ─── Quote extraction + normalization ───────────────────────────────────────────

/// Extract substantial code fragments the finding explicitly quotes.
///
/// Why: a fabricated diff-provable finding "proves" its claim by quoting content it
/// asserts is present at the cited location; verifying those quotes against the
/// cited file is what catches the confabulation.  Only the model's OWN explicit
/// quotes are used (never paraphrase) to keep the check high-precision.
/// What: pulls backtick-, double-quote-, and single-quote-delimited spans from the
/// finding's `description` and `consequence` AFTER stripping the prompt-mandated
/// bracket citations (via [`BRACKET_CITATION_RE`]) so a `path:line` token or a
/// paraphrased excerpt inside a `[code: … ]` citation is never mistaken for a
/// verifiable diff quote.  Each span is normalized and kept only if at least
/// [`MIN_SPAN_LEN`] chars.  `suggested_replacement` is deliberately excluded — it
/// is the PROPOSED fix, which by design need not appear in the diff.
/// Test: `extract_spans_pulls_backtick_and_quotes`, `extract_spans_skips_short`,
/// `extract_spans_skips_prompt_mandated_citation_grammar`.
fn extract_quoted_spans(f: &Finding) -> Vec<String> {
    let mut out = Vec::new();
    for text in [f.description.as_str(), f.consequence.as_str()] {
        let stripped = BRACKET_CITATION_RE.replace_all(text, " ");
        collect_delimited(&stripped, '`', &mut out);
        collect_delimited(&stripped, '"', &mut out);
        collect_delimited(&stripped, '\'', &mut out);
    }
    out.retain(|s| s.len() >= MIN_SPAN_LEN);
    out
}

/// Collect normalized spans delimited by `delim` from `text` into `out`.
///
/// Why: backtick/quote pairs are the model's explicit "this exact text" markers;
/// scanning pairs keeps extraction simple and dependency-free.
/// What: walks `text` splitting on `delim`; every ODD segment (between a pair of
/// delimiters) is normalized and pushed. Unterminated trailing text is ignored.
/// Test: covered by `extract_spans_pulls_backtick_and_quotes`.
fn collect_delimited(text: &str, delim: char, out: &mut Vec<String>) {
    let mut inside = false;
    for segment in text.split(delim) {
        if inside {
            let norm = normalize(segment);
            if !norm.is_empty() {
                out.push(norm);
            }
        }
        inside = !inside;
    }
    // If `inside` ended true, the final segment was after an unmatched delimiter and
    // was correctly treated as outside (not pushed) by the loop's last iteration.
}

/// Normalize text for whitespace-tolerant substring comparison.
///
/// Why: the diff and a finding's quote of it can differ only in incidental
/// whitespace (indentation, wrapping); collapsing runs of whitespace to a single
/// space makes the comparison robust without being lossy about the code itself.
/// What: splits on ASCII/Unicode whitespace and rejoins with single spaces, trimmed.
/// Test: `contains_is_whitespace_tolerant`.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize a diff file path for matching (strip `a/`/`b/` prefixes, trim).
fn normalize_path(p: &str) -> String {
    let t = p.trim();
    t.strip_prefix("a/")
        .or_else(|| t.strip_prefix("b/"))
        .unwrap_or(t)
        .to_string()
}

/// Return the final path component of `p`.
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
#[path = "citation_check_tests.rs"]
mod tests;
