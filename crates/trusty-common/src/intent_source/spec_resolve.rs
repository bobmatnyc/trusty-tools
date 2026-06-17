//! SLD spec-resolution: changed file → governing spec section.
//!
//! Why: when a ticket is silent, the intent may live in a spec the changed
//! code links to. Per SLD (`spec-linked-docs`), Rust code declares that linkage
//! in **rustdoc** (`# Spec References` blocks), not free comments. The ISR
//! resolves those declared links to a `SpecRef` + extracts the spec's
//! prescribed method (spec §6.4). **The ISR does not invent linkage** — a file
//! with no SLD ref yields no spec method (a gap on the spec axis).
//! What: `parse_spec_refs` (find `SPEC-…` ids + anchors in changed-file source)
//! and `extract_spec_method` (lift the governed section's Behavior-Contract /
//! Rationale prose from the spec markdown).
//! Test: `super::tests::spec_resolve_*` (AC-6).
//!
//! Scope note (C1 vs C4): this is the **minimal correct** reader required by
//! C1. A deeper, more robust resolver — symbol-level granularity, multi-anchor
//! reconciliation, revision-drift enforcement — is C4 (#1361). This module is
//! the seam C4 hardens; its public surface is intentionally narrow and stable.

use std::sync::OnceLock;

use regex::Regex;

use super::types::{Method, MethodKind, SpecRef};

/// Parse SLD spec references out of a changed file's source text.
///
/// Why: the ISR must find the `SPEC-{SUBSYSTEM}-{NN}~v{rev}` ids a file
/// declares (in `//!`/`///` `# Spec References` rustdoc) and the
/// `docs/specs/{file}.md#SPEC-…` link target they point at (spec §6.4).
/// What: scans for the SLD link form
/// ``[`SPEC-X-NN~vR`](docs/specs/file.md#SPEC-X-NN~vR)`` and returns one
/// `SpecRef` per distinct id, in first-seen order. Returns an empty vec when
/// the file declares no SLD reference (the ISR never invents linkage).
/// Test: `super::tests::spec_resolve_parse_*` (AC-6).
#[must_use]
pub fn parse_spec_refs(source: &str) -> Vec<SpecRef> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let re = LINK.get_or_init(|| {
        // [`SPEC-X-NN~vR`](docs/specs/file.md#anchor)
        // - group 1: spec id (inside backticks)
        // - group 2: docs/specs file path
        // - group 3: in-file anchor
        Regex::new(
            r"\[`(SPEC-[A-Z0-9]+-\d+~[A-Za-z0-9]+)`\]\((docs/specs/[^)#]+)#([A-Za-z0-9~\-]+)\)",
        )
        .expect("SLD link pattern compiles")
    });

    let mut refs: Vec<SpecRef> = Vec::new();
    for caps in re.captures_iter(source) {
        let spec_id = caps[1].to_string();
        let file = caps[2].to_string();
        let anchor = caps[3].to_string();
        // Defence in depth (the canonicalization guard in `FsSpecLookup::load`
        // is the primary control): reject any captured path containing a `..`
        // traversal segment so a malicious source file cannot point linkage
        // outside `docs/specs/`. The `regex` crate has no look-around, so this
        // is a post-match filter rather than a pattern exclusion.
        if file.split('/').any(|seg| seg == "..") {
            continue;
        }
        // De-duplicate on the spec id (first-seen wins) to keep the result
        // deterministic when the same ref appears in both module- and
        // function-level rustdoc.
        if refs.iter().any(|r| r.spec_id == spec_id) {
            continue;
        }
        refs.push(SpecRef {
            spec_id,
            file,
            anchor,
        });
    }
    refs
}

/// Extract the spec-prescribed method from a spec markdown document.
///
/// Why: once a `SpecRef` is resolved, the spec's *method* is the prescribed
/// approach/constraint stated in the governed section's Behavior-Contract /
/// Rationale prose (spec §6.2, §6.4). This is the spec-axis input to the
/// precedence rule.
/// What: locates the section whose heading carries the `{#anchor}` marker,
/// captures that section's body (until the next `##`/`---`), and runs the same
/// conservative heuristic the ticket path uses
/// ([`super::extract::heuristic_method`]). Returns `None` when the anchor is
/// absent or the section prose prescribes no method.
/// Test: `super::tests::spec_resolve_method_*`.
#[must_use]
pub fn extract_spec_method(spec_markdown: &str, anchor: &str) -> Option<Method> {
    let section = section_body(spec_markdown, anchor)?;
    super::extract::heuristic_method(&section).map(|mut m| {
        // Spec-sourced methods are advisory context; tag the kind but keep the
        // verbatim excerpt the heuristic captured.
        if matches!(m.kind, MethodKind::Unspecified) {
            m.kind = MethodKind::Approach;
        }
        m
    })
}

/// Return the body text of the spec section bearing `{#anchor}`.
///
/// Why: method extraction must be scoped to the *governed* section, not the
/// whole document, so an unrelated method elsewhere in the spec is not
/// attributed to this change (spec §6.4).
/// What: finds the heading line containing `{#anchor}` and returns the text
/// from there up to (but excluding) the next `## ` or top-level `# ` heading,
/// or a `---` horizontal rule. Returns `None` when no heading carries the
/// anchor.
/// Test: `super::tests::spec_resolve_section_*`.
fn section_body(markdown: &str, anchor: &str) -> Option<String> {
    let marker = format!("{{#{anchor}}}");
    let lines: Vec<&str> = markdown.lines().collect();
    let start = lines.iter().position(|l| l.contains(&marker))?;

    let mut body = String::new();
    for line in &lines[start + 1..] {
        let trimmed = line.trim_start();
        // A new `## ` subsection, a top-level `# ` section, or a `---` rule
        // terminates the current section. Breaking on `# ` too prevents a
        // following top-level section's prose from bleeding into this anchor's
        // method extraction.
        if trimmed.starts_with("## ") || trimmed.starts_with("# ") || trimmed == "---" {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    Some(body)
}
