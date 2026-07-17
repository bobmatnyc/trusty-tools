//! Shared frontmatter key/value line parser.
//!
//! Why: Two independent copies of a `split_once(':')` frontmatter parser
//! existed in `agent_builder` and `delegation_authority`; both silently
//! corrupted or hard-errored on values that legitimately contain a colon
//! (e.g. URLs, ISO-8601 timestamps, model ids like
//! `bedrock/us.anthropic.claude-sonnet-4-6`). A single shared function
//! eliminates the divergence and fixes the bug for both call sites. Moved to
//! trusty-agents-common (#2892) so `builder::compose_agent` and any future
//! non-trusty-mpm consumer share one parser; `trusty-mpm`'s
//! `core::delegation_authority` continues to use it via the
//! `core::frontmatter` re-export.
//! What: [`parse_kv_line`] splits a frontmatter line on the *first* colon
//! only, trims the key and value, strips optional surrounding quotes from the
//! value, and returns `Some((key, value))`; it returns `None` for blank lines,
//! comment lines, and YAML fence markers (`---`). [`parse_list_value`] parses
//! a frontmatter array-style scalar (e.g. `skills: [a, b]`) into its element
//! list.
//! Test: `cargo test -p trusty-agents-common -- frontmatter` covers URL
//! values, timestamp values, model-id values, normal key/value pairs,
//! trailing whitespace, and malformed/comment/empty lines.

/// Parse one frontmatter line into a `(key, value)` pair.
///
/// Why: values in YAML-ish frontmatter often contain colons — URLs, timestamps,
/// model ids — so splitting on the first colon and preserving the rest is the
/// only correct interpretation.
/// What: splits `line` on the first `':'`, lower-cases and trims the key, trims
/// the value and strips a single balanced layer of surrounding `"` or `'`
/// quotes. Returns `None` when the line is blank, a comment (`#`), a fence
/// (`---`), or has no colon at all.
/// Test: `frontmatter::tests::parse_kv_*` in this file.
///
/// Quote handling: only ONE balanced outer pair is removed (a leading and
/// matching trailing quote of the same kind). A greedy strip of every outer
/// quote would corrupt values whose escaped content ends in a quote — e.g.
/// an emitted YAML scalar `"He said \"hi\""` — by also eating the inner
/// `\"`. Stripping exactly one pair leaves the inner escapes intact so a
/// caller (see `agent_builder::split_frontmatter`) can unescape them and
/// recover the original string verbatim.
pub fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();

    // Skip fences, blank lines, and YAML comments.
    if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with('#') {
        return None;
    }

    // Split on the FIRST colon only; everything after belongs to the value.
    let (raw_key, raw_value) = trimmed.split_once(':')?;

    let key = raw_key.trim().to_ascii_lowercase();
    // A key must be a non-empty identifier; guard against lines like
    // `https://...` that have a colon in an unexpected position.
    if key.is_empty() {
        return None;
    }

    let value = strip_one_quote_pair(raw_value.trim()).to_string();

    Some((key, value))
}

