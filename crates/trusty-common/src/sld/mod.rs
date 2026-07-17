//! Language-agnostic Spec-Linked Documentation (SLD) reference grammar (DOC-38).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)
//! - [`SPEC-SLD-03~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft)
//!
//! Why: DOC-38 promotes SLD from a Rust-only rustdoc convention to a first-class,
//! implementation-neutral standard — one `(spec-ID, path, anchor)` reference
//! grammar declared inline in any language's comment idiom, or in `spec_refs:`
//! YAML frontmatter for Markdown. Providing that grammar once, in the crate the
//! existing resolver already lives in, means a documentation linter
//! (`trusty-sld-lint`, DOC-38 §10 F1) and `intent_source::spec_resolve` parse
//! ONE grammar (goal G1), and the resolver's `revision_of`/`base_id` primitives
//! are consolidated here rather than duplicated.
//!
//! What: the standard's reusable reader primitives, split one concept per file
//! to stay under the SLOC cap:
//! - [`grammar`] — the `SPEC-{SUBSYSTEM}-{NN}~{rev}` id grammar, the §2.2
//!   reference regex, and `revision_of`/`base_id` (the shared revision helpers).
//! - [`comment`] — the per-extension [`CommentSyntax`] table (§3, Annex A.2).
//! - [`inline`] — fenced-code-aware `# Spec References` block parsing (§2.1–§2.4).
//! - [`frontmatter`] — `spec_refs:` YAML parsing for Markdown (§2.5).
//! - [`anchor`] — `{#SPEC-…}` heading-anchor scanning + revision-tolerant
//!   resolution (§4.3).
//!
//! This module is a **reader** of declared links only — it never invents linkage
//! and treats absence as valid (declared-links-only, §1.2 G4). It does not walk
//! the filesystem; callers supply file contents.
//!
//! Test: `cargo test -p trusty-common --features sld` runs `mod tests` (grammar,
//! comment, inline, frontmatter, anchor), no I/O.

pub mod anchor;
pub mod comment;
pub mod frontmatter;
pub mod grammar;
pub mod inline;

#[cfg(test)]
mod tests;

/// One declared spec reference recovered from an inline block (§2.1–§2.4).
///
/// Why: a linter resolves each declared reference (path + anchor) and reports
/// unresolved ones by `file:line`; the recovered triple plus its line is exactly
/// that unit. The frontmatter form uses [`frontmatter::FrontmatterRef`] (which
/// adds an optional `note`); both share the same `(id, path, anchor)` shape,
/// reflecting the one-grammar-two-representations design (§2).
/// What: the `(id, path, anchor)` triple and the 1-based source line the
/// reference was declared on.
/// Test: `tests::inline_rust`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The spec id (`SPEC-…~rev`).
    pub id: String,
    /// The repo-root-relative `.md` path (canonical form, §2.1).
    pub path: String,
    /// The in-file anchor (equals `id` when well-formed, §2.1 self-check).
    pub anchor: String,
    /// 1-based source line the reference was declared on.
    pub line: usize,
}

// ── Public facade re-exports (callers import from `sld::*`) ───────────────────

pub use anchor::{HeadingAnchor, anchor_resolves, spec_anchors};
pub use comment::{CommentSyntax, syntax_for_extension};
pub use frontmatter::{
    FrontmatterError, FrontmatterRef, has_frontmatter_spec_refs, parse_frontmatter_refs,
};
pub use grammar::{base_id, is_unsafe_path, is_valid_spec_id, reference_regex, revision_of};
pub use inline::parse_inline_refs;
