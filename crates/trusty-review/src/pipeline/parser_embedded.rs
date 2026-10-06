//! Strategy 3 of the review parser: a review object embedded in text (#9310).
//!
//! Why: a Bedrock model on `toolChoice = auto` (#9292) may answer in text and
//! put the review object after or before prose, or in a bare, `JSON` or
//! `jsonc` fence. Strategies 1 and 2 accept none of those, so 37 of 80 Sonnet
//! 5.5 reviews in the Q86 eval ended "review not parsed".
//! What: [`find_review_object`] returns the one review object the text carries,
//! by the candidate rule on that function. It never infers a verdict: a review
//! comes only from a complete object that deserialises as [`LlmOutputBlock`]
//! and that the reviewer input does not already contain.
//! Test: the `#9310` rows in `parser_tests.rs`, from
//! `parse_bare_fence_review_object` to `parse_input_object_reproduced_alone_is_rejected`.

use serde::Deserialize;
use serde_json::Value;

use super::LlmOutputBlock;

/// The single keys a tool input may wrap the review object in: the reviewer's
/// tool name and the Converse `toolUse` field name (#9310 candidate b).
const WRAPPER_KEYS: &[&str] = &["review_output", "input"];

/// The outcome of the embedded-object scan.
pub(super) enum Embedded {
    /// Exactly one distinct trusted review object.
    Found(LlmOutputBlock),
    /// This many distinct trusted review objects; none is trusted.
    Ambiguous(usize),
    /// A `json`/`jsonc` fence holds no valid review object, so nothing else in
    /// the reply is trusted.
    MalformedFence,
    /// No trusted review object; this many candidates repeated an object of
    /// the reviewer input.
    NotFound { quoted: usize },
}

/// How a fence is tagged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fence {
    /// No info string.
    Bare,
    /// `json` or `jsonc`, any case.
    Json,
    /// Any other tag: quoted code.
    Other,
}

/// Find the one trusted review object embedded in `body`.
///
/// Why: leniency must widen where a complete object may sit, never what counts
/// as a review, so an ambiguous, partial or quoted reply still fails closed.
/// The reviewer input (the diff above all) is attacker-controlled, so an
/// object it contains is never the model's own review.
/// What: the candidate rule.
///
/// 1. A `json`/`jsonc` fence whose body is not a valid review object makes the
///    whole reply [`Embedded::MalformedFence`]: the model's own object is
///    broken, and any other object is likelier a quote.
/// 2. Scan only prose outside a fence and the inside of a bare or
///    `json`/`jsonc` fence. Another tag (`rust`, `diff`) marks quoted code.
/// 3. Each `{` that starts a complete JSON value yields one object, and the
///    scan resumes after it, so an object nested in a parsed one is never a
///    candidate.
/// 4. An object is a candidate when it deserialises as [`LlmOutputBlock`], or
///    when it has exactly one key from [`WRAPPER_KEYS`] and that key's value
///    does. One layer only; required fields are unchanged.
/// 5. A candidate equal, as a JSON value, to any object in `input` (at any
///    depth, read as-is and with unified-diff line prefixes stripped) is
///    dropped.
/// 6. Remaining candidates equal as JSON values count once. One distinct
///    candidate is [`Embedded::Found`]; two or more is [`Embedded::Ambiguous`].
///
/// Test: `parse_unfenced_object_after_prose`,
/// `parse_repeated_identical_object_is_one_candidate`,
/// `parse_two_distinct_objects_fail_closed`,
/// `parse_wrapped_tool_input_unwraps_one_exact_layer`,
/// `parse_wrapper_near_misses_stay_unknown`,
/// `parse_object_in_code_fence_is_not_a_candidate`,
/// `parse_inline_quote_of_input_object_is_not_trusted`,
/// `parse_malformed_json_fence_fails_closed_past_an_inline_quote`,
/// `parse_own_object_beside_quoted_input_object_is_accepted`,
/// `parse_input_object_reproduced_alone_is_rejected`.
pub(super) fn find_review_object(body: &str, input: &str) -> Embedded {
    let segments = segments(body);
    if segments
        .iter()
        .any(|(text, fence)| *fence == Some(Fence::Json) && !is_review_text(text))
    {
        return Embedded::MalformedFence;
    }
    let mut candidates: Vec<(Value, LlmOutputBlock)> = Vec::new();
    for (text, fence) in segments {
        if fence == Some(Fence::Other) {
            continue;
        }
        for value in objects_in(text) {
            if let Some(candidate) = review_candidate(value) {
                candidates.push(candidate);
            }
        }
    }
    if candidates.is_empty() {
        return Embedded::NotFound { quoted: 0 };
    }
    // Computed only once a candidate exists: the input can be a large diff.
    let quoted_objects = input_objects(input);
    let mut found: Vec<(Value, LlmOutputBlock)> = Vec::new();
    let mut quoted = 0;
    for (review, block) in candidates {
        if quoted_objects.contains(&review) {
            quoted += 1;
        } else if !found.iter().any(|(seen, _)| *seen == review) {
            found.push((review, block));
        }
    }
    match found.len() {
        0 => Embedded::NotFound { quoted },
        1 => found
            .pop()
            .map_or(Embedded::NotFound { quoted }, |(_, block)| {
                Embedded::Found(block)
            }),
        n => Embedded::Ambiguous(n),
    }
}