/// Strip at most one balanced pair of surrounding `"` or `'` quotes.
///
/// Why: emitted YAML double-quoted scalars escape inner quotes as `\"`; a
/// greedy quote-trim would consume those inner quotes when they sit adjacent
/// to the closing fence (e.g. `"...\""`). Removing exactly one matching outer
/// pair preserves the escaped body for downstream unescaping.
/// What: returns the slice between a matching leading/trailing quote of the
/// same kind, or the input unchanged when it is not wrapped in a balanced pair.
/// Test: `parse_kv_strips_double_quotes`, `parse_kv_strips_single_quotes`,
/// `parse_kv_keeps_inner_escaped_quotes` in this file.
fn strip_one_quote_pair(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Parse a frontmatter array-style scalar into its element list.
///
/// Why: `skills:` (DOC-42, issue #2889) is the first list-valued frontmatter
/// field trusty-mpm parses; rather than pulling in a full YAML parser, it
/// reuses the existing line-based grammar with one addition — an inline
/// bracket array on the same line as the key, e.g.
/// `skills: [code-review-standards, verification-before-completion]` — which
/// is the grammar `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-01
/// documents as normative.
/// What: strips one balanced `[`...`]` pair (falling back to the bare value
/// when unbracketed), splits on `,`, trims and unquotes each element via
/// [`strip_one_quote_pair`], and drops empty elements — so `skills: []`,
/// `skills:` (blank value), and a trailing comma all yield an empty list. A
/// single bare name with no brackets (`skills: foo`) is treated as a
/// one-element list for caller convenience.
/// Test: `parse_list_value_*` in this file.
pub fn parse_list_value(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(str::trim)
        .map(strip_one_quote_pair)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── happy-path cases ─────────────────────────────────────────────────────

    #[test]
    fn parse_kv_normal() {
        // A plain `key: value` must round-trip cleanly.
        let (k, v) = parse_kv_line("name: engineer").unwrap();
        assert_eq!(k, "name");
        assert_eq!(v, "engineer");
    }

    #[test]
    fn parse_kv_url_value() {
        // A URL value contains multiple colons; only the first should be the
        // key/value separator — the rest must be preserved verbatim.
        let (k, v) = parse_kv_line("repo: https://github.com/x/y").unwrap();
        assert_eq!(k, "repo");
        assert_eq!(v, "https://github.com/x/y");
    }

    #[test]
    fn parse_kv_timestamp_value() {
        // ISO-8601 timestamps contain colons in the time component.
        let (k, v) = parse_kv_line("created: 2026-06-05T14:31:34").unwrap();
        assert_eq!(k, "created");
        assert_eq!(v, "2026-06-05T14:31:34");
    }

    #[test]
    fn parse_kv_model_id_value() {
        // Model ids like `bedrock/us.anthropic.claude-sonnet-4-6` contain
        // slashes and dots but no colon in the value — still must parse cleanly.
        let (k, v) = parse_kv_line("model: bedrock/us.anthropic.claude-sonnet-4-6").unwrap();
        assert_eq!(k, "model");
        assert_eq!(v, "bedrock/us.anthropic.claude-sonnet-4-6");
    }

    #[test]
    fn parse_kv_model_id_with_colon_in_value() {
        // A hypothetical model id that actually contains a colon (e.g.
        // `openrouter:gpt-4`) must preserve everything after the first colon.
        let (k, v) = parse_kv_line("model: openrouter:gpt-4").unwrap();
        assert_eq!(k, "model");
        assert_eq!(v, "openrouter:gpt-4");
    }

    #[test]
    fn parse_kv_strips_trailing_whitespace() {
        // Trailing spaces on both key and value must be trimmed.
        let (k, v) = parse_kv_line("  role :   engineer  ").unwrap();
        assert_eq!(k, "role");
        assert_eq!(v, "engineer");
    }

    #[test]
    fn parse_kv_strips_double_quotes() {
        // Values wrapped in double-quotes must have the outer quotes removed.
        let (k, v) = parse_kv_line(r#"description: "Implements features.""#).unwrap();
        assert_eq!(k, "description");
        assert_eq!(v, "Implements features.");
    }

    #[test]
    fn parse_kv_strips_single_quotes() {
        // Values wrapped in single-quotes must have the outer quotes removed.
        let (k, v) = parse_kv_line("description: 'Implements features.'").unwrap();
        assert_eq!(k, "description");
        assert_eq!(v, "Implements features.");
    }

    #[test]
    fn parse_kv_keeps_inner_escaped_quotes() {
        // Only ONE balanced outer pair is stripped; inner escaped quotes
        // (`\"`) from an emitted YAML scalar must survive for the caller to
        // unescape. A greedy trim would have eaten the trailing `\"`.
        let (k, v) = parse_kv_line(r#"initialprompt: "He said \"hi\"""#).unwrap();
        assert_eq!(k, "initialprompt");
        assert_eq!(v, r#"He said \"hi\""#);
    }

    #[test]
    fn parse_kv_key_is_lower_cased() {
        // Keys are normalised to lower-case so lookups are case-insensitive.
        let (k, _) = parse_kv_line("Name: foo").unwrap();
        assert_eq!(k, "name");
    }

    // ── none / skip cases ─────────────────────────────────────────────────────

    #[test]
    fn parse_kv_empty_line_returns_none() {
        // Blank lines must be skipped — they carry no key/value data.
        assert!(parse_kv_line("").is_none());
        assert!(parse_kv_line("   ").is_none());
    }

    #[test]
    fn parse_kv_fence_returns_none() {
        // YAML fence markers must be skipped, not parsed as key/value pairs.
        assert!(parse_kv_line("---").is_none());
        assert!(parse_kv_line("  ---  ").is_none());
    }

    #[test]
    fn parse_kv_comment_returns_none() {
        // YAML comment lines must be skipped.
        assert!(parse_kv_line("# this is a comment").is_none());
        assert!(parse_kv_line("  # indented comment").is_none());
    }

    #[test]
    fn parse_kv_malformed_no_colon_returns_none() {
        // A line with no colon at all cannot be split and must return None.
        assert!(parse_kv_line("not_a_kv_line").is_none());
    }

    // ── parse_list_value (DOC-42 `skills:` grammar) ────────────────────────

    #[test]
    fn parse_list_value_bracket_array() {
        let items = parse_list_value("[code-review-standards, systematic-debugging]");
        assert_eq!(items, vec!["code-review-standards", "systematic-debugging"]);
    }

    #[test]
    fn parse_list_value_single_element() {
        assert_eq!(
            parse_list_value("[toolchains-rust-core]"),
            vec!["toolchains-rust-core"]
        );
    }

    #[test]
    fn parse_list_value_empty_brackets_is_empty() {
        assert!(parse_list_value("[]").is_empty());
    }

    #[test]
    fn parse_list_value_blank_is_empty() {
        assert!(parse_list_value("").is_empty());
        assert!(parse_list_value("   ").is_empty());
    }

    #[test]
    fn parse_list_value_trailing_comma_drops_empty_element() {
        assert_eq!(parse_list_value("[a, b, ]"), vec!["a", "b"]);
    }

    #[test]
    fn parse_list_value_unbracketed_bare_name_is_one_element() {
        // Convenience: a bare name with no brackets is a one-element list.
        assert_eq!(
            parse_list_value("toolchains-rust-core"),
            vec!["toolchains-rust-core"]
        );
    }

    #[test]
    fn parse_list_value_strips_quotes_per_element() {
        assert_eq!(parse_list_value(r#"["a", 'b']"#), vec!["a", "b"]);
    }
}
