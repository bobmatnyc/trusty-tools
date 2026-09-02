//! Code-only rendering of a report template's non-code sections (#6669).
//!
//! Why: a CAST-shaped technical-DD report has sections no repository can
//! answer. Peer Benchmark needs CAST's proprietary corpus of scanned
//! applications; Next Steps is an organizational recommendation drawn from
//! interviews. A code-only audit must still SHOW those headings — a reader who
//! finds one missing cannot tell a deliberate boundary from a silent omission,
//! and the empty-section collapse pass would render it as "no data available",
//! which reads as a measurement that came back empty rather than one that was
//! never attempted. Sections that ARE code-derived but would normally be
//! cross-checked against a human conversation carry a provenance line saying
//! so, so a reader never mistakes them for validated.
//! What: [`apply`] rewrites the template source before the fill engine sees it.
//! A template marks its own regions with `<!-- code_only:non_code <reason> -->`
//! or `<!-- code_only:partial -->`, each closed by `<!-- code_only:end -->`
//! and never nested inside another region; nothing here hardcodes a section
//! name, so a template author moves or adds a boundary without a Rust change.
//! With code-only OFF the source is returned
//! byte-identical and the markers are stripped downstream as ordinary template
//! comments, so an unaffected render stays exactly as it was.
//! Test: `code_only_tests.rs`; end to end by
//! `crates/trusty-review/tests/cast_template_golden.rs`.

use tracing::warn;

use super::polish::balanced_comment_len;

/// Opening marker for a section no code inspection can answer.
pub const NON_CODE_MARKER: &str = "code_only:non_code";
/// Opening marker for a section that is code-derived but never cross-checked.
pub const PARTIAL_MARKER: &str = "code_only:partial";
/// Closing marker for either region kind.
pub const END_MARKER: &str = "code_only:end";

/// The lead-in a NON-CODE section renders in place of its data, under
/// code-only.
///
/// Why: one greppable string is what lets the golden test assert the boundary
/// is stated on every such section rather than checking each wording by hand.
pub const OUT_OF_SCOPE_LEAD: &str = "**Out of scope for a code-only audit**";

/// The reason used when a `non_code` marker names none.
const DEFAULT_REASON: &str = "interviews or operational data";

/// The line appended to a PARTIAL section under code-only.
pub const PARTIAL_NOTE: &str =
    "_Inferred from code; not validated by interview or operational data._";

/// The Report Metadata value stating this render's scope (code-only).
pub const SCOPE_CODE_ONLY: &str = "Code-only — repository inspection alone. No interviews, no \
                                   operational data, no vendor benchmark corpus.";

/// The Report Metadata value stating this render's scope (unrestricted).
pub const SCOPE_FULL: &str =
    "Full — repository inspection plus whatever the engagement supplied beside it.";

/// The addendum every synthesized section's instruction carries under
/// code-only.
///
/// Why: the rendered page states the boundary, but the model writing the
/// executive summary and the top-risks rows never reads the rendered page. An
/// exec summary recommending an organizational change, or citing a peer
/// quartile, contradicts the document it opens.
pub const SYNTHESIS_ADDENDUM: &str = "This is a CODE-ONLY audit: every claim must come from \
                                      repository inspection. Do not recommend organizational or \
                                      process changes, do not cite a peer-benchmark position or \
                                      an industry comparison, and do not refer to interviews, \
                                      operational metrics, or vendor-corpus data — none was \
                                      collected.";

/// Which behaviour a marked region gets under code-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No code inspection can answer this — replace the body with the boundary.
    NonCode,
    /// Code-derived but never cross-checked — keep the body, append the note.
    Partial,
}

