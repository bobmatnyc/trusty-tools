//! Which operand an interpreter runs as its script, for `pm_guard_secret_script`
//! (#8879), split out for the 500-SLOC cap (#10672).
//!
//! Why: an interpreter runs its FIRST positional operand; the operands after it
//! are the script's data. The guard judged the first operand that existed, so
//! `$S/venv/bin/python $S/build.py in.docx` skipped the computed `$S/build.py`
//! and refused the `.docx` as a script that is not text (#10672).
//! What: [`script_operand`] picks the script word, [`locate`] and
//! [`fd_pipe_script`] read it, and [`resolve_script_operand`] gives a leading
//! `$NAME` the literal an earlier plain assignment in the same text set.
//! Test: `pm_guard_secret_script_matt_10548_tests.rs`.

use std::path::PathBuf;

use super::{Candidate, Role, existing_paths, expand, is_computed, is_fd_pipe};
use crate::commands::hook_rewrite::is_env_assignment;
use crate::commands::pm_guard_bash::{split_shell_segments_raw, tokenize};

/// The positional candidate the interpreter runs as its script, if any.
///
/// What: the first positional when it exists or is an fd pipe; the operands
/// after it are data (#10672). A first word that names no file — a literal
/// that is missing, or a word computed at run time that
/// [`resolve_script_operand`] could not resolve — falls back to the first
/// later operand that does: an option value the guard does not model
/// (`bash -o pipefail s.sh`) or a wrapper it cannot measure moves the script
/// right, and judging the later file is the refusing direction. A computed
/// first word still refuses afterwards (`unresolved`).
/// Test: `a_venv_python_script_through_a_variable_allows_matt_10672`,
/// `a_venv_python_credential_script_or_docx_script_refuses_10672`,
/// `an_untracked_variable_script_refuses_10672`.
pub(super) fn script_operand<'a>(
    candidates: &'a [Candidate],
    bases: &[PathBuf],
) -> Option<&'a Candidate> {
    let usable = |c: &Candidate| {
        is_fd_pipe(&expand(c.lookup(), bases)) || !existing_paths(c.lookup(), bases).is_empty()
    };
    let mut positional = candidates.iter().filter(|c| c.role == Role::Positional);
    let first = positional.next()?;
    if usable(first) {
        return Some(first);
    }
    positional.find(|c| usable(c))
}

/// The existing files the candidates name: every loaded file, the script
/// operand ([`script_operand`]), else every stdin file.
pub(super) fn locate(
    candidates: &[Candidate],
    bases: &[PathBuf],
) -> Vec<(String, PathBuf, Option<String>)> {
    let found = |c: &Candidate| {
        existing_paths(c.lookup(), bases)
            .into_iter()
            .map(|p| (c.word.clone(), p, c.interpreter.clone()))
            .collect::<Vec<_>>()
    };
    let loaded = candidates.iter().filter(|c| c.role == Role::Loaded);
    let script: Vec<&Candidate> = match script_operand(candidates, bases) {
        Some(c) => vec![c],
        None => candidates
            .iter()
            .filter(|c| c.role == Role::Stdin)
            .collect(),
    };
    loaded.chain(script).flat_map(found).collect()
}

/// The fd-pipe candidate [`locate`] would pick if the pipe existed: any
/// loaded file, the script operand when it is a pipe, else a stdin pipe.
/// #10748: before, the guard probed its own fd N, so such a pipe was read and
/// refused; this keeps it refused, as `Unread::Unresolvable`.
/// Test: `an_fd_pipe_after_a_missing_script_refuses_10748`.
pub(super) fn fd_pipe_script<'a>(
    candidates: &'a [Candidate],
    bases: &[PathBuf],
) -> Option<&'a Candidate> {
    let fd = |c: &Candidate| is_fd_pipe(&expand(c.lookup(), bases));
    let loaded = candidates.iter().find(|c| c.role == Role::Loaded && fd(c));
    let script = match script_operand(candidates, bases) {
        Some(c) => fd(c).then_some(c),
        None => candidates.iter().find(|c| c.role == Role::Stdin && fd(c)),
    };
    loaded.or(script)
}

