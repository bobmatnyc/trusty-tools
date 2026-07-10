//! Compact findings digest for exec-summary/top-risks synthesis (#2357 follow-up).
//!
//! Why: a live-QA acceptance run found that once the wave-3 investigation pass
//! verified 26 real findings, the TOP-LEVEL `Synthesizer::synthesize()` call —
//! which writes the executive summary + top risks in the SAME request that
//! historically also asked for a full prose elaboration of every RED/AMBER
//! finding — hit its own output-token ceiling and failed closed, leaving the
//! Executive Summary and Top Risks blank atop an otherwise-strong findings
//! section. The structural fix mirrors wave-3's batching fix: bound the INPUT
//! (a compact per-finding digest — title/severity/dimension/file/one-line
//! description, NOT the evidence quote/business-impact/remediation prose that
//! already lives in the report body) and bound the OUTPUT (cap how many
//! findings are even eligible for context, and — separately — how many the
//! model is asked to fully elaborate, since a wave-3-verified finding's prose is
//! already authoritative and merged in later regardless of what this call says).
//! What: [`CompactFinding`] is the minimal per-finding record; [`gather_compact_findings`]
//! builds one per non-green `MetricFinding` across all repositories, enriching
//! `description` + `verified` from `model.investigation` when a matching
//! wave-3-verified finding exists; [`build_split`] separates and caps the
//! CONTEXT list (all findings, top [`CONTEXT_FINDINGS_CAP`] by severity, for
//! grounding the exec summary/top risks) from the ELABORATION-TARGETS list
//! (verified findings excluded entirely, top [`ELABORATION_TARGETS_CAP`] of the
//! rest); [`render_context_section`] / [`render_elaboration_section`] turn the
//! capped lists into prompt text with an honest tail note when truncated.
//! Test: `synthesize_digest_tests.rs` — truncation, cap + tail note, severity
//! ordering, verified-flag enrichment, and a 100-finding prompt-size bound.

use crate::report::metrics::Severity;
use crate::report::model::ReportModel;

/// Max findings shown in the compact CONTEXT digest (grounds the exec summary
/// and top risks; every RED/AMBER finding is eligible, verified or not).
///
/// Why: bounds prompt INPUT size roughly linearly — at ~200 bytes/finding, 40
/// findings is ~8 KB, structurally safe input regardless of how many findings a
/// large repository's investigation verifies.
pub const CONTEXT_FINDINGS_CAP: usize = 40;

/// Max findings the model is asked to fully ELABORATE (the `findings` output
/// array): a bounded, genuinely-expensive OUTPUT task (several prose fields
/// per finding).  Wave-3-verified findings are excluded from this set
/// entirely — their evidence-grounded prose is already authoritative and is
/// merged onto the report regardless of what this call produces, so asking the
/// model to re-elaborate them would only spend output budget on discarded text
/// (see [`super::synthesize::apply_guardrail`]'s caller, which layers
/// investigation prose over synthesis prose, investigation always winning).
pub const ELABORATION_TARGETS_CAP: usize = 10;

/// Max characters kept from a finding's one-line description in the compact
/// digest before truncating with an ellipsis.
pub const DESCRIPTION_TRUNCATE_CHARS: usize = 140;

/// One finding reduced to exactly what exec-summary/top-risks synthesis needs
/// to reference it — never the full prose (evidence quote, business impact,
/// remediation), which already lives in the report body once verified.
#[derive(Debug, Clone)]
pub struct CompactFinding {
    /// The owning application's display name.
    pub app_name: String,
    /// The owning application's slug (for elaboration routing).
    pub app_slug: String,
    /// Finding title.
    pub title: String,
    /// Severity band (RED/AMBER only — greens are excluded upstream).
    pub severity: Severity,
    /// DD dimension / category.
    pub dimension: String,
    /// Affected file/component (from investigation's `file:line` when
    /// available, else the metric's bare `component`).
    pub file: String,
    /// A truncated one-line description, or empty when none is available
    /// (a bare `MetricFinding` with no investigation counterpart has none).
    pub description: String,
    /// True when a wave-3-verified counterpart exists for this finding — its
    /// evidence-grounded prose is already authoritative and must not be
    /// re-requested from the top-level synthesis call.
    pub verified: bool,
}

impl CompactFinding {
    /// Sort key: RED before AMBER (greens never reach this struct).
    fn severity_rank(&self) -> u8 {
        match self.severity {
            Severity::Red => 0,
            Severity::Amber => 1,
            Severity::Green => 2,
        }
    }

