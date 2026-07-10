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

/// Every recognised section id, in the order they appear in the synthesis output.
pub const ALL_SECTION_IDS: &[&str] = &[EXECUTIVE_SUMMARY, TOP_RISKS, FINDING_ELABORATION];

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
        EXECUTIVE_SUMMARY => Some(
            "Write ONE deal-analytic paragraph synthesising the verified findings, \
             severity-weighted (RED findings first), tied to what an acquirer must act \
             on. Reference a coverage gap ONLY if one is genuinely named in the coverage \
             data above (a listed not-investigated dimension or a named failed batch) — \
             and name it specifically; never imply a gap that isn't documented there.",
        ),
        TOP_RISKS => Some(
            "List the most material RED/AMBER risks, most material first (at most 5 \
             rows), each with a qualitative cost/effort framing and the affected \
             application(s). Draw ONLY from the findings provided.",
        ),
        FINDING_ELABORATION => Some(
            "For each finding listed as requiring elaboration, write one concise \
             sentence each for description, business impact, and remediation framing, \
             tied to its cited evidence. Do NOT elaborate a finding whose evidence is \
             already verified elsewhere in the provided data — leave it out of this \
             list entirely; re-elaborating it wastes output budget and cannot improve \
             on already-verified prose.",
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
