//! Structured-format detection for tool-output compression.
//!
//! Why: Running line-based filters over JSON/YAML/TOML/CSV mangles structure
//! and breaks downstream parsers. Structured payloads must round-trip
//! byte-for-byte, so the dispatcher checks this gate before any filter runs.
//! What: `is_structured_format` plus its YAML/CSV/line heuristics.
//! Test: `is_structured_format_*` in `tool_output::tests`.

/// Detect whether `content` is a structured machine-parseable format.
///
/// Why: Running line-based filters over JSON/YAML/TOML/CSV mangles structure
/// and breaks downstream parsers. Structured payloads must round-trip
/// byte-for-byte.
/// What: Heuristic detection — checks for JSON braces/brackets, YAML doc
/// markers or `key:` lines, TOML `[section]` headers, or consistent CSV
/// comma counts across multiple lines.
/// Test: `is_structured_format_*` in test module.
pub fn is_structured_format(content: &str) -> bool {
    let trimmed = content.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    // JSON: starts with `{` or `[`.
    let first = trimmed.as_bytes()[0];
    if first == b'{' || first == b'[' {
        return true;
    }

    // YAML: explicit doc marker.
    if trimmed.starts_with("---\n") || trimmed == "---" || trimmed.starts_with("---\r") {
        return true;
    }

    // TOML: first non-comment, non-blank line is `[section]` or `[[array]]`.
    if let Some(line) = first_meaningful_line(trimmed)
        && line.starts_with('[')
        && (line.ends_with(']') || line.contains("]\n"))
    {
        return true;
    }

    // YAML / key:value heuristic — first meaningful line is `key: value`
    // (key is alphanumeric/underscore/dash, colon followed by space or EOL).
    if let Some(line) = first_meaningful_line(trimmed)
        && looks_like_yaml_kv(line)
    {
        return true;
    }

    // CSV: at least 3 non-empty lines, all with the same nonzero comma count.
    if looks_like_csv(trimmed) {
        return true;
    }

    false
}

fn first_meaningful_line(s: &str) -> Option<&str> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
}

fn looks_like_yaml_kv(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b':' {
            // Must have key chars before, and either EOL or whitespace after.
            if i == 0 {
                return false;
            }
            let after_ok = i + 1 == bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t';
            return after_ok;
        }
        if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.') {
            return false;
        }
        i += 1;
    }
    false
}

fn looks_like_csv(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).take(8).collect();
    if lines.len() < 3 {
        return false;
    }
    let count = lines[0].matches(',').count();
    if count == 0 {
        return false;
    }
    lines.iter().all(|l| l.matches(',').count() == count)
}
