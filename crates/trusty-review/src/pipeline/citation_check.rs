//! Deterministic citation verification for every emitted finding (#2881, #4042).
//!
//! Why: a `code_provable: true` finding (or a `code:`-cited finding) drives the
//! deterministic BLOCK / REQUEST_CHANGES floor unconditionally
//! (`grade::drives_block_floor` = `cited || code_provable`).  The reviewer LLM
//! can confabulate such a finding — the #2881 repro emitted a high-severity
//! `code_provable: true` finding claiming a new vitest test file's content had
//! been "prepended" into a large regenerated bundle (`api/chat.js`), content
//! that lives in a DIFFERENT changed file and never appears in the cited file
//! at all.  The original (#2882) fix downgraded (never dropped) any finding
//! whose free-text quotes failed to ground in the cited file, but scoped the
//! check to `code_provable`/`code:`-cited findings only, and — the load-bearing
//! gap #4042 exploited — deliberately EXEMPTED the prompt-mandated
//! `[code: `path:line` — "excerpt"]` citation's own excerpt from verification,
//! on the theory that it was always a paraphrase.  #4042 showed the model often
//! puts its verbatim "evidence" for a fabrication INSIDE that exact bracket
//! (attributing four mutually exclusive contents to one file, `hotelPage.ts:207`
//! citing a file/line that never existed in the diff at all), so the one
//! citation form the check was built to verify was the one place it never
//! looked.
//!
//! What: rather than adding another downgrade layer on top of the confabulated
//! finding, this module makes the emit path structurally incapable of carrying
//! an unverifiable citation — every finding's citations are checked and, on
//! failure, the finding is DROPPED (not downgraded, not neutered-but-kept):
//!
//!  - [`DiffContentIndex::from_filtered`] indexes every file the diff mentions
//!    (Kept, SummaryOnly, AND Dropped/Stage-A-excluded) so path EXISTENCE can be
//!    checked regardless of disposition, with content available only for
//!    Kept/SummaryOnly files.
//!  - [`enforce_citation_integrity`] runs on EVERY finding (not just
//!    `code_provable`/cited ones — closing the #4042-named coverage gap) and
//!    drops a finding whose `file` does not resolve in the diff at all, whose
//!    free-text quotes contradict the cited file's real content, or whose
//!    `[code: …]` bracket citation — INCLUDING its excerpt, no longer exempted
//!    — is unresolvable or contradicted.
//!  - A cheap, low-false-positive INTERNAL CONSISTENCY check
//!    ([`detect_line_contradictions`]) drops any pair of findings that cite the
//!    SAME file+line with two mutually exclusive quoted contents — at least one
//!    must be invented, and the safest action is to drop both (#4042's own
//!    suggested direction) — even when the file's content is not independently
//!    indexable (SummaryOnly / Stage-A-dropped).
//!
//! FAIL-OPEN remains the default wherever there is genuinely nothing to verify:
//! a finding with no quoted content at all is left alone (only path existence is
//! checked); the `Finding::UNKNOWN_FILE_PLACEHOLDER` sentinel (a legitimately
//! file-less, general observation) is exempt from path resolution entirely.
//!
//! Test: `citation_check_tests.rs`; end-to-end regressions in
//! `tests/citation_2881_regression.rs` and `tests/citation_4042_regression.rs`.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use tracing::warn;

use crate::models::{Finding, UNKNOWN_FILE_PLACEHOLDER};
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff};

/// The five inline bracket-citation forms the system prompt mandates
/// (`[code: … ]`, `[jira: … ]`, `[gh: … ]`, `[apex: … ]`, `[confluence: … ]`).
///
/// Why: used to strip ALL bracket citations before the generic backtick/quote
/// scan, so a citation's `path:line` locator token is never mistaken for a
/// free-text code quote. `jira:`/`gh:`/`apex:`/`confluence:` citations ground
/// in context (ticket/PR/spec/wiki-page text) this module has no index for and
/// are left untouched (fail-open); `code:` citations are extracted and verified
/// SEPARATELY, before this strip runs (see [`CODE_CITATION_RE`]).
/// This list and the prompt's grammar section
/// (`assets/prompts/system_prompt_stock.md`) must stay in step — a form the
/// prompt permits but this regex omits is scanned as an ungrounded quote.
/// Test: `extract_spans_skips_prompt_mandated_citation_grammar`,
/// `extract_spans_skips_confluence_citation_form`.
static BRACKET_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    // #5022: `confluence:` is the fifth form — code-intelligence emits it.
    Regex::new(r"(?i)\[(?:code|jira|gh|apex|confluence):[^\]]*\]")
        .expect("bracket-citation regex is a valid literal")
});