/// Rewrite a template's code-only regions.
///
/// Why: the single transform both the render path and the golden test call, so
/// what a test asserts is what an operator gets.
/// What: with `code_only` false, returns `template` unchanged. With it true,
/// every `non_code` region's body is replaced by the out-of-scope block and
/// every `partial` region gains [`PARTIAL_NOTE`] after its body; the marker
/// comments themselves are consumed either way. Regions never nest: a region
/// left unclosed, or one that opens another region before its own
/// `code_only:end`, is logged and passed through untouched — a template typo
/// must never truncate a report or rewrite a span the author did not mark.
/// Test: `disabled_returns_the_source_unchanged`,
/// `non_code_body_is_replaced_by_the_boundary`,
/// `partial_keeps_its_body_and_gains_the_note`,
/// `an_unclosed_region_is_passed_through`,
/// `a_nested_region_leaves_the_outer_region_untransformed`.
#[must_use]
pub fn apply(template: &str, code_only: bool) -> String {
    if !code_only {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut i = 0usize;
    while let Some(rel) = template[i..].find("<!--") {
        let start = i + rel;
        out.push_str(&template[i..start]);
        let rest = &template[start..];
        let Some(len) = balanced_comment_len(rest) else {
            // Unterminated comment: nothing further is parseable, so the
            // remainder is copied verbatim rather than guessed at.
            out.push_str(rest);
            return out;
        };
        match opener(&rest[4..len - 3]) {
            Some((kind, reason)) => match region_end(template, start + len) {
                RegionEnd::Closed(body_end, after) => {
                    push_region(&mut out, &template[start + len..body_end], kind, &reason);
                    i = after;
                }
                // #6669: both malformed shapes fail open the same way — the
                // opening marker is copied through as an ordinary comment and
                // the scan resumes just after it.
                RegionEnd::Unclosed => {
                    warn!(
                        marker = %kind.marker(),
                        "template code-only region is never closed by `code_only:end`; ignoring it"
                    );
                    out.push_str(&rest[..len]);
                    i = start + len;
                }
                RegionEnd::Nested => {
                    warn!(
                        marker = %kind.marker(),
                        "template code-only region opens another region before its own \
                         `code_only:end`; regions do not nest, so this one is ignored"
                    );
                    out.push_str(&rest[..len]);
                    i = start + len;
                }
            },
            None => {
                // Any other comment — including a stray `code_only:end` — is
                // copied through for the ordinary comment stripper to handle.
                out.push_str(&rest[..len]);
                i = start + len;
            }
        }
    }
    out.push_str(&template[i..]);
    out
}

impl Kind {
    /// The marker spelling this kind opens with, for a diagnostic.
    fn marker(self) -> &'static str {
        match self {
            Self::NonCode => NON_CODE_MARKER,
            Self::Partial => PARTIAL_MARKER,
        }
    }
}

/// The region kind and reason an opening marker declares, if it is one.
///
/// Why: the reason travels in the marker so each section states its OWN
/// boundary — "CAST's proprietary corpus" and "interviews with the delivery
/// team" are different facts and must not collapse into one generic line.
/// What: `Some` for either opening marker; the reason is whitespace-normalized
/// so a marker wrapped across template lines renders as one sentence.
/// Test: `a_marker_reason_is_whitespace_normalized`,
/// `a_non_code_marker_with_no_reason_uses_the_default`.
fn opener(inner: &str) -> Option<(Kind, String)> {
    let trimmed = inner.trim();
    if let Some(reason) = trimmed.strip_prefix(NON_CODE_MARKER) {
        let reason = reason.trim();
        let reason = if reason.is_empty() {
            DEFAULT_REASON.to_string()
        } else {
            reason.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        return Some((Kind::NonCode, reason));
    }
    if trimmed.strip_prefix(PARTIAL_MARKER).is_some() {
        return Some((Kind::Partial, String::new()));
    }
    None
}

/// How the region opened at `from` ends.
///
/// Why: `Nested` exists because it used to be indistinguishable from
/// `Closed` — the scan took the FIRST `code_only:end` it found, so an outer
/// region closed at an inner region's end marker, the rest of the outer body
/// flowed into the report as literal template text, and no warning fired
/// because an end marker HAD been found (#6669).
enum RegionEnd {
    /// Closed: the body stops at `.0` and copying resumes at `.1`.
    Closed(usize, usize),
    /// Another region opens before this one's `code_only:end`.
    Nested,
    /// No `code_only:end` closes this region.
    Unclosed,
}

/// Where the region body that starts at `from` ends.
///
/// What: the first marker comment after `from` decides — `code_only:end`
/// closes the region, either opening marker means the template nested one
/// region inside another, and any other comment is skipped over.
/// Test: `an_unclosed_region_is_passed_through`,
/// `a_nested_region_leaves_the_outer_region_untransformed`,
/// `consecutive_regions_are_each_transformed`.
fn region_end(template: &str, from: usize) -> RegionEnd {
    let mut i = from;
    while let Some(rel) = template[i..].find("<!--") {
        let start = i + rel;
        let Some(len) = balanced_comment_len(&template[start..]) else {
            return RegionEnd::Unclosed;
        };
        let inner = &template[start + 4..start + len - 3];
        if inner.trim() == END_MARKER {
            return RegionEnd::Closed(start, start + len);
        }
        if opener(inner).is_some() {
            return RegionEnd::Nested;
        }
        i = start + len;
    }
    RegionEnd::Unclosed
}

/// Append one rewritten region to `out`.
fn push_region(out: &mut String, body: &str, kind: Kind, reason: &str) {
    match kind {
        Kind::NonCode => {
            out.push('\n');
            out.push_str("> ");
            out.push_str(OUT_OF_SCOPE_LEAD);
            out.push_str(" — requires ");
            out.push_str(reason);
            out.push_str(", not available from repository inspection alone.\n");
        }
        Kind::Partial => {
            out.push_str(body.trim_end());
            out.push_str("\n\n");
            out.push_str(PARTIAL_NOTE);
            out.push('\n');
        }
    }
}

#[cfg(test)]
#[path = "code_only_tests.rs"]
mod tests;