    /// The severity token used in prompt bullets.
    fn severity_label(&self) -> &'static str {
        match self.severity {
            Severity::Red => "RED",
            Severity::Amber => "AMBER",
            Severity::Green => "GREEN",
        }
    }
}

/// Truncate `s` to at most `max_chars` characters (char-boundary safe),
/// appending `…` when truncation occurred; returns `s` unchanged otherwise.
///
/// Why: a description field pulled from a verified finding could itself be a
/// full sentence or more; capping it is part of what keeps the digest input
/// bounded regardless of how verbose the source prose is.
/// What: counts `char`s (not bytes) so multi-byte UTF-8 is never split.
/// Test: `synthesize_digest_tests::{truncates_long_description,
/// short_description_unchanged}`.
pub fn truncate_description(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

/// Gather every RED/AMBER finding across all repositories into compact form.
///
/// Why: the single place that reduces rich verified-finding data (or a bare
/// historical `MetricFinding`) down to the minimal fields the exec-summary/
/// top-risks prompt needs — never the full prose.
/// What: iterates each repository's `metrics.findings`, skipping greens (the
/// structural no-green rule, unchanged); for each, looks up a same-title
/// [`crate::report::investigate::VerifiedFinding`] in
/// `model.investigation.repos[slug]` to enrich `description` (truncated to
/// [`DESCRIPTION_TRUNCATE_CHARS`]) and `file`; `verified` is true iff that
/// lookup succeeded.  A finding with no investigation counterpart carries an
/// empty `description` and the metric's bare `component` as `file` — unchanged
/// from the pre-#2357-follow-up behaviour for non-investigated runs.
/// Test: `synthesize_digest_tests::{enriches_from_investigation,
/// falls_back_without_investigation, excludes_greens}`.
pub fn gather_compact_findings(model: &ReportModel) -> Vec<CompactFinding> {
    let mut out = Vec::new();
    for repo in &model.repositories {
        let Some(metrics) = &repo.metrics else {
            continue;
        };
        let verified_repo = model
            .investigation
            .as_ref()
            .and_then(|inv| inv.repos.iter().find(|r| r.slug == repo.slug));

        for f in &metrics.findings {
            if f.severity == Severity::Green {
                continue;
            }
            let verified_match = verified_repo.and_then(|r| {
                r.findings
                    .iter()
                    .find(|v| v.title == f.title && !v.file.is_empty())
            });
            let (description, file, verified) = match verified_match {
                Some(v) => (
                    truncate_description(&v.description, DESCRIPTION_TRUNCATE_CHARS),
                    match v.line {
                        Some(line) => format!("{}:{line}", v.file),
                        None => v.file.clone(),
                    },
                    true,
                ),
                None => (String::new(), f.component.clone(), false),
            };
            out.push(CompactFinding {
                app_name: repo.name.clone(),
                app_slug: repo.slug.clone(),
                title: f.title.clone(),
                severity: f.severity,
                dimension: f.category.clone(),
                file,
                description,
                verified,
            });
        }
    }
    out
}

/// The two capped, prompt-ready subsets of a gathered finding list, plus each
/// subset's overflow count for an honest tail note.
#[derive(Debug, Default)]
pub struct DigestSplit {
    /// Top [`CONTEXT_FINDINGS_CAP`] findings (severity-first), for grounding.
    pub context: Vec<CompactFinding>,
    /// Findings cut from the context list by the cap.
    pub context_overflow: usize,
    /// Top [`ELABORATION_TARGETS_CAP`] UNVERIFIED findings, for elaboration.
    pub elaboration_targets: Vec<CompactFinding>,
    /// Unverified findings cut from the elaboration list by the cap.
    pub elaboration_overflow: usize,
}

/// Build the capped context + elaboration-target splits from a gathered list.
///
/// Why: this is the single place both output-size bounds are enforced —
/// CONTEXT bounds prompt input size; ELABORATION bounds the genuinely
/// expensive per-finding prose the model is asked to produce, and structurally
/// excludes anything wave-3 already verified.
/// What: sorts a clone of `all` by severity (RED first, stable so same-severity
/// order is preserved) for context; takes the top [`CONTEXT_FINDINGS_CAP`] and
/// records the remainder as `context_overflow`.  Filters to `!verified`, sorts
/// the same way, takes the top [`ELABORATION_TARGETS_CAP`] for
/// `elaboration_targets`, recording `elaboration_overflow`.
/// Test: `synthesize_digest_tests::{caps_context_at_40_red_first,
/// elaboration_targets_exclude_verified_and_cap_at_10}`.
pub fn build_split(all: &[CompactFinding]) -> DigestSplit {
    let mut by_severity: Vec<CompactFinding> = all.to_vec();
    by_severity.sort_by_key(CompactFinding::severity_rank);

    let context_overflow = by_severity.len().saturating_sub(CONTEXT_FINDINGS_CAP);
    let context: Vec<CompactFinding> = by_severity
        .iter()
        .take(CONTEXT_FINDINGS_CAP)
        .cloned()
        .collect();

    let mut unverified: Vec<CompactFinding> =
        by_severity.into_iter().filter(|f| !f.verified).collect();
    unverified.sort_by_key(CompactFinding::severity_rank);
    let elaboration_overflow = unverified.len().saturating_sub(ELABORATION_TARGETS_CAP);
    let elaboration_targets: Vec<CompactFinding> = unverified
        .into_iter()
        .take(ELABORATION_TARGETS_CAP)
        .collect();

    DigestSplit {
        context,
        context_overflow,
        elaboration_targets,
        elaboration_overflow,
    }
}

/// Render the compact CONTEXT findings section (grounds exec summary/top risks).
///
/// Why: this is the ONLY findings section the compact digest sends for
/// exec-summary/top-risks grounding — one bullet per finding, no full prose.
/// What: one line per capped finding (`[SEV] Title — App; dimension: …; file:
/// …`, plus its truncated description on a continuation line when present, and
/// a `[verified]` tag when investigation already produced grounded prose); a
/// trailing note names how many additional lower-severity findings exist when
/// the cap truncated the list.
/// Test: `synthesize_digest_tests::renders_context_with_tail_note`.
pub fn render_context_section(split: &DigestSplit) -> String {
    let mut out = String::with_capacity(split.context.len() * 160 + 64);
    out.push_str(&format!(
        "## Findings — compact digest ({} of {} shown, RED first; context for the executive summary and top risks ONLY, not for re-elaboration)\n\n",
        split.context.len(),
        split.context.len() + split.context_overflow,
    ));
    if split.context.is_empty() {
        out.push_str("- none\n");
    }
    for f in &split.context {
        let verified_tag = if f.verified { " [verified]" } else { "" };
        out.push_str(&format!(
            "- [{}] {}{verified_tag} — {}; dimension: {}; file: {}\n",
            f.severity_label(),
            f.title,
            f.app_name,
            f.dimension,
            f.file,
        ));
        if !f.description.is_empty() {
            out.push_str(&format!("  {}\n", f.description));
        }
    }
    if split.context_overflow > 0 {
        out.push_str(&format!(
            "- … and {} additional lower-severity finding(s) exist (see report body); reference the tail honestly if relevant, without inventing specifics for them.\n",
            split.context_overflow
        ));
    }
    out
}

/// Render the ELABORATION-targets section (what the model must prose-elaborate).
///
/// Why: separating this from the context list is what makes the no-redundant-
/// elaboration rule structural — a `[verified]`-tagged finding above never
/// appears here, so the model has no textual invitation to re-elaborate it.
/// What: one bullet per target (title/severity/dimension/app/file only — the
/// model supplies the prose); an explicit "none" line when investigation
/// already verified everything (the common case for a wave-3 run); a tail note
/// when the unverified set itself exceeded the cap.
/// Test: `synthesize_digest_tests::renders_elaboration_targets_or_none`.
pub fn render_elaboration_section(split: &DigestSplit) -> String {
    let mut out = String::from(
        "\n## Findings requiring elaboration (populate `findings` for EXACTLY these — leave verified findings above out entirely)\n\n",
    );
    if split.elaboration_targets.is_empty() {
        out.push_str(
            "- none — every RED/AMBER finding already has verified evidence-grounded prose elsewhere in this report; return an empty `findings` array.\n",
        );
        return out;
    }
    for f in &split.elaboration_targets {
        out.push_str(&format!(
            "- [{}] {} — {} (slug: {}); dimension: {}; file: {}\n",
            f.severity_label(),
            f.title,
            f.app_name,
            f.app_slug,
            f.dimension,
            f.file,
        ));
    }
    if split.elaboration_overflow > 0 {
        out.push_str(&format!(
            "- … and {} additional unverified finding(s) also need elaboration eventually; elaborate the highest-severity ones above first.\n",
            split.elaboration_overflow
        ));
    }
    out
}

#[cfg(test)]
#[path = "synthesize_digest_tests.rs"]
mod tests;
