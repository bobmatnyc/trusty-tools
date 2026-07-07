//! Minimal `---`-fenced frontmatter parser for `SKILL.md` files.
//!
//! Why: `trusty-code` does not depend on `trusty-mpm` (separate products —
//! see the crate-level docs), so its own lightweight parser is needed rather
//! than reusing `trusty_mpm::core::frontmatter`. Skill metadata only ever
//! needs a couple of scalar keys (`name`, `description`), so this is
//! deliberately a small subset of full YAML: flat `key: value` lines between
//! two `---` fences, first-colon-only splitting (so URLs/model-ids/timestamps
//! in a value survive), and a single optional surrounding-quote strip.
//! What: [`parse_frontmatter`] returns the fenced key/value map;
//! [`strip_frontmatter`] returns everything after the closing fence.
//! Test: `frontmatter::tests::*` — happy path, no-fence input, unterminated
//! fence, quoted values, blank/comment lines inside the fence.

use std::collections::HashMap;

/// Parse the `---`-fenced frontmatter block at the start of `raw` into a
/// flat key/value map.
///
/// Why: Discovery needs `name`/`description` without reading (or caring
/// about) the Markdown body that follows.
/// What: Returns an empty map when `raw` does not open with a `---` fence
/// line, or when no closing fence is found (malformed input degrades to "no
/// metadata", never a panic). Each line between the fences is parsed by
/// [`parse_kv_line`]; blank lines and `#` comments are skipped.
/// Test: `frontmatter::tests::parse_frontmatter_reads_name_and_description`,
/// `parse_frontmatter_no_opening_fence_is_empty`,
/// `parse_frontmatter_unterminated_fence_is_empty`.
pub fn parse_frontmatter(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut lines = raw.lines();

    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return map,
    }

    for line in lines {
        if line.trim() == "---" {
            return map;
        }
        if let Some((k, v)) = parse_kv_line(line) {
            map.insert(k, v);
        }
    }

    // No closing fence found — treat as if no frontmatter were present.
    HashMap::new()
}

/// Return the slice of `raw` after the closing `---` fence, or `raw` itself
/// when there is no well-formed fence pair.
///
/// Why: The lazy body loader wants the Markdown body only, not the
/// frontmatter block that discovery already consumed.
/// What: Walks lines tracking whether the opening fence was seen; on the
/// second `---` line, returns everything after it. Falls back to the whole
/// input when the fence is absent or unterminated.
/// Test: `frontmatter::tests::strip_frontmatter_removes_fenced_block`,
/// `strip_frontmatter_returns_whole_input_when_no_fence`.
pub fn strip_frontmatter(raw: &str) -> &str {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return raw;
    }

    let mut saw_open = false;
    let mut consumed = 0usize;
    for line in trimmed.split_inclusive('\n') {
        consumed += line.len();
        if line.trim_end_matches('\n').trim() == "---" {
            if saw_open {
                return &trimmed[consumed..];
            }
            saw_open = true;
        }
    }

    // Opening fence with no closing fence — malformed; treat whole input as body.
    raw
}

/// Parse one frontmatter line into a `(key, value)` pair.
///
/// Why: Values may legitimately contain colons (URLs, timestamps); splitting
/// on the *first* colon only preserves them.
/// What: Returns `None` for blank lines and `#` comments. Trims the key,
/// trims the value, and strips one balanced surrounding `"`/`'` quote pair.
/// Test: `frontmatter::tests::parse_kv_line_*`.
fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (raw_key, raw_value) = trimmed.split_once(':')?;
    let key = raw_key.trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, strip_one_quote_pair(raw_value.trim()).to_string()))
}

/// Strip at most one balanced pair of surrounding `"` or `'` quotes.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_reads_name_and_description() {
        let raw = "---\nname: demo\ndescription: A demo skill\n---\nbody\n";
        let map = parse_frontmatter(raw);
        assert_eq!(map.get("name").map(String::as_str), Some("demo"));
        assert_eq!(
            map.get("description").map(String::as_str),
            Some("A demo skill")
        );
    }

    #[test]
    fn parse_frontmatter_no_opening_fence_is_empty() {
        let map = parse_frontmatter("name: demo\nno fence here\n");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_frontmatter_unterminated_fence_is_empty() {
        let map = parse_frontmatter("---\nname: demo\n(no closing fence)\n");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_frontmatter_skips_blank_and_comment_lines() {
        let raw = "---\n\n# a comment\nname: demo\n---\n";
        let map = parse_frontmatter(raw);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("name").map(String::as_str), Some("demo"));
    }

    #[test]
    fn parse_frontmatter_value_with_colon_survives() {
        let raw = "---\nwhen_to_use: Use when x: y happens\n---\n";
        let map = parse_frontmatter(raw);
        assert_eq!(
            map.get("when_to_use").map(String::as_str),
            Some("Use when x: y happens")
        );
    }

    #[test]
    fn parse_frontmatter_strips_quotes() {
        let raw = "---\nversion: \"1.0.0\"\n---\n";
        let map = parse_frontmatter(raw);
        assert_eq!(map.get("version").map(String::as_str), Some("1.0.0"));
    }

    #[test]
    fn strip_frontmatter_removes_fenced_block() {
        let raw = "---\nname: demo\n---\n# Body\ntext\n";
        let body = strip_frontmatter(raw);
        assert_eq!(body.trim(), "# Body\ntext");
    }

    #[test]
    fn strip_frontmatter_returns_whole_input_when_no_fence() {
        let raw = "# Just a body\ntext\n";
        assert_eq!(strip_frontmatter(raw), raw);
    }

    #[test]
    fn strip_frontmatter_returns_whole_input_when_unterminated() {
        let raw = "---\nname: demo\nno closing fence\n";
        assert_eq!(strip_frontmatter(raw), raw);
    }
}
