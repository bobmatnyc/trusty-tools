//! What a chained Bash call is, for the opportunity ledger (#11507).
//!
//! Why: the observer named a Bash row by `effective_tool_name`, which for
//! `cd X && cargo test` is `cd` — 5.5M tokens a day were logged under `cd`,
//! and the ledger kept no command text, so chained shapes could only be
//! guessed from first tokens.
//! What: [`dominant_step`] names the step a chained row is measured under;
//! [`command_shape`] is the chain's separators and program names only — no
//! argument, path or quoted text — so the next measurement counts shapes.
//! Test: `the_dominant_step_names_a_chain_not_cd`,
//! `the_command_shape_keeps_only_separators_and_programs`.

use super::compress_chain::parse_chain_spec;
use super::hook_rewrite::{compressible_tool_decision, effective_tool_name, first_command_token};
use super::hook_rewrite_chain::separator_after;
use super::pm_guard_bash::split_shell_segments_raw;

/// Programs that run no step worth naming a row after.
const INERT: &[&str] = &["cd", "pushd", "popd", "true", "export", "echo", "date"];

/// The tool a chained `command`'s row is named after; `None` for one segment.
///
/// What: for a `tm compress --chain` rewrite, the last covered step in its
/// spec. Otherwise, past `cd` and the other [`INERT`] programs and any reader
/// after a `|`: the last filter-covered step, else the first remaining step's
/// [`effective_tool_name`].
/// Test: `the_dominant_step_names_a_chain_not_cd`.
pub(crate) fn dominant_step(command: &str) -> Option<String> {
    if let Some(spec) = chain_spec(command) {
        return parse_chain_spec(spec).into_iter().flatten().last();
    }
    let segments = split_shell_segments_raw(command);
    if segments.len() < 2 {
        return None;
    }
    let (mut covered, mut first) = (None, None);
    for (i, segment) in segments.iter().enumerate() {
        let text = segment.trim();
        if i > 0 && separator_after(command, segments[i - 1], segment) == "|" {
            continue;
        }
        let program = first_command_token(text).unwrap_or_default();
        if text.is_empty() || INERT.contains(&program.as_str()) {
            continue;
        }
        if let Ok(tool) = compressible_tool_decision(text) {
            covered = Some(tool);
        }
        first.get_or_insert_with(|| effective_tool_name(text));
    }
    covered.or(first).filter(|t| !t.is_empty())
}

/// The sanitised shape of a chained `command`; `None` for one segment.
///
/// What: each segment's program basename (letters, digits, `-_.` only, else
/// `?`), joined by the separator as written; a line break reads `NL`.
/// Test: `the_command_shape_keeps_only_separators_and_programs`.
pub(crate) fn command_shape(command: &str) -> Option<String> {
    let segments = split_shell_segments_raw(command);
    if segments.len() < 2 {
        return None;
    }
    let mut shape = String::new();
    for (i, segment) in segments.iter().enumerate() {
        if i > 0 {
            let sep = separator_after(command, segments[i - 1], segment);
            shape.push_str(&format!(" {} ", if sep.is_empty() { "NL" } else { sep }));
        }
        let program = first_command_token(segment.trim()).unwrap_or_default();
        let clean = !program.is_empty()
            && program.len() <= 32
            && program
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        shape.push_str(if clean { &program } else { "?" });
    }
    Some(shape)
}

/// The step spec of a `tm compress --tool "<spec>" --chain` rewrite.
fn chain_spec(command: &str) -> Option<&str> {
    let (_, after) = command.split_once("| tm compress --tool \"")?;
    let (spec, rest) = after.split_once('"')?;
    rest.trim_start().starts_with("--chain ").then_some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #11507: on the pre-fix commit every one of these rows was named `cd`.
    #[test]
    fn the_dominant_step_names_a_chain_not_cd() {
        for (command, tool) in [
            ("cd x && cargo build && cargo test -p x", "cargo test"),
            ("cd x; git diff", "git diff"),
            ("cd x && sed -n 1,5p f.txt", "sed -n"),
            ("cd x && git diff | head -5", "git diff"),
            (
                "cd x && { git diff; printf M; } | tm compress --tool \"=cd ; git diff && cargo test\" --chain ab",
                "cargo test",
            ),
        ] {
            assert_eq!(dominant_step(command).as_deref(), Some(tool), "{command}");
        }
        assert_eq!(dominant_step("git diff"), None);
        assert_eq!(dominant_step("cd x && cd y"), None);
    }

    #[test]
    fn the_command_shape_keeps_only_separators_and_programs() {
        assert_eq!(
            command_shape("cd /secret/path && cargo test -p x || grep -n 'token' f | head\nls")
                .as_deref(),
            Some("cd && cargo || grep | head NL ls")
        );
        assert_eq!(command_shape("git diff --stat"), None);
        assert_eq!(command_shape("a=1 $(x) ; ls").as_deref(), Some("? ; ls"));
    }
}
