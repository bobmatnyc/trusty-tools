//! Text-based tool-call extraction fallbacks (#1023).
//!
//! Why: Only well-known model families reliably populate the OpenAI-native
//! `choices[0].message.tool_calls` wire field (see
//! `provider::traits::Provider::supports_native_tools`). Others (Qwen,
//! DeepSeek, Gemma, and any future family) emit their intended tool call as
//! text inside `content` instead, using one of a few conventions. Each
//! convention gets its own free function here so [`super::ToolCallExtractor`]
//! can try them in a per-model order (see `super::strategy_order_for`).
//! What: [`extract_fenced_json`] handles a ```` ```json ``` ```` code block;
//! [`extract_angle_bracket`] handles `<tool_call>{...}</tool_call>`;
//! [`extract_balanced_json_scan`] is the tolerant last resort — it scans the
//! whole text for the first balanced `{...}` object (respecting quoted
//! strings) that parses as JSON and has a `name` field. All three return the
//! same `(name, arguments)` shape via the shared [`parse_call_object`] helper,
//! so `{"name": ..., "arguments": {...}}` and the `"parameters"` alias are
//! accepted uniformly.
//! Test: `strategies::tests::*`.

use serde_json::Value;

/// Parse a JSON object body into a `(name, arguments)` tool-call candidate.
///
/// Why: All three text-based strategies converge on the same expected JSON
/// shape once they've located the candidate substring; centralising the
/// parse avoids three copies of the same `name`/`arguments` extraction.
/// What: Parses `body` as JSON; requires a string `"name"` field; accepts
/// either `"arguments"` or `"parameters"` (alias) as the argument object,
/// defaulting to an empty object when neither is present. Returns `None` on
/// any parse failure or a missing/non-string `name`.
/// Test: `strategies::tests::parse_call_object_accepts_parameters_alias`.
fn parse_call_object(body: &str) -> Option<(String, Value)> {
    let value: Value = serde_json::from_str(body).ok()?;
    let name = value.get("name")?.as_str()?.to_string();
    let args = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some((name, args))
}

/// Extract a tool call from a fenced ```` ```json ``` ```` code block.
///
/// Why: Prompt-guided models (told "respond with a ```json tool call") commonly
/// follow that instruction literally rather than using native function-calling.
/// What: Finds the first ```` ```json ```` fence, takes everything up to the
/// next ```` ``` ````, and parses it via [`parse_call_object`]. Returns `None`
/// when no such fence exists or the body doesn't parse to the expected shape.
/// Test: `strategies::tests::fenced_json_extracts_call`.
pub(super) fn extract_fenced_json(text: &str) -> Option<(String, Value)> {
    const FENCE_OPEN: &str = "```json";
    const FENCE_CLOSE: &str = "```";
    let start = text.find(FENCE_OPEN)? + FENCE_OPEN.len();
    let rest = &text[start..];
    let end = rest.find(FENCE_CLOSE)?;
    parse_call_object(rest[..end].trim())
}

/// Extract a tool call from an `<tool_call>{...}</tool_call>` span.
///
/// Why: Some model families (notably Qwen/DeepSeek-style chat templates) are
/// trained on this exact tag convention for function-calling.
/// What: Finds the first `<tool_call>`/`</tool_call>` pair and parses the
/// enclosed body via [`parse_call_object`].
/// Test: `strategies::tests::angle_bracket_extracts_call`.
pub(super) fn extract_angle_bracket(text: &str) -> Option<(String, Value)> {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let start = text.find(OPEN)? + OPEN.len();
    let rest = &text[start..];
    let end = rest.find(CLOSE)?;
    parse_call_object(rest[..end].trim())
}

/// Tolerant last-resort scan: find any balanced `{...}` object in noisy text.
///
/// Why: A model may wrap its tool call in prose, partial markdown, or a
/// malformed fence — this is the catch-all before giving up entirely.
/// What: Scans left-to-right for `{`, finds its matching (quote-aware)
/// closing `}` via [`find_balanced_end`], and attempts [`parse_call_object`]
/// on each balanced span in turn until one succeeds. Returns `None` when no
/// balanced span parses to the expected shape.
/// Test: `strategies::tests::balanced_scan_recovers_call_from_noisy_text`.
pub(super) fn extract_balanced_json_scan(text: &str) -> Option<(String, Value)> {
    let mut search_from = 0usize;
    while let Some(rel_start) = text[search_from..].find('{') {
        let start = search_from + rel_start;
        let Some(end) = find_balanced_end(text, start) else {
            break;
        };
        if let Some(call) = parse_call_object(&text[start..=end]) {
            return Some(call);
        }
        search_from = start + 1;
    }
    None
}

