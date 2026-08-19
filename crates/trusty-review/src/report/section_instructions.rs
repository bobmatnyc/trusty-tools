//! Generic built-in section-instruction defaults for LLM synthesis (#2357).
//!
//! Why: every synthesized report section (executive summary, top risks, finding
//! elaboration) needs a baseline instruction the tool ships with out of the box,
//! which a template may override for its own methodology's voice, and which an
//! analyst brief may further steer (additively) at run time.  This is the SAME
//! three-tier layering the crate already uses for reviewer voice — stock rules →
//! `principles` addendum → an optional custom `voice` package (see `src/voice/`)
//! — applied to synthesis section instructions instead: a generic default →
//! an optional per-template override → an additive per-run analyst overlay.
//! Centralising the generic defaults here with a stable section-id lookup keeps
//! every consumer (the prompt builder, the template parser, tests) reading the
//! same source of truth for what a section id means and which ids are valid.
//! What: the three `SectionId`-like string constants; [`ALL_SECTION_IDS`] for
//! validating a template's `<!-- instruct:<id> ... -->` blocks;
//! [`is_valid_section_id`]; [`default_instruction`]; and [`resolve`], which
//! merges a template's parsed overrides onto the generic defaults into one
//! fully-populated map.
//! Test: `section_instructions_tests.rs` covers the default map, override
//! merge, and unknown-id rejection.

use std::collections::BTreeMap;

/// Section id: the executive-summary paragraph.
pub const EXECUTIVE_SUMMARY: &str = "executive_summary";
/// Section id: the top-risks table rows.
pub const TOP_RISKS: &str = "top_risks";
/// Section id: per-finding elaboration prose.
pub const FINDING_ELABORATION: &str = "finding_elaboration";
/// Section id: the Code Quality & Architecture narrative paragraph (#6004).
pub const CODE_QUALITY_SUMMARY: &str = "code_quality_summary";
/// Section id: the Security Posture narrative paragraph (#6004).
pub const SECURITY_SUMMARY: &str = "security_summary";
/// Section id: the Authorship & Key-Person Risk narrative paragraph
/// (#5453, #6004).
pub const AUTHORSHIP_SUMMARY: &str = "authorship_summary";

/// Every recognised section id, in the order they appear in the synthesis output.
pub const ALL_SECTION_IDS: &[&str] = &[
    EXECUTIVE_SUMMARY,
    TOP_RISKS,
    FINDING_ELABORATION,
    CODE_QUALITY_SUMMARY,
    SECURITY_SUMMARY,
    AUTHORSHIP_SUMMARY,
];

/// True when `id` names a recognised synthesized section.
///
/// Why: a template's `<!-- instruct:<id> ... -->` block may misspell or invent
/// a section id; validating against the fixed set is what lets the parser warn
/// and ignore instead of silently accepting a no-op override.
/// What: membership test against [`ALL_SECTION_IDS`].
/// Test: `section_instructions_tests::validates_known_ids`.
pub fn is_valid_section_id(id: &str) -> bool {
    ALL_SECTION_IDS.contains(&id)
}

/// The generic, shipped-default instruction for one section id.
///
/// Why: this is the tool's own baseline voice for each synthesized section —
/// what a bare `--synthesize` run (no template override) produces.
/// What: returns `Some(text)` for a recognised id, `None` otherwise.
/// Test: `section_instructions_tests::defaults_cover_all_sections`.
pub fn default_instruction(id: &str) -> Option<&'static str> {
    match id {
        // #6004: balanced/adversarial voice — the owner's characterization is
        // "acquirer-side, skeptical, strengths and risks presented evenhandedly
        // but risk hunted adversarially, never promotional." Applied to every
        // default below; a template's `instruct:` override may still replace it.
        EXECUTIVE_SUMMARY => Some(
            "Write ONE deal-analytic paragraph synthesising the verified findings, \
             severity-weighted (RED findings first), tied to what an acquirer must act \
             on. Write from the acquirer's side: skeptical and adversarial about risk — \
             hunt for what should worry a buyer — while still naming genuine strengths \
             the data supports, evenhandedly, never as promotional filler. Reference a \
             coverage gap ONLY if one is genuinely named in the coverage data above (a \
             listed not-investigated dimension or a named failed batch) — and name it \
             specifically; never imply a gap that isn't documented there.",
        ),
        TOP_RISKS => Some(
            "List the most material RED/AMBER risks, most material first (at most 5 \
             rows), each with a qualitative cost/effort framing and the affected \
             application(s). Draw ONLY from the findings provided. Hunt for risk \
             adversarially, from the acquirer's side — do not soften a finding to read \
             as minor when the underlying data does not support that.",
        ),
        FINDING_ELABORATION => Some(
            "For each finding listed as requiring elaboration, write one concise \
             sentence each for description, business impact, and remediation framing, \
             tied to its cited evidence. Do NOT elaborate a finding whose evidence is \
             already verified elsewhere in the provided data — leave it out of this \
             list entirely; re-elaborating it wastes output budget and cannot improve \
             on already-verified prose.",
        ),
        CODE_QUALITY_SUMMARY => Some(
            "Write ONE paragraph on code quality and architecture, grounded strictly in \
             the complexity distribution, code-smell/refactor findings, and LoC/tech-stack \
             data provided. Take the acquirer's skeptical side on maintainability risk \
             while naming genuine strengths the data supports, evenhandedly. Never invent \
             an architectural pattern or a metric not present in the data.",
        ),
        SECURITY_SUMMARY => Some(
            "Write ONE paragraph characterising the security posture strictly from the \
             lint-graded RED/AMBER findings provided. This is code-hygiene signal, NOT a \
             SAST/CVE/secrets/pen-test result — never write as though it were; state that \
             limitation if it changes how a risk should be read. Take the acquirer's \
             skeptical side while still crediting a genuinely clean signal where the data \
             supports it.",
        ),
        AUTHORSHIP_SUMMARY => Some(
            "Write ONE high-level paragraph on the codebase's health from an authorship \
             perspective, grounded strictly in the bus factor, ownership concentration, \
             single-author subsystems, and trailing-12-month trajectory data provided. This \
             is a narrative of development trajectory over the past year, not a data dump — \
             do not restate every number in the table; characterise the trend and what it \
             means for continuity risk. Take the acquirer's skeptical side on key-person \
             risk while crediting genuine strengths (e.g. a widening contributor base) the \
             data supports, evenhandedly.",
        ),
        _ => None,
    }
}

/// Merge a template's parsed `instruct:` overrides onto the generic defaults.
///
/// Why: the prompt builder always wants a fully-populated map (one entry per
/// section id) regardless of whether the active template overrode any of
/// them — this is the single place that resolves "template override, else
/// generic default" so callers never fall back ad hoc.
/// What: for each id in [`ALL_SECTION_IDS`], takes `overrides[id]` when present,
/// else [`default_instruction`].  An override map may be empty (no template
/// overrides) or partial (only some sections overridden).
/// Test: `section_instructions_tests::{resolve_uses_defaults_when_no_overrides,
/// resolve_partial_override_only_replaces_that_section}`.
pub fn resolve(overrides: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    ALL_SECTION_IDS
        .iter()
        .map(|&id| {
            let text = overrides
                .get(id)
                .cloned()
                .unwrap_or_else(|| default_instruction(id).unwrap_or_default().to_string());
            (id.to_string(), text)
        })
        .collect()
}

#[cfg(test)]
#[path = "section_instructions_tests.rs"]
mod tests;
