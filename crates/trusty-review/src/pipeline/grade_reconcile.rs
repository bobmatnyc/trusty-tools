//! Reconcile the grade embedded in `review_body` with the final envelope grade.
//!
//! Why: `ReviewResult` carries the letter grade in TWO observable places — the
//! top-level `grade` field (the single authoritative value the pipeline computes
//! after the severity floor, verdict clamp, and shallow-review cap) and, in the
//! unified single-pass path, the RAW LLM response text stored verbatim in
//! `review_body` (which embeds the model's OWN self-assessed `"grade"` in its JSON
//! output block).  Those two values were never reconciled, so any late-stage
//! adjustment that lowered the top-level grade below the model's self-assessment
//! (the #1877 shallow-review cap; the #1486 post-verification clamp; the severity
//! floor) left the embedded grade stale and HIGHER than the top-level field.  A
//! caller that read only `grade` (a PM merge gate) saw a harsher grade than the
//! one printed inside `review_body`, and the two disagreed by up to a full letter
//! (issue #1886: outer "C"/"C+" vs embedded "B+" across PRs #1879/#1883/#1884).
//!
//! What: [`reconcile_review_body_grade`] takes the raw `review_body` text and the
//! FINAL top-level grade, and rewrites every JSON `"grade": "…"` string value in
//! the body to that final grade — making the embedded grade a mirror of the one
//! authoritative value rather than an independent (pre-floor) datum.  It is a
//! no-op when the body carries no JSON `"grade"` key (the map-reduce prose summary
//! path and the verdict-keyword-scan fallback) and when the final grade is `None`
//! (an un-reviewable UNKNOWN verdict, #1474 — there is no authoritative grade to
//! mirror, so the model's text is left untouched rather than fabricating one).
//!
//! This is a deliberately conservative, dependency-free string rewrite (no `regex`
//! crate, no full re-serialisation that would reflow the model's prose): it matches
//! the exact key token `"grade"` — which never collides with `"grade_justification"`
//! because that key has no `"` immediately after `grade` — followed by `:` and a
//! quoted value, and replaces only the value.  Grade strings are ASCII with no
//! embedded quotes, so the scan is byte-safe even amid multi-byte UTF-8 prose.
//!
//! #1902 generalises the same rewrite to the `"verdict"` key. The top-level
//! `verdict` is authoritative for exactly the same reason the grade is — it is
//! computed after the severity floor, the coverage floor, and the verification
//! round — and `review_body` carried the model's raw pre-adjustment lean beside
//! it. See [`reconcile_review_body_verdict`].
//!
//! Test: `grade_reconcile_tests.rs`.

use crate::models::Verdict;

/// The JSON key token whose string value is the embedded letter grade.
///
/// Why: matching the fully-quoted key `"grade"` (rather than the bare word
/// `grade`) is what makes the rewrite ignore `"grade_justification"` — in that key
/// the byte after `grade` is `_`, not the closing `"`, so the token never matches.
/// What: the literal searched for in the body to locate each embedded grade value.
/// Test: `reconcile_leaves_grade_justification_untouched`.
const GRADE_KEY_TOKEN: &str = "\"grade\"";

/// The JSON key token whose string value is the embedded verdict (#1902).
///
/// Why: the same fully-quoted-key rule as [`GRADE_KEY_TOKEN`] — matching
/// `"verdict"` and not the bare word is what keeps `"verdict_justification"`
/// untouched, since the byte after `verdict` there is `_`, not the closing `"`.
/// What: the literal searched for in the body to locate each embedded verdict.
/// Test: `reconcile_leaves_verdict_justification_untouched`.
const VERDICT_KEY_TOKEN: &str = "\"verdict\"";

/// Rewrite every JSON `"grade"` value in `review_body` to the final `grade`.
///
/// Why: guarantees the grade embedded in `review_body` can never disagree with the
/// authoritative top-level `ReviewResult.grade` (issue #1886).  The top-level grade
/// is the single source of truth; this propagates it into the model's own JSON so
/// both observable copies always match, without re-running any grading logic.
/// What: when `final_grade` is `Some(g)`, scans `body` for each `"grade": "…"`
/// occurrence and replaces the quoted value with `g`, preserving all surrounding
/// text (prose, fences, whitespace, and the key/colon separator) verbatim; a
/// `"grade"` key that is not followed by a quoted string value is left untouched.
/// When `final_grade` is `None`, or `body` contains no `"grade"` key, `body` is
/// returned unchanged.
/// Test: `reconcile_rewrites_direct_json_grade`,
/// `reconcile_rewrites_fenced_block_grade`,
/// `reconcile_none_grade_is_noop`, `reconcile_prose_without_grade_is_noop`,
/// `reconcile_rewrites_all_occurrences`,
/// `reconcile_leaves_grade_justification_untouched`,
/// `reconcile_handles_whitespace_around_colon`,
/// `reconcile_non_string_grade_is_noop`, `reconcile_is_byte_safe_with_utf8_prose`.
pub(crate) fn reconcile_review_body_grade(body: &str, final_grade: Option<&str>) -> String {
    // No authoritative grade to mirror (UNKNOWN / un-reviewable, #1474) — leave the
    // model's text exactly as-is rather than fabricating a value.
    let Some(new_grade) = final_grade else {
        return body.to_string();
    };
    rewrite_json_string_values(body, GRADE_KEY_TOKEN, new_grade)
}

