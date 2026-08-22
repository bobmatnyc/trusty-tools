//! Region discipline for LLM-authored investigation findings (#6082 lap 4).
//!
//! Why: trusty-analyze measures complexity over a REGION — an `impl` block with
//! no function name of its own — and #6082 lap 3 taught the hotspot renderer to
//! say so. The LLM half of the investigation never learned it. One finding read
//! "The copy_all_from function has cyclomatic complexity 89" while the analyze
//! data attributed that 89 to the enclosing impl block at
//! `crates/trusty-search/src/core/corpus/meta_ops.rs` lines 19–437, and
//! `copy_all_from` at line 359 is one method inside it. The number is real and
//! the file is real; the attribution is not.
//!
//! The same index answers a second defect in the same report: seven LLM findings
//! restated a hotspot entry that already sat in the same AMBER list, pair for
//! pair — same file, a line inside the region, and the region's own cyclomatic
//! number quoted back.
//!
//! What: [`RegionIndex`] re-derives each measured region from the findings the
//! analyze adapter already rendered. [`rescope_impl_claims`] rewrites a
//! misattributed claim onto the region it belongs to, or drops the finding when
//! the sentence is not one this module can rewrite. [`suppress_duplicates`] then
//! removes the LLM copy of a hotspot the deterministic entry already carries,
//! keeping the deterministic one — it has the structured fields.
//!
//! Test: `regions_tests.rs`.

use std::sync::OnceLock;

use regex::Regex;
use tracing::info;

use crate::report::analyze_findings::{IMPL_BLOCK_REMEDIATION_PREFIX, IMPL_BLOCK_TITLE};
use crate::report::metrics::AnalyzeMetrics;

use super::verify::VerifiedFinding;

/// One complexity region the analyze pass measured, as the rendered finding
/// states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Region {
    /// Repository-relative file the region lives in.
    pub(super) file: String,
    /// First line of the region.
    pub(super) start: u64,
    /// Last line of the region.
    pub(super) end: u64,
    /// The cyclomatic complexity measured over the whole region.
    pub(super) cyclomatic: u64,
}

impl Region {
    /// True when `line` falls inside this region.
    fn covers(&self, line: u64) -> bool {
        self.start <= line && line <= self.end
    }
}

/// Every measured region in one repository's analyze findings.
///
/// Why: both checks below ask the same question — "does the analyze data
/// attribute this number to a region rather than to a symbol?" — so they read
/// one index rather than two parsers that could disagree.
/// What: built from the rendered hotspot findings, which is the only place the
/// span and the score appear together. Empty when the run had no `--analyze`
/// metrics, and every check below is then inert.
/// Test: `regions_tests::an_impl_hotspot_is_indexed`.
#[derive(Debug, Default, Clone)]
pub(super) struct RegionIndex {
    regions: Vec<Region>,
}

impl RegionIndex {
    /// Re-derive the regions from one repository's analyze findings.
    ///
    /// Why/What: see the struct doc. A finding qualifies when it is the
    /// adapter's own whole-impl hotspot — [`IMPL_BLOCK_TITLE`] plus the
    /// [`IMPL_BLOCK_REMEDIATION_PREFIX`] line range — and its description
    /// states a cyclomatic score. Anything else is skipped, so an ordinary
    /// diagnostic never masquerades as a region.
    /// Test: `regions_tests::{an_impl_hotspot_is_indexed,
    /// a_named_function_hotspot_is_not_a_region}`.
    pub(super) fn from_metrics(metrics: Option<&AnalyzeMetrics>) -> Self {
        let mut regions = Vec::new();
        for f in metrics.iter().flat_map(|m| m.findings.iter()) {
            if f.title.trim() != IMPL_BLOCK_TITLE
                || !f.remediation.starts_with(IMPL_BLOCK_REMEDIATION_PREFIX)
            {
                continue;
            }
            let (Some((start, end)), Some(cyclomatic)) =
                (line_range(&f.remediation), cyclomatic_score(&f.description))
            else {
                continue;
            };
            regions.push(Region {
                file: strip_line_suffix(&f.component).to_string(),
                start,
                end,
                cyclomatic,
            });
        }
        RegionIndex { regions }
    }

    /// The region that owns `score` at `file:line`, if the analyze data
    /// attributes that score to a region covering that line.
    fn owner(&self, file: &str, line: u64, score: u64) -> Option<&Region> {
        self.regions
            .iter()
            .find(|r| r.file == file && r.cyclomatic == score && r.covers(line))
    }

    /// True when the index measured nothing.
    fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

/// `cyclomatic complexity N` stated anywhere in `text`.
fn cyclomatic_score(text: &str) -> Option<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)cyclomatic\s+complexity\s+(?:of\s+)?([0-9]+)").expect("valid score regex")
    })
    .captures(text)?
    .get(1)?
    .as_str()
    .parse()
    .ok()
}

