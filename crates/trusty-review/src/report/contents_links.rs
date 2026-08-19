//! Executive-summary jump-list: deterministic intra-document anchor links
//! (#6004).
//!
//! Why: owner ruling 2026-08-18 — the executive summary becomes a navigable
//! entry point, carrying links to every section, and "no link to a section
//! that was omitted." The anchor set can only be known AFTER the full
//! document is rendered and polished (a section's heading can survive while
//! its body collapses to the omit-empty gap line, and a custom/vendor
//! template — e.g. `report-technical-dd-cast.md` — may not carry every
//! section this template does at all). So the link scaffold is built by a
//! deterministic POST-render scan of the actual `##` headings present,
//! never authored by the LLM: the guardrailed narrative text never has to
//! emit a URL.
//! What: [`set_contents_placeholder`] fills a stable sentinel scalar at
//! fill-time (so it survives `polish`'s comment-stripping and honesty-marker
//! rules exactly like any other real content); [`inject`] runs LAST in
//! `Reporter::render`, scans the finished markdown for every level-2 (`## `)
//! heading, and replaces the sentinel with a bullet list of markdown links
//! to each one it actually found — skipping the Executive Summary heading
//! itself, since the list lives inside that section.
//! Test: `contents_links_tests.rs`.

use super::fill::Scope;
use super::manifest::slugify;

/// The scalar name the template carries the jump-list placeholder under.
pub const PLACEHOLDER_FIELD: &str = "report_contents_block";

/// A sentinel unlikely to collide with any real content, filled at fill-time
/// and replaced post-polish once the final heading set is known.
///
/// Why: NUL is not valid inside any markdown a template author or the LLM
/// could produce (the numeric guardrail and JSON transport both exclude it),
/// so a literal-text collision is not a real risk.
const SENTINEL: &str = "\u{0}REPORT_CONTENTS_LINKS\u{0}";

/// The heading text this module never links to (it lives inside that
/// section, so a self-link is redundant).
const SELF_HEADING_NEEDLE: &str = "Executive Summary";

/// Set the jump-list placeholder scalar.
///
/// Why: called unconditionally from `reporter::build_scope` — the block is
/// tool structure, not data, so it is never honesty-marked or omitted.
/// What: sets [`PLACEHOLDER_FIELD`] to [`SENTINEL`].
pub fn set_contents_placeholder(root: &mut Scope) {
    root.set(PLACEHOLDER_FIELD, SENTINEL);
}

/// Replace the sentinel with the real jump-list, built from the headings
/// actually present in `rendered`.
///
/// Why: must run AFTER `polish` (so a collapsed-but-still-headed section is
/// linkable) and after every section-appending pass in `Reporter::render`
/// (mermaid, synthesis/benchmark status notes, investigation sections) so
/// every heading that ends up in the document is a candidate.
/// What: scans line-by-line for `## ` (level-2 only — deeper headings are
/// per-application/per-finding detail, not top-level sections) in document
/// order, skips [`SELF_HEADING_NEEDLE`], slugs each via [`slugify`] (the same
/// algorithm GitHub's own renderer produces for a heading of this shape:
/// lowercase, punctuation collapsed to `-`), de-duplicates a slug collision
/// with a numeric suffix, and replaces [`SENTINEL`] with one bullet per
/// heading found. No sentinel in `rendered` (a custom template that never
/// included the placeholder) leaves the text unchanged. Zero other headings
/// found removes the sentinel with no list. A fenced code block (``` … ```)
/// is tracked with the same `in_fence` toggle guard as
/// [`super::polish::collapse_recursive`] — every fenced line is skipped, so a
/// `## `-prefixed line inside a `raw_evidence` quote of source bytes is never
/// misread as a heading and never linked as a dangling anchor.
/// Test: `contents_links_tests::{links_every_top_level_heading,
/// never_links_a_heading_absent_from_the_document, noop_without_sentinel,
/// fenced_evidence_hash_line_is_not_mistaken_for_a_heading}`.
pub fn inject(rendered: &str) -> String {
    if !rendered.contains(SENTINEL) {
        return rendered.to_string();
    }

    let mut seen_slugs: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in rendered.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let Some(text) = trimmed.strip_prefix("## ") else {
            continue;
        };
        // Exclude a nested `### ` etc. that also starts with "## " by chance —
        // cannot happen (`### ` does not start with `## ` followed by a space
        // at the same position), but guard against a stray "##" with no space.
        if text.trim().is_empty() || text.contains(SELF_HEADING_NEEDLE) {
            continue;
        }
        let heading = text.trim();
        let mut slug = slugify(heading);
        let mut n = 2;
        while seen_slugs.contains(&slug) {
            slug = format!("{}-{n}", slugify(heading));
            n += 1;
        }
        seen_slugs.push(slug.clone());
        links.push(format!("- [{heading}](#{slug})"));
    }

    let block = if links.is_empty() {
        String::new()
    } else {
        format!("**Contents**\n\n{}", links.join("\n"))
    };
    rendered.replace(SENTINEL, &block)
}

#[cfg(test)]
#[path = "contents_links_tests.rs"]
mod tests;