/// Rewrite every JSON `"verdict"` value in `review_body` to the final verdict.
///
/// Why: #1886 reconciled the embedded GRADE and stopped there, so the same
/// staleness survived one field over (#1902). A `review_pr` on PR #1901
/// returned a top-level `verdict: BLOCK` while the verdict inside `review_body`
/// read `APPROVE`: the top-level value is the pipeline's, computed after the
/// severity floor, the coverage floor, and the verification round, while the
/// embedded one is the model's raw pre-adjustment lean. An automated merge gate
/// reading the top-level `BLOCK` refused to proceed while the review a human
/// read said APPROVE, and the disagreement cost a manual investigation.
/// What: mirrors the authoritative verdict into the model's own JSON with the
/// same conservative rewrite [`reconcile_review_body_grade`] uses — the key
/// token `"verdict"` followed by a quoted value, value replaced, everything
/// else byte-for-byte. A body with no JSON `"verdict"` key (the map-reduce
/// prose summary) is returned unchanged; `"verdict_justification"` never
/// matches. `UNKNOWN` is written like any other verdict: unlike a grade, it is
/// a real assessed outcome, not the absence of one.
/// Test: `reconcile_rewrites_direct_json_verdict`,
/// `reconcile_rewrites_fenced_block_verdict`,
/// `reconcile_leaves_verdict_justification_untouched`,
/// `reconcile_verdict_prose_without_verdict_is_noop`,
/// `reconcile_rewrites_all_verdict_occurrences`.
pub(crate) fn reconcile_review_body_verdict(body: &str, final_verdict: &Verdict) -> String {
    rewrite_json_string_values(body, VERDICT_KEY_TOKEN, &final_verdict.to_string())
}

/// Replace the quoted string value of every `key_token` occurrence in `body`.
///
/// Why: the grade (#1886) and verdict (#1902) reconciliations are one rewrite
/// over two keys; a second copy of the scanner would drift the moment either
/// one's edge cases changed.
/// What: walks `body` for `key_token`, and for each occurrence whose value is a
/// quoted string, emits everything up to the opening quote verbatim, then
/// `new_value`, then resumes at the closing quote. An occurrence that is not
/// followed by a quoted string value (`null`, a number, a truncated tail) is
/// emitted untouched and the scan continues past it. The scan compares single
/// ASCII bytes only, so it is byte-safe amid multi-byte UTF-8 prose.
/// Test: the `reconcile_*` cases in `grade_reconcile_tests.rs`, which drive
/// this through both public wrappers.
fn rewrite_json_string_values(body: &str, key_token: &str, new_value: &str) -> String {
    let mut out = String::with_capacity(body.len() + new_value.len());
    let mut cursor = 0usize;

    while let Some(rel) = body[cursor..].find(key_token) {
        let key_start = cursor + rel;
        let key_end = key_start + key_token.len();

        match string_value_span(body, key_end) {
            // `key_end..value_start` includes the `:`, any whitespace, and the
            // opening quote; emit it verbatim, then the new grade, then resume at
            // the closing quote so it (and everything after) is emitted next.
            Some((value_start, close_quote)) => {
                out.push_str(&body[cursor..value_start]);
                out.push_str(new_value);
                cursor = close_quote;
            }
            // `"grade"` is present but not a quoted string value (e.g. `null`,
            // a number, or a truncated tail) — leave it untouched and continue the
            // scan after the key so we do not spin on the same match.
            None => {
                out.push_str(&body[cursor..key_end]);
                cursor = key_end;
            }
        }
    }

    out.push_str(&body[cursor..]);
    out
}

/// Locate the quoted string value that follows a rewritten key.
///
/// Why: the rewrite must find exactly the value between the quotes so it can splice
/// in the authoritative grade while preserving the key, colon, whitespace, and the
/// quote characters themselves.
/// What: starting at `key_end` (the byte just past the closing quote of the key),
/// skips ASCII whitespace, requires a `:`, skips ASCII whitespace, requires an
/// opening `"`, then scans to the next `"`.  Returns `(value_start, close_quote)`
/// as byte offsets — `value_start` is the first byte inside the quotes and
/// `close_quote` is the offset of the closing quote — or `None` when the shape does
/// not match (no colon, no opening quote, or an unterminated value).  The scan only
/// ever compares single ASCII bytes (`:`, whitespace, `"`), so the returned offsets
/// fall on `char` boundaries even when the surrounding body is multi-byte UTF-8.
/// Test: `reconcile_handles_whitespace_around_colon`,
/// `reconcile_non_string_grade_is_noop`, `reconcile_is_byte_safe_with_utf8_prose`.
fn string_value_span(body: &str, key_end: usize) -> Option<(usize, usize)> {
    let bytes = body.as_bytes();
    let n = bytes.len();
    let mut i = key_end;

    // Skip whitespace, require the ':' separator.
    while i < n && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || bytes[i] != b':' {
        return None;
    }
    i += 1;

    // Skip whitespace, require the opening quote.
    while i < n && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || bytes[i] != b'"' {
        return None;
    }
    i += 1;

    // Scan to the closing quote. Grade strings never contain an embedded quote, so
    // the first '"' terminates the value.
    let value_start = i;
    while i < n && bytes[i] != b'"' {
        i += 1;
    }
    if i >= n {
        return None; // unterminated string — leave the body untouched
    }
    Some((value_start, i))
}

#[cfg(test)]
#[path = "grade_reconcile_tests.rs"]
mod tests;