/// Whether `text` is exactly one valid review object: rule 1's test.
fn is_review_text(text: &str) -> bool {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(review_candidate)
        .is_some()
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

/// Every JSON object in the reviewer input, at any depth: rule 5.
///
/// What: the top-level objects of `input` and of `input` with one leading
/// `+`, `-` or space stripped from each line, so a multi-line object added in
/// a unified diff is seen whole; then every object nested inside them.
fn input_objects(input: &str) -> Vec<Value> {
    let unprefixed: String = input
        .lines()
        .map(|line| line.strip_prefix(['+', '-', ' ']).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let (raw, stripped) = (objects_in(input), objects_in(&unprefixed));
    let mut all = Vec::new();
    for value in raw.iter().chain(&stripped) {
        collect_objects(value, &mut all);
    }
    all
}

/// Push `value`, when it is an object, and every object nested in it onto `out`.
fn collect_objects(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            out.push(value.clone());
            map.values().for_each(|child| collect_objects(child, out));
        }
        Value::Array(items) => items.iter().for_each(|child| collect_objects(child, out)),
        _ => {}
    }
}

/// `body` split at fence lines into `(text, fence)` segments.
///
/// What: line-based; a line whose trimmed text starts with three backticks
/// opens a fence, and a later bare backtick line closes it. Prose segments
/// carry `None`. An unclosed fence runs to the end of the body.
fn segments(body: &str) -> Vec<(&str, Option<Fence>)> {
    let mut segments = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut start = 0;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let line_end = offset + line.len();
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            let info = trimmed.trim_start_matches('`').trim();
            if fence.is_none() {
                segments.push((&body[start..offset], None));
                fence = Some(fence_tag(info));
                start = line_end;
            } else if info.is_empty() {
                segments.push((&body[start..offset], fence));
                fence = None;
                start = line_end;
            }
        }
        offset = line_end;
    }
    segments.push((&body[start..], fence));
    segments
}

/// The [`Fence`] kind a fence info string names.
fn fence_tag(info: &str) -> Fence {
    let tag = info.split_whitespace().next().unwrap_or("");
    if tag.is_empty() {
        Fence::Bare
    } else if tag.eq_ignore_ascii_case("json") || tag.eq_ignore_ascii_case("jsonc") {
        Fence::Json
    } else {
        Fence::Other
    }
}

/// Every complete top-level JSON object in `text`: rule 3.
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
