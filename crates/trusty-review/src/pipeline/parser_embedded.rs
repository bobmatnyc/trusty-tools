//! Strategy 3 of the review parser: a review object embedded in text (#9310).
//!
//! Why: a Bedrock model on `toolChoice = auto` (#9292) may answer in text and
//! put the review object after or before prose, or in a bare, `JSON` or
//! `jsonc` fence. Strategies 1 and 2 accept none of those, so 37 of 80 Sonnet
//! 5.5 reviews in the Q86 eval ended "review not parsed".
//! What: [`find_review_object`] returns the one review object the text carries,
//! by the candidate rule on that function. It never infers a verdict: a review
//! comes only from a complete object that deserialises as [`LlmOutputBlock`].
//! Test: the `#9310` rows in `parser_tests.rs`, from
//! `parse_bare_fence_review_object` to `parse_object_in_code_fence_is_not_a_candidate`.

use serde::Deserialize;
use serde_json::Value;

use super::LlmOutputBlock;

/// The single keys a tool input may wrap the review object in: the reviewer's
/// tool name and the Converse `toolUse` field name (#9310 candidate b).
const WRAPPER_KEYS: &[&str] = &["review_output", "input"];

/// The outcome of the embedded-object scan.
pub(super) enum Embedded {
    /// Exactly one distinct review object.
    Found(LlmOutputBlock),
    /// This many distinct review objects; none is trusted.
    Ambiguous(usize),
    /// No complete review object.
    NotFound,
}

/// Find the one review object embedded in `body`.
///
/// Why: leniency must widen where a complete object may sit, never what counts
/// as a review, so an ambiguous or partial reply still fails closed.
/// What: the candidate rule.
///
/// 1. Scan only prose outside a fence and the inside of a fence tagged with
///    nothing, `json` or `jsonc` (any case). Another tag (`rust`, `diff`)
///    marks quoted code, which can hold a review-shaped fixture from the diff.
/// 2. Each `{` that starts a complete JSON value yields one object, and the
///    scan resumes after it, so an object nested in a parsed one is never a
///    candidate.
/// 3. An object is a candidate when it deserialises as [`LlmOutputBlock`], or
///    when it has exactly one key from [`WRAPPER_KEYS`] and that key's value
///    does. One layer only; required fields are unchanged.
/// 4. Candidates equal as JSON values count once. One distinct candidate is
///    [`Embedded::Found`]; two or more is [`Embedded::Ambiguous`].
///
/// Test: `parse_unfenced_object_after_prose`,
/// `parse_repeated_identical_object_is_one_candidate`,
/// `parse_two_distinct_objects_fail_closed`,
/// `parse_wrapped_tool_input_unwraps_one_exact_layer`,
/// `parse_wrapper_near_misses_stay_unknown`,
/// `parse_object_in_code_fence_is_not_a_candidate`.
pub(super) fn find_review_object(body: &str) -> Embedded {
    let mut found: Vec<(Value, LlmOutputBlock)> = Vec::new();
    for region in eligible_regions(body) {
        for value in objects_in(region) {
            if let Some((review, block)) = review_candidate(value)
                && !found.iter().any(|(seen, _)| *seen == review)
            {
                found.push((review, block));
            }
        }
    }
    match found.len() {
        0 => Embedded::NotFound,
        1 => found
            .pop()
            .map_or(Embedded::NotFound, |(_, block)| Embedded::Found(block)),
        n => Embedded::Ambiguous(n),
    }
}

/// The review object `value` carries, directly or in one exact wrapper.
fn review_candidate(value: Value) -> Option<(Value, LlmOutputBlock)> {
    if let Ok(block) = LlmOutputBlock::deserialize(&value) {
        return Some((value, block));
    }
    let Value::Object(map) = value else {
        return None;
    };
    if map.len() != 1 {
        return None;
    }
    let (key, inner) = map.into_iter().next()?;
    if !WRAPPER_KEYS.contains(&key.as_str()) {
        return None;
    }
    let block = LlmOutputBlock::deserialize(&inner).ok()?;
    Some((inner, block))
}

/// The slices of `body` the scan may read: rule 1 of [`find_review_object`].
///
/// What: line-based; a line whose trimmed text starts with three backticks
/// opens a fence, and a later bare backtick line closes it. An unclosed
/// fence runs to the end of the body.
fn eligible_regions(body: &str) -> Vec<&str> {
    let mut regions = Vec::new();
    // `Some(eligible)` while inside a fence.
    let mut fence: Option<bool> = None;
    let mut start = 0;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let line_end = offset + line.len();
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            let info = trimmed.trim_start_matches('`').trim();
            match fence {
                None => {
                    regions.push(&body[start..offset]);
                    fence = Some(is_json_tag(info));
                    start = line_end;
                }
                Some(eligible) if info.is_empty() => {
                    if eligible {
                        regions.push(&body[start..offset]);
                    }
                    fence = None;
                    start = line_end;
                }
                Some(_) => {}
            }
        }
        offset = line_end;
    }
    if fence != Some(false) {
        regions.push(&body[start..]);
    }
    regions
}

/// Whether a fence info string marks JSON: empty, `json` or `jsonc`, any case.
fn is_json_tag(info: &str) -> bool {
    let tag = info.split_whitespace().next().unwrap_or("");
    tag.is_empty() || tag.eq_ignore_ascii_case("json") || tag.eq_ignore_ascii_case("jsonc")
}

/// Every complete top-level JSON object in `text`: rule 2 of
/// [`find_review_object`].
fn objects_in(text: &str) -> Vec<Value> {
    let mut objects = Vec::new();
    let mut pos = 0;
    while let Some(rel) = text[pos..].find('{') {
        let start = pos + rel;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) => {
                objects.push(value);
                pos = start + stream.byte_offset();
            }
            // `{` is one byte, so `start + 1` is a char boundary.
            _ => pos = start + 1,
        }
    }
    objects
}