/// Find the index of the `}` that balances the `{` at `start`, quote-aware.
///
/// Why: A naive "next `}`" search breaks on any tool argument string that
/// itself contains braces (e.g. a `content` argument with JSON inside it);
/// tracking string state and depth is required for a correct scan.
/// What: Walks characters from `start`, toggling `in_string` on unescaped `"`
/// and adjusting brace `depth` outside strings. Returns the byte index of the
/// `}` where `depth` first returns to zero, or `None` if the text ends first.
/// Test: Exercised via `extract_balanced_json_scan` tests (nested-object case).
fn find_balanced_end(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (idx, ch) in text.char_indices() {
        if idx < start {
            continue;
        }
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fenced ```json block extracts the call's name and arguments.
    ///
    /// Why: The primary fallback path for prompt-guided tool calling.
    /// What: A response with a fenced block; assert name + args.
    /// Test: this test.
    #[test]
    fn fenced_json_extracts_call() {
        let text = "Sure, I'll do that.\n```json\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}\n```\nDone.";
        let (name, args) = extract_fenced_json(text).expect("fenced call");
        assert_eq!(name, "bash");
        assert_eq!(args, json!({"command": "ls"}));
    }

    /// Missing fence returns `None` rather than panicking.
    ///
    /// Why: Must not treat arbitrary text as a hit.
    /// What: Plain text with no fence.
    /// Test: this test.
    #[test]
    fn fenced_json_none_without_fence() {
        assert!(extract_fenced_json("just some prose").is_none());
    }

    /// An `<tool_call>` tag extracts the call's name and arguments.
    ///
    /// Why: Qwen/DeepSeek-style tag convention.
    /// What: A response wrapping the JSON body in `<tool_call>` tags.
    /// Test: this test.
    #[test]
    fn angle_bracket_extracts_call() {
        let text =
            "<tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}</tool_call>";
        let (name, args) = extract_angle_bracket(text).expect("angle-bracket call");
        assert_eq!(name, "read_file");
        assert_eq!(args, json!({"path": "a.rs"}));
    }

    /// Missing tags return `None`.
    ///
    /// Why: Guard against false positives.
    /// What: Plain text with no tags.
    /// Test: this test.
    #[test]
    fn angle_bracket_none_without_tags() {
        assert!(extract_angle_bracket("no tags here").is_none());
    }

    /// The balanced scan recovers a call embedded in noisy surrounding prose.
    ///
    /// Why: This is the last-resort fallback for a model that ignores both
    /// conventions but still emits a raw JSON object somewhere in its answer.
    /// What: Prose before and after a bare JSON object.
    /// Test: this test.
    #[test]
    fn balanced_scan_recovers_call_from_noisy_text() {
        let text = "Let me call the tool: {\"name\": \"bash\", \"arguments\": {\"command\": \"echo hi\"}} — running now.";
        let (name, args) = extract_balanced_json_scan(text).expect("scanned call");
        assert_eq!(name, "bash");
        assert_eq!(args, json!({"command": "echo hi"}));
    }

    /// The balanced scan is brace-depth-aware around nested objects.
    ///
    /// Why: A naive "find first `}`" would truncate the arguments object;
    /// verify nested braces inside `arguments` are handled correctly.
    /// What: `arguments` itself contains a nested object.
    /// Test: this test.
    #[test]
    fn balanced_scan_handles_nested_braces() {
        let text = r#"{"name": "write_file", "arguments": {"path": "a.json", "content": "{\"nested\": true}"}}"#;
        let (name, args) = extract_balanced_json_scan(text).expect("scanned call");
        assert_eq!(name, "write_file");
        assert_eq!(args["path"], "a.json");
    }

    /// The scan skips a non-matching brace span and finds a later valid one.
    ///
    /// Why: Noisy text may contain an unrelated `{...}` (e.g. an example) before
    /// the real tool call; the scan must not give up on the first failure.
    /// What: A non-tool-call object followed by a real one.
    /// Test: this test.
    #[test]
    fn balanced_scan_skips_non_matching_object_then_finds_real_call() {
        let text = r#"Example: {"foo": "bar"} then the real call {"name": "bash", "arguments": {"command": "pwd"}}"#;
        let (name, _) = extract_balanced_json_scan(text).expect("scanned call");
        assert_eq!(name, "bash");
    }

    /// No JSON object anywhere in the text returns `None`.
    ///
    /// Why: The extractor must be able to report "nothing found" cleanly.
    /// What: Plain prose with no braces at all.
    /// Test: this test.
    #[test]
    fn balanced_scan_none_when_no_object_present() {
        assert!(extract_balanced_json_scan("no json here at all").is_none());
    }

    /// `parse_call_object` accepts `"parameters"` as an alias for `"arguments"`.
    ///
    /// Why: Some model templates use `parameters` instead of `arguments`; the
    /// extractor should not reject an otherwise well-formed call over naming.
    /// What: A call object using `parameters` instead of `arguments`.
    /// Test: this test.
    #[test]
    fn parse_call_object_accepts_parameters_alias() {
        let (name, args) =
            parse_call_object(r#"{"name": "bash", "parameters": {"command": "ls"}}"#)
                .expect("parsed call");
        assert_eq!(name, "bash");
        assert_eq!(args, json!({"command": "ls"}));
    }

    /// A call object missing `name` is rejected.
    ///
    /// Why: `name` is the one field with no reasonable default.
    /// What: A JSON object with only `arguments`.
    /// Test: this test.
    #[test]
    fn parse_call_object_rejects_missing_name() {
        assert!(parse_call_object(r#"{"arguments": {}}"#).is_none());
    }
}
