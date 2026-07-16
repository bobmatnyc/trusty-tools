//! The canonical SLD spec-reference grammar (DOC-38 §2.1–§2.2).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)
//!
//! Why: DOC-38 fixes ONE reference grammar for every language — a
//! `(spec-ID, path, anchor)` triple with a self-checking anchor. Centralising
//! the `SPEC-{SUBSYSTEM}-{NN}~{rev}` id pattern and the revision helpers here
//! means the resolver (`intent_source::spec_resolve`) and any linter parse the
//! same grammar instead of drifting apart (§2.4 rationale, goal G1).
//! What: the compiled id/reference regexes (§2.1–§2.2), `is_valid_spec_id`, and
//! the revision splitters `revision_of` / `base_id` — the single source both
//! this module and `spec_resolve` share.
//! Test: `super::tests::grammar_*`.

use std::sync::OnceLock;

use regex::Regex;

/// The SLD spec-ID pattern body, shared by the id and reference regexes.
///
/// Why: DOC-38 §2.1 fixes the id shape once; expressing it as one string keeps
/// the standalone-id matcher and the three-capture reference matcher provably
/// consistent (they cannot drift to different id grammars).
/// What: `SPEC-{SUBSYSTEM}-{NN}~{rev}` where SUBSYSTEM is
/// `[A-Z0-9][A-Z0-9-]*`, NN is `[0-9]{2,}` (zero-padded, opaque), and REV is the
/// open lowercase token `[a-z][a-z0-9]*` (`draft`, `v1`, `approved`, …).
/// Test: `super::tests::grammar_valid_ids` / `grammar_rejects_malformed`.
const SPEC_ID_BODY: &str = r"SPEC-[A-Z0-9][A-Z0-9-]*-[0-9]{2,}~[a-z][a-z0-9]*";

/// The compiled anchored matcher for a standalone spec ID (§2.1).
///
/// Why: validating an id or an anchor in isolation (e.g. a `{#SPEC-…}` heading
/// marker, or a frontmatter `id:` field) needs an anchored match so a partial
/// substring does not pass.
/// What: `^{SPEC_ID_BODY}$`, compiled once (the workspace `OnceLock` exception).
/// Test: `super::tests::grammar_valid_ids`.
fn spec_id_anchored_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"^{SPEC_ID_BODY}$")).expect("anchored spec-id pattern compiles")
    })
}

/// The compiled three-capture SLD reference matcher (§2.2).
///
/// Why: §2.2 admits two inline surface forms (a Markdown link and a bare
/// `id path#anchor`) but parses both with ONE regex that captures the three
/// fields and ignores link/comment punctuation between them.
/// What: captures `(id, path.md, anchor)` — the id, a repo-root-relative `.md`
/// path, and the anchor after `#`, tolerating arbitrary same-line noise between
/// the id and the path (`[^\n]*?`, lazy). Compiled once.
/// Test: `super::tests::grammar_reference_both_forms`.
pub fn reference_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"({SPEC_ID_BODY})[^\n]*?([A-Za-z0-9._/-]+\.md)#({SPEC_ID_BODY})"
        ))
        .expect("SLD reference pattern compiles")
    })
}

/// True when `id` is a well-formed spec ID per DOC-38 §2.1.
///
/// Why: the linter's id-grammar check and anchor validation both need a single
/// authoritative "is this a valid `SPEC-…~rev` id?" predicate so they agree.
/// What: anchored match against `SPEC_ID_BODY` — subsystem, a 2+-digit opaque
/// counter, and an open lowercase revision token. A resolver MUST NOT reject a
/// well-formed `~<token>` revision it does not recognise (§2.1), so no revision
/// allowlist is applied.
/// Test: `super::tests::grammar_valid_ids`, `grammar_rejects_malformed`.
#[must_use]
pub fn is_valid_spec_id(id: &str) -> bool {
    spec_id_anchored_re().is_match(id)
}

/// Split a `SPEC-…~rev` id/anchor into its revision (the part after the last `~`).
///
/// Why: revision-drift awareness (§4.4) and revision-tolerant section matching
/// both compare the revision independently of the base id. This is the single
/// canonical implementation — `intent_source::spec_resolve` re-exports it rather
/// than defining its own (DOC-38 one-grammar consolidation).
/// What: returns the substring after the last `~`, or `None` when the id carries
/// no `~` suffix.
/// Test: `super::tests::grammar_revision_of`.
#[must_use]
pub fn revision_of(spec_id: &str) -> Option<String> {
    spec_id.rsplit_once('~').map(|(_, rev)| rev.to_string())
}

/// The revision-insensitive base of a `SPEC-…~rev` id/anchor.
///
/// Why: matching a referencing anchor (`SPEC-X-01~v1`) to a section heading
/// (`SPEC-X-01~v2`) must key on the stable base id, not the revision — so a
/// reference still resolves across a revision bump (§4.4; drift is flagged, not
/// enforced — §1.3). The single canonical implementation shared with
/// `spec_resolve`.
/// What: returns the substring before the last `~`, or the whole id when there
/// is no `~`.
/// Test: `super::tests::grammar_base_id`.
#[must_use]
pub fn base_id(spec_id: &str) -> &str {
    spec_id.rsplit_once('~').map_or(spec_id, |(base, _)| base)
}

/// True when a captured reference path contains a `..` traversal segment.
///
/// Why: a repo-root-relative reference path must never escape the repository via
/// `..`; the `regex` crate has no look-around, so this is a post-match filter
/// (mirrors the `intent_source` defence-in-depth guard).
/// What: returns `true` when any `/`-split segment equals `..`.
/// Test: `super::tests::grammar_rejects_traversal`.
#[must_use]
pub fn has_traversal(path: &str) -> bool {
    path.split('/').any(|seg| seg == "..")
}
