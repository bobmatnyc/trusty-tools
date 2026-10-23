//! Rule-id markers in bundled agent and skill Markdown (#10058).
//!
//! Why: bundle tests used to pin a rule by its exact wording, so rewording a
//! sentence broke the build. A test now pins the rule's stable id instead. A
//! composed agent body is paid on every subagent turn, so the composer drops
//! the markers, the way the PM prompt fold drops authoring comments (#7616).
//! What: the grammar, a parser and a stripper.
//!
//! - A marker is a whole line `<!-- rule:<id> -->`. Leading and trailing
//!   whitespace is allowed; anything else on the line makes it prose.
//! - `<id>` is one or more `[a-z0-9]+` words joined by `-` or `/`, for
//!   example `tm-workflow/pre-pull-collision-pointer` or `r-049`.
//! - A marker sits on the line directly above the paragraph, bullet, table,
//!   fence or blockquote it pins, under that block's heading, and adds no
//!   blank line, so removing it restores the text exactly.
//! - A line inside a fenced code block (opened by three backticks or tildes)
//!   is never a marker.
//!
//! Test: `rule_marker_tests.rs`.

use std::borrow::Cow;

/// The text every marker line starts with.
pub const RULE_MARKER_OPEN: &str = "<!-- rule:";

/// The text every marker line ends with.
const RULE_MARKER_CLOSE: &str = " -->";

/// The id of marker `line`, or `None` when the line is not a valid marker.
///
/// Test: `marker_id_accepts_the_grammar_and_rejects_the_rest`.
pub fn marker_id(line: &str) -> Option<&str> {
    let id = line
        .trim()
        .strip_prefix(RULE_MARKER_OPEN)?
        .strip_suffix(RULE_MARKER_CLOSE)?;
    let valid = id.split(['-', '/']).all(|word| {
        !word.is_empty() && word.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'))
    });
    valid.then_some(id)
}

/// Each line of `text` (line ending kept) with whether it lies outside a
/// fenced code block and is not itself a fence line.
fn outside_fences(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut fenced = false;
    text.split_inclusive('\n').map(move |line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            (line, false)
        } else {
            (line, !fenced)
        }
    })
}

/// The ids of every valid marker in `text`, in order.
///
/// Test: `rule_ids_skips_fenced_examples_and_malformed_lines`.
pub fn rule_ids(text: &str) -> Vec<&str> {
    outside_fences(text)
        .filter(|(_, outside)| *outside)
        .filter_map(|(line, _)| marker_id(line))
        .collect()
}

/// Every line of `text` that opens like a marker but does not parse as one,
/// trimmed. A typo in an id would otherwise hide the rule from every test.
///
/// Test: `rule_ids_skips_fenced_examples_and_malformed_lines`.
pub fn malformed_markers(text: &str) -> Vec<&str> {
    outside_fences(text)
        .filter(|(line, outside)| *outside && line.trim_start().starts_with(RULE_MARKER_OPEN))
        .filter(|(line, _)| marker_id(line).is_none())
        .map(|(line, _)| line.trim())
        .collect()
}

/// `text` with every marker line removed and every other byte kept.
///
/// Why: markers are test metadata, so a composed agent body must not pay for
/// them on every turn (#10058).
/// What: drops each valid marker line outside a fence; returns `text`
/// unchanged, without copying, when it holds no marker.
/// Test: `strip_removes_marker_lines_and_keeps_every_other_byte`.
pub fn strip_rule_markers(text: &str) -> Cow<'_, str> {
    if !text.contains(RULE_MARKER_OPEN) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        outside_fences(text)
            .filter(|(line, outside)| !(*outside && marker_id(line).is_some()))
            .map(|(line, _)| line)
            .collect(),
    )
}

#[cfg(test)]
#[path = "rule_marker_tests.rs"]
mod tests;
