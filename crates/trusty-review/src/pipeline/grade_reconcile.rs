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
//! Test: `grade_reconcile_tests.rs`.

/// The JSON key token whose string value is the embedded letter grade.
///
/// Why: matching the fully-quoted key `"grade"` (rather than the bare word
/// `grade`) is what makes the rewrite ignore `"grade_justification"` — in that key
/// the byte after `grade` is `_`, not the closing `"`, so the token never matches.
/// What: the literal searched for in the body to locate each embedded grade value.
/// Test: `reconcile_leaves_grade_justification_untouched`.
const GRADE_KEY_TOKEN: &str = "\"grade\"";

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

    let mut out = String::with_capacity(body.len() + new_grade.len());
    let mut cursor = 0usize;

    while let Some(rel) = body[cursor..].find(GRADE_KEY_TOKEN) {
        let key_start = cursor + rel;
        let key_end = key_start + GRADE_KEY_TOKEN.len();

        match string_value_span(body, key_end) {
            // `key_end..value_start` includes the `:`, any whitespace, and the
            // opening quote; emit it verbatim, then the new grade, then resume at
            // the closing quote so it (and everything after) is emitted next.
            Some((value_start, close_quote)) => {
                out.push_str(&body[cursor..value_start]);
                out.push_str(new_grade);
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

/// Locate the quoted string value that follows a `"grade"` key.
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
