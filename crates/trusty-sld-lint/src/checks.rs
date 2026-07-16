//! The pure SLD check functions (DOC-38 §2–§4).
//!
//! Why: keeping the checks pure — `(&str content, …) -> Vec<Diagnostic>`, no I/O
//! — makes them directly unit-testable with fixtures and keeps all filesystem
//! access in the orchestration layer ([`crate::run`]). Each check maps to a
//! DOC-38 acceptance criterion and carries a stable id (also the allowlist key).
//! What: [`check_reference`] resolves one declared reference (path + anchor,
//! revision-tolerant); [`check_code_file`] / [`check_markdown_refs`] extract and
//! resolve every reference in a file; [`check_spec_doc`] applies the spec-document
//! conventions (header block, catalog row, anchor↔id) to opted-in (or, in strict
//! mode, all) specs.
//! Test: `super::tests::checks_*`.

use std::collections::HashSet;

use trusty_common::sld::{
    anchor_resolves, has_frontmatter_spec_refs, has_traversal, is_valid_spec_id,
    parse_frontmatter_refs, parse_inline_refs, spec_anchors, syntax_for_extension,
};

use crate::catalog::doc_number_of;
use crate::report::Diagnostic;

/// The required fields of a spec's bold-field header block (DOC-38 §4.2).
const REQUIRED_HEADER_FIELDS: &[&str] =
    &["Status", "Subsystem", "Owner", "Last-updated", "Spec ID"];

/// Resolve one declared spec reference: self-check, traversal, path, anchor.
///
/// Why: a reference resolves only when its target file exists and carries the
/// anchor (§2.1, §4.3); the anchor must also equal the id (§2.1 self-check).
/// Resolution is revision-tolerant so a `~v1` reference still resolves against a
/// `~v2` section — enforcing drift is a non-goal (§1.3).
/// What: emits `ref-anchor-mismatch` when `anchor != id`, `ref-traversal` when
/// the path escapes via `..`, `ref-path-missing` when `lookup(path)` is `None`,
/// and `ref-anchor-missing` when the target has no matching heading anchor. An
/// empty vec means the reference resolves cleanly.
/// Test: `super::tests::checks_reference_resolves`, `checks_reference_errors`.
pub fn check_reference(
    decl_path: &str,
    id: &str,
    path: &str,
    anchor: &str,
    line: usize,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if anchor != id {
        out.push(Diagnostic::error(
            decl_path,
            line,
            "ref-anchor-mismatch",
            format!(
                "reference anchor `{anchor}` must equal its id `{id}` (DOC-38 §2.1 self-check)"
            ),
        ));
    }
    if has_traversal(path) {
        out.push(Diagnostic::error(
            decl_path,
            line,
            "ref-traversal",
            format!("reference path `{path}` must not contain a `..` traversal segment"),
        ));
        return out;
    }
    match lookup(path) {
        None => out.push(Diagnostic::error(
            decl_path,
            line,
            "ref-path-missing",
            format!("referenced spec file `{path}` does not exist (repo-root-relative)"),
        )),
        Some(target) if !anchor_resolves(&target, anchor) => out.push(Diagnostic::error(
            decl_path,
            line,
            "ref-anchor-missing",
            format!("anchor `{anchor}` has no matching `{{#SPEC-…}}` heading in `{path}`"),
        )),
        Some(_) => {}
    }
    out
}