/// The mandated `[code: `path:line` — "excerpt"]` citation form, captured
/// structurally (#4042).
///
/// Why: this is the ONE citation form the prompt requires the model to ground
/// in a real diff/repo location, and the #2882 fix exempted its excerpt from
/// verification entirely (assuming it was always a paraphrase). #4042 showed
/// the model routinely puts its strongest — and sometimes fabricated —
/// "evidence" here. Capturing the locator (group 1, backtick-delimited) and
/// the remainder of the bracket (group 2, where the quoted excerpt lives)
/// separately lets [`extract_citations`] verify the excerpt against the
/// locator's OWN path, independent of `f.file` (a citation may legitimately
/// point at a different file than the one the finding is primarily filed
/// against).
/// What: matches case-insensitively; group 1 is the backtick-delimited
/// `path[:line]` locator, group 2 is everything else in the bracket up to `]`.
/// Test: `code_citation_re_captures_path_line_and_excerpt`.
static CODE_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)\[code:\s*`([^`\]]+)`\s*[—-]+\s*([^\]]*)\]")
        .expect("code-citation regex is a valid literal")
});

/// Minimum normalized length for a quoted fragment to count as verifiable
/// evidence.
///
/// Why: short quotes (a bare identifier, a single keyword) coincidentally
/// appear in many files and would drive false-positive drops; a finding must
/// quote a substantial fragment before we hold it to the "must appear in the
/// cited file" bar. Below this length we FAIL-OPEN.
const MIN_SPAN_LEN: usize = 12;

// ─── Diff content index ─────────────────────────────────────────────────────────

/// Per-file index of the diff, for grounding finding citations (#4042: now
/// covers every disposition, not only Kept/SummaryOnly).
///
/// Why: verifying a finding's quoted content requires the exact per-file text
/// the reviewer saw; verifying a finding's cited PATH requires knowing every
/// file the diff actually touches, regardless of whether that file's content
/// reached the prompt. Building both from the [`FilteredDiff`] once and
/// reusing across all findings keeps the check O(findings).
/// What: `by_file` maps a normalized file path to that file's normalized
/// content — hunk line bodies for `Kept`, the summary line for `SummaryOnly`,
/// and the EMPTY string for a Stage-A-excluded file (`dropped_files`): the path
/// is real (part of the diff), but no content was ever shown to the model, so
/// any finding that quotes SPECIFIC content for it is verifiably wrong. `all`
/// is every file's content concatenated, used to distinguish "this quote
/// belongs to a DIFFERENT changed file" from "this quote is absent everywhere".
/// Test: `index_from_filtered_indexes_each_file`,
/// `index_indexes_dropped_files_with_empty_content` (#4042),
/// `contains_is_whitespace_tolerant`.
pub struct DiffContentIndex {
    by_file: HashMap<String, String>,
    all: String,
}

impl DiffContentIndex {
    /// Build an index from a post-filter [`FilteredDiff`].
    ///
    /// Why: the `FilteredDiff` is the authoritative record of what the diff
    /// contains — `Kept` files carry their surviving hunk lines and
    /// `SummaryOnly` files their summary line; `dropped_files` (Stage A
    /// exclusions — lockfiles, snapshots, generated code) never reached the
    /// prompt but ARE real files in the diff, so their paths must still
    /// resolve (#4042: a citation must be droppable for being fabricated, not
    /// merely for citing a file the noise filter removed).
    /// What: for each surviving file, joins its hunk line bodies (or summary
    /// line) into one normalized string keyed by the normalized path; for each
    /// Stage-A-dropped file, indexes an EMPTY string at its path. Also builds
    /// the concatenated `all` blob (surviving files only — a dropped file's
    /// absent content cannot possibly ground a cross-file misattribution).
    /// Test: `index_from_filtered_indexes_each_file`,
    /// `index_indexes_dropped_files_with_empty_content`.
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
                // Never populated in `filtered.files` (see `FilteredFile::disposition`'s
                // doc) — handled defensively as "path real, content unseen".
                FileDisposition::Dropped => String::new(),
            };
            let norm = normalize(&content);
            all.push_str(&norm);
            all.push(' ');
            by_file.insert(normalize_path(&file.filename), norm);
        }

        // Stage-A-excluded files: real paths in the diff, zero content shown to
        // the model. Indexing them (empty) lets path resolution succeed while
        // content verification still fails any finding that quotes them.
        for dropped in &filtered.dropped_files {
            by_file.entry(normalize_path(&dropped.path)).or_default();
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

/// Enforce citation integrity over every finding, DROPPING (never downgrading)
/// any finding whose citation cannot be verified, returning the number dropped
/// (#4042).
///
/// Why: the property this pipeline must guarantee is "every emitted finding
/// cites a path (and, where quoted, content) that verifiably exists in the
/// diff." #2882 approximated this with a downgrade-only check scoped to
/// `code_provable`/cited findings and exempting the mandated citation
/// grammar's own excerpt; #4042 is the direct consequence of both narrowings.
/// Dropping (rather than downgrading) the finding is the structurally simpler
/// fix Bob's simplify mandate asks for: a dropped finding cannot leak into the
/// rendered review OR the verdict floor by construction, with no second
/// "is this still escalation-eligible after neutering" reasoning needed
/// downstream.
/// What: runs in two passes:
///  1. [`detect_line_contradictions`] — a finding that cites the same
///     file+line as another finding, with mutually exclusive quoted content, is
///     dropped alongside its contradictor (internal-consistency backstop, no
///     index required).
///  2. Per-finding verification against `index` — a finding whose `file` does
///     not resolve in the diff at all (except the
///     [`UNKNOWN_FILE_PLACEHOLDER`] sentinel), whose free-text quotes
///     contradict the cited file's real content, or whose `[code: …]` bracket
///     citation is unresolvable or contradicted, is dropped.
///
/// FAIL-OPEN: a finding with no quoted content and a resolvable `file` is left
/// completely untouched.
///
/// Test: `drops_cross_file_misattribution`, `keeps_grounded_code_provable`,
/// `drops_citation_to_path_outside_diff` (#4042),
/// `drops_findings_with_line_contradiction` (#4042),
/// `fail_open_when_no_quote`.
pub fn enforce_citation_integrity(findings: &mut Vec<Finding>, index: &DiffContentIndex) -> usize {
    let contradictory = detect_line_contradictions(findings);

    let mut reasons: Vec<Option<&'static str>> = Vec::with_capacity(findings.len());
    for (i, f) in findings.iter().enumerate() {
        reasons.push(if contradictory.contains(&i) {
            Some(
                "mutual-contradiction: another finding cites the same file+line with \
                  incompatible content",
            )
        } else {
            unresolvable_reason(f, index)
        });
    }

    let mut dropped = 0usize;
    let mut kept = Vec::with_capacity(findings.len());
    for (f, reason) in std::mem::take(findings).into_iter().zip(reasons) {
        match reason {
            Some(reason) => {
                warn!(
                    file = %f.file,
                    line = ?f.line,
                    kind = %f.kind,
                    reason,
                    "citation-check: dropping unverifiable finding (#4042)"
                );
                dropped += 1;
            }
            None => kept.push(f),
        }
    }
    *findings = kept;

    if dropped > 0 {
        warn!(count = dropped, "citation-check: findings dropped");
    }
    dropped
}

/// Decide whether a single finding's citations are unresolvable, and why.
///
/// Why: isolates the decision from the mutation so it is unit-testable.
/// What: returns `Some(reason)` when:
///  - `f.file` is not the [`UNKNOWN_FILE_PLACEHOLDER`] sentinel AND does not
///    resolve in `index` at all (outside the diff entirely — the
///    `hotelPage.ts:207` #4042 shape); or
///  - `f.file` resolves AND the finding quotes ≥1 substantial free-text
///    fragment (backtick/quote spans OUTSIDE any bracket citation) of which
///    NONE appears in that file's content; or
///  - any `[code: …]` bracket citation's own path does not resolve in `index`,
///    or its excerpt does not appear in that path's content.
///
/// Every other case FAILS OPEN and returns `None` — most notably a finding
/// with a resolvable `file` and no verifiable quote at all.
///
/// Test: covered by the `drops_*` / `keeps_*` / `fail_open_*` tests.
fn unresolvable_reason(f: &Finding, index: &DiffContentIndex) -> Option<&'static str> {
    let extracted = extract_citations(f);

    if f.file != UNKNOWN_FILE_PLACEHOLDER {
        match index.lookup(&f.file) {
            None => return Some("cited file is not part of the diff at all"),
            Some(content) => {
                if !extracted.generic.is_empty()
                    && !extracted
                        .generic
                        .iter()
                        .any(|s| content.contains(s.as_str()))
                {
                    let elsewhere = extracted
                        .generic
                        .iter()
                        .any(|s| index.all.contains(s.as_str()));
                    return Some(if elsewhere {
                        "quoted content belongs to a different changed file"
                    } else {
                        "quoted content is absent from the diff entirely"
                    });
                }
            }
        }
    }

    for citation in &extracted.code_citations {
        match index.lookup(&citation.path) {
            None => return Some("[code: …] citation's path is not part of the diff at all"),
            Some(content) => {
                if !content.contains(citation.excerpt.as_str()) {
                    return Some("[code: …] citation's excerpt does not appear at the cited path");
                }
            }
        }
    }

    None
}

/// Detect pairs of findings that cite the SAME file+line with mutually
/// exclusive quoted content (#4042's "internal consistency" backstop).
///
/// Why: at most one of two findings that assert incompatible contents for the
/// SAME location can be right — this is provable without knowing the file's
/// real content, so it catches fabrications even when the cited file's content
/// was never indexed (`SummaryOnly` / Stage-A-dropped). Scoped to the SAME
/// line (not merely the same file) deliberately: two real, distinct findings
/// about DIFFERENT lines of one file are normal and must never be flagged as
/// contradictory — only the same locator with incompatible content is proof.
/// What: groups every qualifying quoted span (free-text spans keyed by
/// `(f.file, f.line)`; `[code: …]` excerpts keyed by their OWN
/// `(citation.path, citation.line)`) by locator. Within a group, any two
/// DISTINCT findings whose spans are neither a substring nor a superstring of
/// one another are marked contradictory (both findings, not just one — #4042:
/// "at least one had to be invented", and there is no deterministic way to
/// tell which).
/// Test: `drops_findings_with_line_contradiction`,
/// `distinct_lines_are_never_contradictory`.
fn detect_line_contradictions(findings: &[Finding]) -> std::collections::HashSet<usize> {
    let mut groups: HashMap<(String, u32), Vec<(usize, String)>> = HashMap::new();

    for (i, f) in findings.iter().enumerate() {
        let extracted = extract_citations(f);
        if let Some(line) = f.line {
            for span in &extracted.generic {
                groups
                    .entry((normalize_path(&f.file), line))
                    .or_default()
                    .push((i, span.clone()));
            }
        }
        for citation in &extracted.code_citations {
            if let Some(line) = citation.line {
                groups
                    .entry((normalize_path(&citation.path), line))
                    .or_default()
                    .push((i, citation.excerpt.clone()));
            }
        }
    }

    let mut contradictory = std::collections::HashSet::new();
    for entries in groups.into_values() {
        if entries.len() < 2 {
            continue;
        }
        for a in 0..entries.len() {
            for b in (a + 1)..entries.len() {
                let (idx_a, span_a) = &entries[a];
                let (idx_b, span_b) = &entries[b];
                if idx_a == idx_b {
                    continue; // same finding citing itself twice — not a conflict
                }
                if !span_a.contains(span_b.as_str()) && !span_b.contains(span_a.as_str()) {
                    contradictory.insert(*idx_a);
                    contradictory.insert(*idx_b);
                }
            }
        }
    }
    contradictory
}

// ─── Quote extraction + normalization ───────────────────────────────────────────

/// A single `[code: `path:line` — "excerpt"]` citation, extracted structurally.
struct CodeCitation {
    path: String,
    line: Option<u32>,
    excerpt: String,
}

/// The citations extracted from one finding's text.
struct ExtractedCitations {
    /// `[code: …]` citations, each carrying its OWN path/line (may differ from
    /// `f.file`/`f.line`).
    code_citations: Vec<CodeCitation>,
    /// Free-text backtick/quote spans OUTSIDE any bracket citation, checked
    /// against `f.file`'s content.
    generic: Vec<String>,
}

/// Extract every citation a finding carries — structured `[code: …]` brackets
/// (with their own path/line/excerpt) AND free-text quoted spans — from its
/// `description` and `consequence` (#4042).
///
/// Why: #2882 stripped `[code: …]` brackets wholesale before scanning for
/// quotes, on the theory the excerpt inside was always a paraphrase — #4042
/// showed that is where the model's fabricated "evidence" most often lives.
/// Extracting the bracket's locator (path/line) and excerpt SEPARATELY from
/// the general free-text scan lets each be verified against its OWN cited
/// path, while still preventing the locator token itself (`path.ts:42`, never
/// literally present in that file's content) from being misread as a
/// content quote.
/// What: for each `[code: `path:line` — "excerpt"]` match, splits the locator
/// into path + optional line and pulls the excerpt from the remainder via
/// quote-delimited scanning; then strips ALL FOUR bracket forms (`code`,
/// `jira`, `gh`, `apex`) and scans the remainder for backtick/double/single
/// quoted spans ≥ [`MIN_SPAN_LEN`] chars. `suggested_replacement` and
/// `suggestion` are deliberately excluded (as before #4042) — they are the
/// PROPOSED fix, which by design need not appear in the diff.
/// Test: `code_citation_re_captures_path_line_and_excerpt`,
/// `extract_spans_pulls_backtick_and_quotes`, `extract_spans_skips_short`.
fn extract_citations(f: &Finding) -> ExtractedCitations {
    let mut code_citations = Vec::new();
    let mut generic = Vec::new();

    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in CODE_CITATION_RE.captures_iter(text) {
            let locator = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
            let rest = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
            let (path, line) = split_locator(locator);
            let mut excerpts = Vec::new();
            collect_delimited(rest, '"', &mut excerpts);
            collect_delimited(rest, '\'', &mut excerpts);
            for excerpt in excerpts {
                if excerpt.len() >= MIN_SPAN_LEN {
                    code_citations.push(CodeCitation {
                        path: path.clone(),
                        line,
                        excerpt,
                    });
                }
            }
        }

        let stripped = BRACKET_CITATION_RE.replace_all(text, " ");
        collect_delimited(&stripped, '`', &mut generic);
        collect_delimited(&stripped, '"', &mut generic);
        collect_delimited(&stripped, '\'', &mut generic);
    }
    generic.retain(|s| s.len() >= MIN_SPAN_LEN);

    ExtractedCitations {
        code_citations,
        generic,
    }
}

/// Split a `[code: …]` locator into its path and optional trailing line
/// number.
///
/// What: if `locator` ends in `:<digits>`, returns `(path-without-suffix,
/// Some(n))`; otherwise returns `(locator, None)` unchanged (e.g. a spec file
/// cited without a line, `docs/specs/foo.md`).
/// Test: `split_locator_extracts_line`, `split_locator_no_line`.
fn split_locator(locator: &str) -> (String, Option<u32>) {
    if let Some((path, line)) = locator.rsplit_once(':')
        && let Ok(n) = line.trim().parse::<u32>()
    {
        return (path.trim().to_string(), Some(n));
    }
    (locator.to_string(), None)
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