/// The `(first, last)` line range a whole-impl remediation states.
///
/// The adapter writes it as `(lines 19–437)`; the dash is an en dash today and
/// was an ASCII hyphen in older daemon output, so both are accepted — the same
/// tolerance `analyze_findings::line_range` applies on the way in.
fn line_range(text: &str) -> Option<(u64, u64)> {
    let inner = text.split_once("(lines ")?.1.split_once(')')?.0;
    let (a, b) = inner
        .split_once('–')
        .or_else(|| inner.split_once('-'))
        .or_else(|| inner.split_once('—'))?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// A component or file reference without its trailing `:line`.
fn strip_line_suffix(component: &str) -> &str {
    match component.rsplit_once(':') {
        Some((path, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => path,
        _ => component,
    }
}

/// The claim shape this module can rewrite: `<name> function has cyclomatic
/// complexity <n>`, with or without backticks, `the`, `a`, and `of`.
fn attribution_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:the\s+)?`?([A-Za-z_][A-Za-z0-9_]{2,})`?\s+(?:function|method)\s+has\s+(?:an?\s+)?cyclomatic\s+complexity\s+(?:of\s+)?([0-9]+)",
        )
        .expect("valid attribution regex")
    })
}

/// Rescope every LLM finding that credits a region's score to one symbol.
///
/// Why: see the module doc. The number and the file survive the correction —
/// only the thing the number is said to describe changes — so a reader keeps a
/// real hotspot and loses a false claim about one method.
/// What: for each finding citing a score the index attributes to a region
/// covering the finding's line, rewrites `<name> function has cyclomatic
/// complexity <n>` to name the enclosing impl block and its span. A finding
/// whose claim does not match that shape is DROPPED with a named reason rather
/// than shipped with an attribution nothing could correct. Returns the number of
/// findings dropped; every rescope and every drop is logged.
/// Test: `regions_tests::{an_impl_score_is_rescoped_onto_its_region,
/// an_unrewritable_attribution_is_dropped,
/// a_score_outside_every_region_is_left_alone}`.
pub(super) fn rescope_impl_claims(
    findings: &mut Vec<VerifiedFinding>,
    index: &RegionIndex,
) -> usize {
    if index.is_empty() {
        return 0;
    }
    let mut dropped = 0usize;
    findings.retain_mut(|f| {
        let Some(line) = f.line else { return true };
        let Some(score) = cyclomatic_score(&f.description).or_else(|| cyclomatic_score(&f.title))
        else {
            return true;
        };
        let Some(region) = index.owner(&f.file, line, score) else {
            return true;
        };
        let Some(caps) = attribution_re().captures(&f.description) else {
            dropped += 1;
            info!(
                finding = %f.title,
                file = %f.file,
                score,
                region = format!("{}-{}", region.start, region.end),
                "investigation: dropped a finding — it credits a whole-region complexity score to \
                 a symbol, and the claim is not in a shape this crate can rescope"
            );
            return false;
        };
        let symbol = caps.get(1).map_or("", |m| m.as_str()).to_string();
        let replacement = format!(
            "the impl block enclosing `{symbol}` (lines {}–{}) has cyclomatic complexity {score}",
            region.start, region.end
        );
        f.description = f
            .description
            .replace(caps.get(0).map_or("", |m| m.as_str()), &replacement);
        info!(
            finding = %f.title,
            file = %f.file,
            symbol = %symbol,
            score,
            "investigation: rescoped a whole-region complexity score onto the region that carries it"
        );
        true
    });
    dropped
}

/// Drop each LLM finding that restates a hotspot entry already in the list.
///
/// Why: the graded report listed seven such pairs side by side in the same AMBER
/// band, each pair reading as two findings about one measurement. The
/// deterministic entry is the one kept: it carries the structured span, score
/// and smell fields, and the LLM copy carries a paraphrase of them.
/// What: a finding is a duplicate when its file matches a measured region, its
/// line falls inside that region, and it quotes that region's own cyclomatic
/// score. Returns how many were suppressed; each is logged with the region it
/// duplicates.
/// Test: `regions_tests::{a_restated_hotspot_is_suppressed,
/// a_finding_quoting_a_different_score_survives,
/// a_finding_outside_the_region_survives}`.
pub(super) fn suppress_duplicates(
    findings: &mut Vec<VerifiedFinding>,
    index: &RegionIndex,
) -> usize {
    if index.is_empty() {
        return 0;
    }
    let mut suppressed = 0usize;
    findings.retain(|f| {
        let Some(line) = f.line else { return true };
        let Some(score) = cyclomatic_score(&f.description).or_else(|| cyclomatic_score(&f.title))
        else {
            return true;
        };
        let Some(region) = index.owner(&f.file, line, score) else {
            return true;
        };
        suppressed += 1;
        info!(
            finding = %f.title,
            file = %f.file,
            line,
            score,
            region = format!("{}-{}", region.start, region.end),
            "investigation: suppressed a finding that restates a measured hotspot already in the \
             findings list — the deterministic entry carries the structured fields and is kept"
        );
        false
    });
    suppressed
}

#[cfg(test)]
#[path = "regions_tests.rs"]
mod tests;