/// Resolve an interpreter's computed first operand from a plain assignment.
///
/// Why: `S=<dir>; $S/venv/bin/python $S/build.py` names its script only
/// through `$S`; the guard refused it as computed at run time (#10672).
/// What: when the first positional is an interpreter's word starting with
/// `$NAME` or `${NAME}` followed by `/` or nothing, and `segment` is written
/// once in `text` with no `'` or `\`, the word is resolved by
/// [`assigned_literal`]. It is kept only when the resolved path exists; any
/// other case leaves the word computed, and it refuses.
/// Test: `a_venv_python_script_through_a_variable_allows_matt_10672`,
/// `an_untracked_variable_script_refuses_10672`.
pub(super) fn resolve_script_operand(
    candidates: &mut [Candidate],
    text: &str,
    segment: &str,
    bases: &[PathBuf],
) {
    let Some(first) = candidates.iter_mut().find(|c| c.role == Role::Positional) else {
        return;
    };
    if first.interpreter.is_none() || !first.word.starts_with('$') {
        return;
    }
    // A quote or backslash could make `$NAME` literal text; that is not tracked.
    if segment.contains(['\'', '\\']) || !segment.contains(first.word.as_str()) {
        return;
    }
    let rest = &first.word[1..];
    let (name, tail) = match rest.strip_prefix('{') {
        Some(braced) => match braced.split_once('}') {
            Some(split) => split,
            None => return,
        },
        None => rest.split_at(
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len()),
        ),
    };
    let named = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !named || !(tail.is_empty() || tail.starts_with('/')) || is_computed(tail) {
        return;
    }
    let resolved = assigned_literal(text, segment, name).map(|value| format!("{value}{tail}"));
    first.resolved = resolved.filter(|word| !existing_paths(word, bases).is_empty());
}

/// The literal `name` holds when `segment` runs, read from `text`.
///
/// What: walking back from `segment`, every earlier segment up to the one
/// that assigns `name` must be a bare assignment run unconditionally in the
/// current shell — followed by `;`, a newline or `&&`, never `|`, `&` or
/// `||`. The assigning segment must itself start the text or follow `;` or a
/// newline, and its value must be a plain literal ([`plain_literal`]). Any
/// command, compound keyword, subshell, function or conditional between them
/// answers `None`, as does a latest assignment that is not a plain literal.
/// `segment` must be one raw segment written exactly once in `text`, so a
/// wrapper's inner command (`bash -c '…'`) never resolves from the outer shell.
/// Test: `an_untracked_variable_script_refuses_10672`.
fn assigned_literal(text: &str, segment: &str, name: &str) -> Option<String> {
    if text.matches(segment).count() != 1 {
        return None;
    }
    let raws = split_shell_segments_raw(text);
    let at = raws.iter().position(|raw| raw.trim() == segment)?;
    let start = |raw: &str| raw.as_ptr() as usize - text.as_ptr() as usize;
    let separator_after = |k: usize| {
        let end = start(raws[k]) + raws[k].len();
        let next = raws.get(k + 1).map_or(text.len(), |raw| start(raw));
        text.get(end..next).unwrap_or("|").trim()
    };
    for k in (0..at).rev() {
        if !matches!(separator_after(k), ";" | "" | "&&") {
            return None;
        }
        let words = tokenize(raws[k].trim()).ok()?;
        if words.is_empty() || !words.iter().all(|w| is_env_assignment(w)) {
            return None;
        }
        // Bare assignments apply left to right: the last one for `name` wins.
        let Some(value) = words.iter().rev().find_map(|w| {
            w.split_once('=')
                .filter(|(key, _)| *key == name)
                .map(|(_, v)| v)
        }) else {
            continue;
        };
        let unconditional = k == 0 || matches!(separator_after(k - 1), ";" | "");
        return (unconditional && plain_literal(value)).then(|| value.to_string());
    }
    None
}

/// Whether an assignment's unquoted value is a plain literal: non-empty, and
/// every character a letter, digit, non-ASCII, space or one of `/._-+,:@%=_`.
/// No expansion, glob, tilde or substitution survives this.
fn plain_literal(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || !c.is_ascii() || "/._-+,:@%= _".contains(c))
}

#[cfg(test)]
mod tests {
    use super::plain_literal;

    /// Only a value with no expansion, glob, tilde or quote is plain.
    #[test]
    fn plain_literal_refuses_every_shell_expansion_10672() {
        assert!(plain_literal("/var/folders/a_b/T/.tmp-x/scratch pad"));
        for value in [
            "", "$X", "~/x", "a*", "a?", "[a]", "{a,b}", "$(x)", "`x`", "a'b", "a\"b",
        ] {
            assert!(!plain_literal(value), "{value:?} must not be plain");
        }
    }
}
