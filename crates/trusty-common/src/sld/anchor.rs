//! `{#SPEC-…}` heading-anchor scanning + reference resolution (DOC-38 §4.3).
//!
//! # Spec References
//!
//! - [`SPEC-SLD-01~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-01~draft)
//!
//! Why: a governed spec section anchors its ID with a `{#SPEC-…}` heading marker
//! (§4.3); a reference resolves when its target file carries a heading anchor for
//! the referenced ID. Resolution is revision-tolerant — a `~v1` reference still
//! resolves to a `~v2` section — because *enforcing* revision drift is a non-goal
//! (§1.3, §4.4); the linter flags drift separately, it does not fail on it.
//! What: [`spec_anchors`] lists every `{#SPEC-…}` heading anchor (with its line),
//! skipping fenced code; [`anchor_resolves`] answers "does this file carry a
//! heading anchor whose base id matches?" for the linter's resolution check.
//! Test: `super::tests::anchor_*`.

use super::grammar::base_id;

/// A `{#SPEC-…}` anchor found on a heading line, with its 1-based line.
///
/// Why: the linter's anchor-vs-declared-id check and its drift signal both need
/// the anchor text and where it sits, for `file:line` diagnostics.
/// What: the full anchor id (`SPEC-…~rev`) and the heading's 1-based line.
/// Test: `super::tests::anchor_scan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingAnchor {
    /// The full anchor id including revision (`SPEC-SLD-02~draft`).
    pub id: String,
    /// 1-based line of the heading carrying the anchor.
    pub line: usize,
}

/// True when a stripped line opens/closes a Markdown code fence.
///
/// Why: a `{#SPEC-…}` appearing inside a fenced example (DOC-38's body has many)
/// is illustrative, not a real section anchor, and MUST be skipped (§2.3).
/// What: returns the fence char when the line begins with a run of 3+ `` ` ``
/// or `~`, else `None`.
/// Test: covered by `super::tests::anchor_skips_fenced`.
fn fence_char(trimmed: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|&ch| trimmed.starts_with(&std::iter::repeat_n(ch, 3).collect::<String>()))
}

/// Extract a `{#SPEC-…}` anchor from a heading line, if present.
///
/// Why: section matching keys on the heading's `{#anchor}` marker; a generic
/// slug (`{#overview}`) is not an SLD target, so only `SPEC-`-prefixed anchors
/// qualify (mirrors the `intent_source` contract).
/// What: returns the text inside a `{#…}` marker on the line, but only when it
/// starts with `SPEC-`; `None` otherwise.
/// Test: `super::tests::anchor_scan`.
fn anchor_in_heading(line: &str) -> Option<&str> {
    let open = line.find("{#")?;
    let rest = &line[open + 2..];
    let close = rest.find('}')?;
    let anchor = rest[..close].trim();
    anchor.starts_with("SPEC-").then_some(anchor)
}

/// List every `{#SPEC-…}` heading anchor in a Markdown document (skips fences).
///
/// Why: the linter needs the full set of section anchors to (a) check each
/// governed section's anchor against its declared ID and (b) resolve inbound
/// references. Fenced examples must be excluded so DOC-38's own body does not
/// self-populate spurious anchors (§2.3).
/// What: scans heading lines (those whose trimmed form starts with `#`) outside
/// fenced code, returning each `{#SPEC-…}` anchor with its 1-based line, in
/// document order.
/// Test: `super::tests::anchor_scan`, `anchor_skips_fenced`.
#[must_use]
pub fn spec_anchors(markdown: &str) -> Vec<HeadingAnchor> {
    let mut anchors = Vec::new();
    let mut in_fence: Option<char> = None;
    for (idx, raw) in markdown.lines().enumerate() {
        let trimmed = raw.trim_start();
        if let Some(open) = in_fence {
            if fence_char(trimmed) == Some(open) {
                in_fence = None;
            }
            continue;
        }
        if let Some(ch) = fence_char(trimmed) {
            in_fence = Some(ch);
            continue;
        }
        if !trimmed.starts_with('#') {
            continue;
        }
        if let Some(anchor) = anchor_in_heading(trimmed) {
            anchors.push(HeadingAnchor {
                id: anchor.to_string(),
                line: idx + 1,
            });
        }
    }
    anchors
}

/// True when `markdown` carries a heading anchor resolving `anchor` (drift-tolerant).
///
/// Why: a reference resolves when its target section exists; because enforcing
/// revision drift is a non-goal (§1.3), resolution keys on the revision-*insensitive*
/// base id, so a `~v1` reference still resolves against a `~v2` section (the
/// linter may flag the drift, but does not treat it as unresolved).
/// What: returns `true` when any `{#SPEC-…}` heading anchor in `markdown` shares
/// `anchor`'s base id (the part before the last `~`).
/// Test: `super::tests::anchor_resolves_exact`, `anchor_resolves_across_revision`.
#[must_use]
pub fn anchor_resolves(markdown: &str, anchor: &str) -> bool {
    let want = base_id(anchor);
    spec_anchors(markdown)
        .iter()
        .any(|a| base_id(&a.id) == want)
}
