//! Inline `# Spec References` block parsing (DOC-38 §2.1–§2.4, §3).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-03~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft)
//!
//! Why: code and config declare linkage inline, grouped under a `# Spec
//! References` block introduced by a marker line (§2.3). References belong to
//! the nearest preceding marker within the same comment/docstring region, and
//! fenced code blocks are NEVER scanned (§2.3) — the standard's own examples
//! would otherwise self-trigger. Scoping to declared blocks is what stops a
//! resolver inventing linkage from a `SPEC-…` mention in unrelated prose (G4).
//! What: [`parse_inline_refs`] — a single line-oriented state machine that
//! tracks the comment/docstring region (via [`CommentSyntax`]), the open
//! `# Spec References` block, and fenced-code state, then applies the §2.2
//! reference regex within each open, non-fenced block. Records 1-based line
//! numbers so a linter can emit `file:line` diagnostics.
//! Test: `super::tests::inline_*`.

use super::Reference;
use super::comment::CommentSyntax;
use super::grammar::{is_unsafe_path, reference_regex};

/// True when a stripped line opens or closes a Markdown code fence (§2.3).
///
/// Why: fenced content must never be scanned for markers or references; the
/// spec's own body quotes many example blocks inside fences (DOC-38 is the acid
/// test). Detecting fences on the comment *content* (not the raw line) means a
/// Rust `/// ```` doc-fence is honoured too.
/// What: returns the fence character (`` ` `` or `~`) when `stripped` begins
/// with a run of three or more of it, else `None`.
/// Test: `super::tests::inline_skips_fenced`.
fn fence_char(stripped: &str) -> Option<char> {
    ['`', '~'].into_iter().find(|&ch| {
        let run: String = std::iter::repeat_n(ch, 3).collect();
        stripped.starts_with(&run)
    })
}

/// Extract the comment/docstring *content* of a line, updating docstring state.
///
/// Why: the marker phrase and reference grammar apply to comment content, not
/// raw source; docstring languages (Python `"""`, JSDoc `/* */`) carry
/// non-prefixed reference lines that are in scope only while the region is open.
/// What: returns `Some(content)` when the line is a line-comment or lies inside
/// an open docstring region (mutating `in_doc` as it opens/closes the region),
/// or `None` for a plain code line. A leading `*` (JSDoc continuation) is
/// stripped from docstring-interior lines.
/// Test: `super::tests::inline_python_docstring`, `inline_ts_jsdoc`.
fn comment_content(trimmed: &str, syntax: &CommentSyntax, in_doc: &mut bool) -> Option<String> {
    // A line comment is always content, regardless of docstring state.
    let line_comment = syntax.strip_line_comment(trimmed).map(str::to_string);

    let Some((open, close)) = syntax.block else {
        return line_comment;
    };

    if *in_doc {
        if trimmed.contains(close) {
            *in_doc = false;
        }
        let body = trimmed
            .strip_prefix('*')
            .map_or(trimmed, |s| s.strip_prefix(' ').unwrap_or(s));
        return Some(body.to_string());
    }

    if let Some(idx) = trimmed.find(open) {
        let after = &trimmed[idx + open.len()..];
        // Re-open unless the region also closes on this same line.
        *in_doc = !after.contains(close);
        return Some(after.to_string());
    }

    line_comment
}

/// Parse every inline `# Spec References` reference in a source file.
///
/// Why: a linter (and any resolver) must recover the exact set of declared
/// references — with line numbers — to resolve them and report unresolved ones
/// by `file:line`. Honouring only marker-scoped, non-fenced blocks is what keeps
/// the result to *declared* linkage (§2.3, §2.4). For a line-comment-only idiom
/// (shell/TOML/YAML's `#`), the comment lead-in strip removes the SAME `#` that
/// would otherwise mark a new heading, so a plain follow-on comment line (e.g.
/// prose that happens to mention another well-formed reference triple) is
/// indistinguishable from a continued declaration by heading-detection alone —
/// it must instead close the block by NOT looking like a block item (§2.3's
/// bullet convention: every real declaration line is blank or `-`-prefixed).
/// What: walks `source` line by line under `syntax`'s comment/docstring idiom.
/// A marker line (stripped content, minus leading `#`s, equal case-insensitively
/// to `Spec References`) opens a block; a nested heading or a non-comment line
/// closes it; fenced regions are skipped entirely. Within an open block, a line
/// stays part of the block (and is scanned by the §2.2 reference regex) only
/// while it is blank or `-`-prefixed after stripping the comment lead-in — the
/// first line that is neither closes the block WITHOUT being scanned, so trailing
/// prose in the same comment run can never masquerade as a declaration. Extracted
/// references drop any unsafe path (a `..` traversal segment or an absolute
/// path, §2.1's repo-root-relative canonical form). Duplicates are retained
/// (each keeps its own line) so a linter can flag each occurrence.
/// Test: `super::tests::inline_rust`, `inline_bare_shell`, `inline_skips_fenced`,
/// `inline_ignores_non_block_ref`, `inline_hash_block_closes_on_prose`.
#[must_use]
pub fn parse_inline_refs(source: &str, syntax: &CommentSyntax) -> Vec<Reference> {
    let mut refs = Vec::new();
    let mut in_doc = false;
    let mut in_fence: Option<char> = None;
    let mut block_open = false;

    for (idx, raw) in source.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw.trim_start();

        let Some(content) = comment_content(trimmed, syntax, &mut in_doc) else {
            // A plain code line ends any open block and cannot be inside a fence.
            block_open = false;
            continue;
        };
        let stripped = content.trim_start();

        // Fenced code (evaluated on comment content, so `/// ``` ` is caught).
        if let Some(open) = in_fence {
            if fence_char(stripped) == Some(open) {
                in_fence = None;
            }
            continue;
        }
        if let Some(ch) = fence_char(stripped) {
            in_fence = Some(ch);
            continue;
        }

        let heading = stripped.trim_start_matches('#').trim();
        if heading.eq_ignore_ascii_case("Spec References") {
            block_open = true;
            continue;
        }
        if stripped.starts_with('#') {
            // A different heading terminates the current block.
            block_open = false;
            continue;
        }
        if block_open {
            // A declaration line is blank (continuation whitespace within the
            // block, e.g. Rust's bare `//!` separator line) or `-`-prefixed
            // (the §2.3 bullet convention). The first line that is neither
            // closes the block instead of being scanned — this is what stops
            // trailing prose in a single-line-comment idiom (shell/TOML/YAML,
            // which has no nested-heading signal once the lead-in `#` is
            // stripped) from being swept in as a declaration.
            if !stripped.is_empty() && !stripped.starts_with('-') {
                block_open = false;
                continue;
            }
            for caps in reference_regex().captures_iter(&content) {
                let path = caps[2].to_string();
                if is_unsafe_path(&path) {
                    continue;
                }
                refs.push(Reference {
                    id: caps[1].to_string(),
                    path,
                    anchor: caps[3].to_string(),
                    line,
                });
            }
        }
    }
    refs
}