/// Extract and resolve every inline `# Spec References` reference in a code file.
///
/// Why: DOC-38 scopes inline linkage to non-Markdown source in its native
/// comment idiom (§3); resolving each declared reference is the always-on check
/// that keeps linkage honest everywhere.
/// What: looks up the file's [`syntax_for_extension`]; an unknown extension
/// yields nothing (never guess an idiom). Otherwise parses inline references and
/// resolves each via [`check_reference`].
/// Test: `super::tests::checks_code_file`.
pub fn check_code_file(
    decl_path: &str,
    content: &str,
    ext: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Vec<Diagnostic> {
    let Some(syntax) = syntax_for_extension(ext) else {
        return Vec::new();
    };
    parse_inline_refs(content, &syntax)
        .iter()
        .flat_map(|r| check_reference(decl_path, &r.id, &r.path, &r.anchor, r.line, lookup))
        .collect()
}

/// Validate and resolve a Markdown document's `spec_refs:` frontmatter.
///
/// Why: frontmatter is the canonical Markdown declaration form (§2.5); a schema
/// violation is itself a defect, and each valid entry must resolve.
/// What: on a schema error emits `frontmatter-schema` (file-scoped); otherwise
/// resolves each frontmatter reference via [`check_reference`].
/// Test: `super::tests::checks_markdown_refs`, `checks_markdown_bad_frontmatter`.
pub fn check_markdown_refs(
    decl_path: &str,
    content: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Vec<Diagnostic> {
    match parse_frontmatter_refs(content) {
        Ok(refs) => refs
            .iter()
            .flat_map(|r| check_reference(decl_path, &r.id, &r.path, &r.anchor, r.line, lookup))
            .collect(),
        Err(e) => vec![Diagnostic::error(
            decl_path,
            0,
            "frontmatter-schema",
            format!("invalid `spec_refs:` frontmatter: {e}"),
        )],
    }
}

/// Apply the spec-document conventions to a `docs/specs` Markdown file.
///
/// Why: DOC-38 §4 fixes how a spec is numbered, headered, and anchored. To honour
/// grandfathering (existing specs predate the frontmatter retrofit, §10 F5/F6),
/// these full checks run only on files that have **opted in** (carry `spec_refs:`
/// frontmatter) — unless `strict` forces them on every spec (the post-retrofit
/// mode).
/// What: for an opted-in (or strict) spec, checks the bold-field header block
/// (§4.2), the catalog-row requirement (§4.5), and every `{#SPEC-…}` anchor's
/// grammar + agreement with its section's declared `**ID:**` (§4.3). Returns no
/// diagnostics for a grandfathered file in non-strict mode.
/// Test: `super::tests::checks_spec_doc_opt_in`, `checks_spec_doc_strict`.
pub fn check_spec_doc(
    decl_path: &str,
    content: &str,
    catalog: &HashSet<u32>,
    strict: bool,
) -> Vec<Diagnostic> {
    if !strict && !has_frontmatter_spec_refs(content) {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.extend(check_header_block(decl_path, content));
    out.extend(check_catalog_row(decl_path, content, catalog));
    out.extend(check_anchors(decl_path, content));
    out
}

/// Check the bold-field header block carries every required field (§4.2).
///
/// Why: a spec's metadata block (Status/Subsystem/Owner/Last-updated/Spec ID) is
/// the human-facing contract header; a missing field is a documentation defect.
/// What: emits one `spec-header` diagnostic per required field absent as a
/// `**Field:**` (or `**Field**`) line.
/// Test: `super::tests::checks_header_block`.
fn check_header_block(decl_path: &str, content: &str) -> Vec<Diagnostic> {
    REQUIRED_HEADER_FIELDS
        .iter()
        .filter(|field| {
            !content.contains(&format!("**{field}:**"))
                && !content.contains(&format!("**{field}**"))
        })
        .map(|field| {
            Diagnostic::error(
                decl_path,
                0,
                "spec-header",
                format!("missing required header field `**{field}:**` (DOC-38 §4.2)"),
            )
        })
        .collect()
}

/// Check the spec's self-labeled `DOC-N` has a catalog row (§4.5).
///
/// Why: an uncataloged spec is a defect — a resolver/reader cannot discover it
/// from the catalog. A spec with no `DOC-N` label is out of scope for this check.
/// What: emits `spec-catalog` when the file's `DOC-N` is absent from the parsed
/// catalog set; no diagnostic when the number is cataloged or unlabeled.
/// Test: `super::tests::checks_catalog_row`.
fn check_catalog_row(decl_path: &str, content: &str, catalog: &HashSet<u32>) -> Vec<Diagnostic> {
    match doc_number_of(content) {
        Some(n) if !catalog.contains(&n) => vec![Diagnostic::error(
            decl_path,
            0,
            "spec-catalog",
            format!("DOC-{n} has no row in the spec catalog (docs/specs/README.md, DOC-38 §4.5)"),
        )],
        _ => Vec::new(),
    }
}

/// Check anchor grammar and anchor↔`**ID:**` agreement (§4.3).
///
/// Why: every governed section anchors its ID with `{#SPEC-…}` equal to that ID
/// (§4.3, AC-3); a malformed or mismatched anchor breaks resolution.
/// What: one pass over the document — each `{#SPEC-…}` anchor must satisfy the
/// id grammar (`spec-id-grammar`), and where the section then declares a
/// `**ID:** <id>` line before the next anchor, the anchor must equal it
/// (`anchor-id-mismatch`).
/// Test: `super::tests::checks_anchors`.
fn check_anchors(decl_path: &str, content: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let anchors = spec_anchors(content);
    for a in &anchors {
        if !is_valid_spec_id(&a.id) {
            out.push(Diagnostic::error(
                decl_path,
                a.line,
                "spec-id-grammar",
                format!(
                    "anchor `{}` is not a valid SPEC-…~rev id (DOC-38 §2.1)",
                    a.id
                ),
            ));
        }
    }
    // Pair each `**ID:** <id>` line with the nearest preceding anchor.
    let mut current: Option<&trusty_common::sld::HeadingAnchor> = None;
    let mut anchor_iter = anchors.iter().peekable();
    for (idx, line) in content.lines().enumerate() {
        let lineno = idx + 1;
        while anchor_iter.peek().is_some_and(|a| a.line <= lineno) {
            current = anchor_iter.next();
        }
        if let Some(id) = declared_id(line) {
            if let Some(anchor) = current {
                if anchor.id != id {
                    out.push(Diagnostic::error(
                        decl_path,
                        lineno,
                        "anchor-id-mismatch",
                        format!(
                            "section `**ID:** {id}` disagrees with its anchor `{{#{}}}` (DOC-38 §4.3)",
                            anchor.id
                        ),
                    ));
                }
                current = None; // consume: one ID per section
            }
        }
    }
    out
}

/// Extract a section's declared `**ID:** <SPEC-…>` value from a line.
///
/// Why: the anchor↔id agreement check keys on the visible `**ID:**` field a
/// governed section carries just under its heading.
/// What: returns the `SPEC-…` token following a `**ID:**` (or `**Spec ID:**`)
/// bold label on the line, with any surrounding backticks stripped (specs often
/// write `` **ID:** `SPEC-…` ``), else `None`.
/// Test: covered by `super::tests::checks_anchors`.
fn declared_id(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("**ID:**")
        .or_else(|| t.strip_prefix("**Spec ID:**"))?;
    rest.split_whitespace()
        .next()
        .map(|tok| tok.trim_matches('`').to_string())
}
