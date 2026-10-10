//! Parser messages with every quoted input value withheld.
//!
//! Why: serde quotes the value it could not read (`invalid type: string
//! "xoxb-…"`), so a token typed into the wrong key of the host file or a
//! project file would reach an error and every finding built from it
//! (#8454). One rule serves the YAML host parser and the TOML project parser.
//! What: [`withhold`] rewrites a message; the callers pass the parser's
//! location.
//! Test: `host_faults_deny_all`, `host_unknown_key_denies_all`,
//! `project_parse_errors_withhold_input_values`.

/// serde messages whose `, expected …` tail is written by this code's types.
const EXPECTING: [&str; 5] = [
    "invalid type: ",
    "invalid value: ",
    "invalid length ",
    "unknown variant ",
    "unknown field ",
];

/// A parser message with every value it quotes withheld.
///
/// Why: see the module doc.
/// What: `missing field` and `duplicate field` name a field of this code's
/// types and stay. Every duplicate-key message becomes a fixed text with its
/// position. A message with a code-written `, expected …` tail keeps the
/// tail and replaces the span from its first to its last quote mark (`"`,
/// `'` or `` ` ``) before it. Any other message that quotes something
/// becomes a fixed text with its position. A message that quotes nothing,
/// such as a libyaml syntax error, stays. A multi-line message keeps its
/// first line only, so a source excerpt never passes.
/// Test: `host_faults_deny_all`, `project_parse_errors_withhold_input_values`.
pub(crate) fn withhold(msg: &str, at: Option<(usize, usize)>) -> String {
    let msg = msg.lines().next().unwrap_or_default().trim_end();
    if msg.starts_with("missing field `") || msg.starts_with("duplicate field `") {
        return msg.to_string();
    }
    let withheld = |what: &str| {
        let at = at
            .map(|(line, column)| format!(" at line {line} column {column}"))
            .unwrap_or_default();
        format!("{what}{at} (value withheld)")
    };
    // #8454: a number, null or collection key is printed unquoted, and the
    // key-path prefix can carry input text, so no duplicate keeps its text.
    if msg.contains("duplicate entry ") || msg.contains("duplicate key") {
        return withheld("a mapping repeats a key");
    }
    let (head, tail) = match msg.rfind(", expected ") {
        Some(i) if EXPECTING.iter().any(|p| msg.starts_with(p)) => msg.split_at(i),
        _ => (msg, ""),
    };
    let quote = |c: char| matches!(c, '"' | '`' | '\'');
    let (Some(first), Some(last)) = (head.find(quote), head.rfind(quote)) else {
        // An unquoted serde value (`invalid type: integer 5`) is a number or
        // a bool; any other unquoted text is the parser's own.
        return msg.to_string();
    };
    if tail.is_empty() {
        return withheld("a quoted value is invalid");
    }
    // Quote marks are ASCII, so `last + 1` is a char boundary.
    format!(
        "{}<value withheld>{}{tail}",
        &head[..first],
        &head[last + 1..]
    )
}

/// The 1-based line and column of byte `offset` in `text`.
pub(crate) fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}
