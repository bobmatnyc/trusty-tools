//! YAML scalar encoding for the composed frontmatter block.
//!
//! Why: `builder.rs` reached the 500-SLOC production cap while `provenance:`
//! (#4698) was being added to it. These four functions are the one cohesive
//! group in that file that nothing outside the composer calls and that carries
//! no knowledge of agents, inheritance, or `Frontmatter` — they take a `&str`
//! and return a `String`. Splitting them out is the smallest cut that leaves
//! both halves whole.
//! What: [`escape_yaml_double_quoted`] and [`unescape_yaml_double_quoted`] are
//! exact inverses, so a compose -> deploy -> re-compose cycle round-trips a
//! value verbatim; [`needs_quoting`] decides whether a scalar must be quoted at
//! all, and [`render_scalar`] applies that decision. Every item is
//! `pub(crate)` — this is the composer's private encoding, not a public API.
//! Test: the round-trip and quoting tests live with the composer they serve, in
//! `builder_tests.rs`: `initial_prompt_with_embedded_quote_round_trips`,
//! `initial_prompt_with_backslash_round_trips`,
//! `description_with_embedded_newline_round_trips`,
//! `compose_description_with_colon_is_quoted_and_strict_yaml_valid`,
//! `compose_description_without_colon_is_unquoted`.

/// Escape a string for emission inside a YAML double-quoted scalar.
///
/// Why: `merge_frontmatter` wraps the `initialPrompt` value — and, since
/// issue #3556, any other scalar [`render_scalar`] decided needs quoting —
/// in double-quotes. A raw `"` or `\` in the value would otherwise terminate
/// the quote early or be misread as an escape, and a raw embedded newline
/// would break the single-line frontmatter grammar entirely; either produces
/// malformed YAML. claude-mpm parity requires the emitted frontmatter always
/// be parseable.
/// What: a single pass over `value`'s characters that escapes `\` → `\\`,
/// `"` → `\"`, and a literal newline → the two-character sequence `\n` (so a
/// multi-line description/model/etc. value still composes to one physical
/// frontmatter line). Every other character passes through unchanged. The
/// exact inverse of [`unescape_yaml_double_quoted`].
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips`,
/// `description_with_embedded_newline_round_trips` in builder_tests.rs.
pub(crate) fn escape_yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Reverse [`escape_yaml_double_quoted`] on a parsed scalar value.
///
/// Why: a value parsed back from emitted frontmatter still carries the `\"`
/// and `\\` escapes after [`parse_kv_line`] strips the single outer quote pair.
/// Decoding them here makes a compose→deploy→re-compose round-trip yield the
/// original `initialPrompt` string unchanged.
/// What: collapses `\\` → `\`, `\"` → `"`, and `\n` (the two-character
/// escape sequence) → a literal newline; any other `\x` sequence (and a
/// trailing lone `\`) is left verbatim so non-escaped backslashes survive.
/// Test: `initial_prompt_with_embedded_quote_round_trips`,
/// `initial_prompt_with_backslash_round_trips`,
/// `description_with_embedded_newline_round_trips` in builder_tests.rs.
pub(crate) fn unescape_yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether a frontmatter scalar value requires YAML quoting to stay parseable
/// by a strict YAML reader.
///
/// Why (issue #3556): `merge_frontmatter` used to emit every scalar field
/// (`name`, `role`, `description`, `model`, `resource_tier`) as a bare plain
/// scalar regardless of content, even though `parse_kv_line` had already
/// stripped any quotes the SOURCE file used. `split_frontmatter`'s own
/// lenient parser (first-colon-only split) tolerates a colon anywhere in the
/// value, so a source template quoting `description: 'Rust 2024 edition
/// specialist: memory-safe systems...'` composed to the exact same UNQUOTED
/// `description: Rust 2024 edition specialist: memory-safe systems...` line
/// regardless of the source's quoting style — invalid YAML a strict parser
/// (`serde_yaml`, used by `trusty-agents::agents::registry::md_agent::parse_md_agent`)
/// rejects with "mapping values are not allowed in this context". Because
/// compose output was invariant to source quoting, re-provisioning alone
/// could never have fixed the 11 affected agents — recomposing reproduced
/// byte-identical broken output. Quoting-on-emit whenever a value is unsafe
/// as a plain scalar fixes it at the one place all consumers share.
/// What: `true` when `value` is empty, opens with a YAML indicator
/// character, has leading/trailing whitespace, contains an embedded newline,
/// is one of the YAML core-schema null tokens (`null`/`Null`/`NULL`/`~`,
/// which would otherwise round-trip through `Option<String>` as `None`
/// instead of the literal string), or contains a `": "` / trailing `:` / a
/// mid-string `" #"` sequence a plain scalar cannot represent. The `" #"`
/// check exists because a space followed by `#` starts a YAML comment
/// ANYWHERE in a plain scalar, not just at the start of the line — the
/// leading-character check above only catches `#` as the very first
/// character, so a value like `Model context protocol #1 tool for
/// delegating` silently truncated to `Model context protocol` at the first
/// `" #"` with no error anywhere in the pipeline (code-critic review of
/// #3556's PR #3565): `compose_agent` succeeds, `validate_frontmatter`
/// accepts it (truncated-but-still-valid YAML is syntactically fine), and
/// the real consumer (`trusty-agents`' `.md` loader) silently drops
/// everything from the `#` onward. Same blind spot as the #3556 root cause
/// (trusty-mpm's own lenient `parse_kv_line` only treats `#` as a comment
/// marker at the start of the trimmed line, so it never notices either) —
/// just a different trigger character.
/// Test: `needs_quoting_true_for_colon_space`, `needs_quoting_true_for_empty`,
/// `needs_quoting_false_for_plain_value`, `needs_quoting_true_for_mid_string_hash_comment`,
/// `needs_quoting_true_for_embedded_newline`, `needs_quoting_true_for_null_tokens`,
/// `needs_quoting_indicator_characters` (table-driven) in builder_tests.rs.
pub(crate) fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if matches!(value, "null" | "Null" | "NULL" | "~") {
        return true;
    }
    let first = value.chars().next().expect("checked non-empty above");
    if "!&*-?|>%@`\"'#,[]{}:".contains(first) {
        return true;
    }
    if value.starts_with(' ') || value.ends_with(' ') {
        return true;
    }
    if value.contains('\n') {
        return true;
    }
    value.contains(": ") || value.ends_with(':') || value.contains(" #")
}

/// Render one frontmatter scalar value, quoting it when [`needs_quoting`]
/// says a plain scalar would not survive a strict YAML parse.
///
/// Why: shared by every scalar field `merge_frontmatter` emits (`name`,
/// `role`, `description`, `model`, `resource_tier`) so the quote-when-needed
/// policy is applied uniformly rather than ad hoc per field (issue #3556).
/// What: returns `value` unchanged when it is safe as a plain scalar;
/// otherwise a double-quoted, escaped scalar via
/// [`escape_yaml_double_quoted`] — the same quoting style `initialPrompt`
/// already used, so `split_frontmatter`'s existing `unescape_yaml_double_quoted`
/// decode (now applied to these fields too) round-trips it.
/// Test: `compose_description_with_colon_is_quoted_and_strict_yaml_valid`,
/// `compose_description_without_colon_is_unquoted` in builder_tests.rs.
pub(crate) fn render_scalar(value: &str) -> String {
    if needs_quoting(value) {
        format!("\"{}\"", escape_yaml_double_quoted(value))
    } else {
        value.to_string()
    }
}
