//! Apply a verdict's closed redaction ops to a refused row (#8624, PR-C).
//!
//! Why: a reviewer admits a row by naming which spans to remove, never by
//! supplying text. Ops address spans by index into the live
//! `find_secret_spans` output; this module never reads a marker.
//! What: [`check_ops`] refuses an empty list, an index past the span list and
//! an index named twice. [`apply_ops`] turns each op into a byte-range edit,
//! skips an edit wholly inside an earlier one (a span on a dropped line), and
//! refuses edits that partly overlap.
//! Test: `review::admit_tests` (`out_of_range_op_refuses_the_row`,
//! `ops_edit_the_named_spans_only`).

use std::collections::HashSet;
use std::ops::Range;

use trusty_common::memory_core::filter::SecretSpan;

use super::verdict::RedactOp;

/// Why an op list was refused. Carries indices only, never text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OpError {
    #[error("admit-redacted needs at least one op")]
    Empty,
    #[error("op {op} names span {span}, but the row has {spans} span(s)")]
    OutOfRange {
        op: usize,
        span: usize,
        spans: usize,
    },
    #[error("span {span} is named by more than one op")]
    DuplicateSpan { span: usize },
    #[error("op {op} partly overlaps an earlier op's edit")]
    Overlap { op: usize },
    #[error("op {op} does not fall on char boundaries")]
    Boundary { op: usize },
}

/// Refuse `ops` unless it is non-empty, every index is below `span_count`,
/// and no index repeats.
///
/// Test: `out_of_range_op_refuses_the_row`, `record_refuses_bad_ops`.
pub(crate) fn check_ops(ops: &[RedactOp], span_count: usize) -> Result<(), OpError> {
    if ops.is_empty() {
        return Err(OpError::Empty);
    }
    let mut seen = HashSet::new();
    for (op, o) in ops.iter().enumerate() {
        let span = o.span();
        if span >= span_count {
            return Err(OpError::OutOfRange {
                op,
                span,
                spans: span_count,
            });
        }
        if !seen.insert(span) {
            return Err(OpError::DuplicateSpan { span });
        }
    }
    Ok(())
}

/// The byte range of the whole line(s) holding `r`, its newline included.
fn line_range(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let start = text.get(..r.start)?.rfind('\n').map_or(0, |i| i + 1);
    let end = text
        .get(r.end..)?
        .find('\n')
        .map_or(text.len(), |i| r.end + i + 1);
    Some(start..end)
}

/// `text` with `ops` applied to `spans`, or the reason it cannot be.
///
/// Why (#8624 rule 5): the ops are the only way a reviewer changes content,
/// and an op that names no real span must refuse the row, not be skipped.
/// What: `redact_span` writes `[REDACTED:<RULE>]`, the label upper-cased so
/// that `BASE64_BLOB`'s digits never form a mixed-case run the re-screen
/// would flag; `delete_span` removes the span; `drop_line` removes its whole
/// line. Edits apply in text order. The result is not screened here; the
/// caller screens it again.
/// Test: `out_of_range_op_refuses_the_row`, `ops_edit_the_named_spans_only`.
pub(crate) fn apply_ops(
    text: &str,
    spans: &[SecretSpan],
    ops: &[RedactOp],
) -> Result<String, OpError> {
    check_ops(ops, spans.len())?;
    let mut edits: Vec<(usize, Range<usize>, String)> = Vec::with_capacity(ops.len());
    for (op, o) in ops.iter().enumerate() {
        let s = spans.get(o.span()).ok_or(OpError::OutOfRange {
            op,
            span: o.span(),
            spans: spans.len(),
        })?;
        let edit = match o {
            RedactOp::RedactSpan { .. } => {
                (
                    s.range.clone(), // #8624: upper-case keeps the marker itself from screening as a secret.
                    format!("[REDACTED:{}]", s.rule.label().to_ascii_uppercase()),
                )
            }
            RedactOp::DeleteSpan { .. } => (s.range.clone(), String::new()),
            RedactOp::DropLine { .. } => (
                line_range(text, &s.range).ok_or(OpError::Boundary { op })?,
                String::new(),
            ),
        };
        edits.push((op, edit.0, edit.1));
    }
    // Containing edits first, so a contained one is seen after its container.
    edits.sort_by_key(|(_, r, _)| (r.start, std::cmp::Reverse(r.end)));
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    for (op, r, replacement) in edits {
        if r.start < pos {
            if r.end <= pos {
                continue;
            }
            return Err(OpError::Overlap { op });
        }
        out.push_str(text.get(pos..r.start).ok_or(OpError::Boundary { op })?);
        out.push_str(&replacement);
        pos = r.end;
    }
    out.push_str(text.get(pos..).ok_or(OpError::Boundary { op: ops.len() })?);
    Ok(out)
}
