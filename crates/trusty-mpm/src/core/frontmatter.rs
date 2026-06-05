//! Shared frontmatter key/value line parser.
//!
//! Why: Two independent copies of a `split_once(':')` frontmatter parser
//! existed in `agent_builder` and `delegation_authority`; both silently
//! corrupted or hard-errored on values that legitimately contain a colon
//! (e.g. URLs, ISO-8601 timestamps, model ids like
//! `bedrock/us.anthropic.claude-sonnet-4-6`). A single shared function
//! eliminates the divergence and fixes the bug for both call sites.
//! What: [`parse_kv_line`] splits a frontmatter line on the *first* colon
//! only, trims the key and value, strips optional surrounding quotes from the
//! value, and returns `Some((key, value))`; it returns `None` for blank lines,
//! comment lines, and YAML fence markers (`---`).
//! Test: `cargo test -p trusty-mpm -- frontmatter` covers URL values,
//! timestamp values, model-id values, normal key/value pairs, trailing
//! whitespace, and malformed/comment/empty lines.

/// Parse one frontmatter line into a `(key, value)` pair.
///
/// Why: values in YAML-ish frontmatter often contain colons — URLs, timestamps,
/// model ids — so splitting on the first colon and preserving the rest is the
/// only correct interpretation.
/// What: splits `line` on the first `':'`, lower-cases and trims the key, trims
/// the value and strips one layer of surrounding `"` or `'` quotes. Returns
/// `None` when the line is blank, a comment (`#`), a fence (`---`), or has no
/// colon at all.
/// Test: `frontmatter::tests::parse_kv_*` in this file.
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

    let value = raw_value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();

    Some((key, value))
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
}
