//! The walk that locates every script a command text runs, for
//! `pm_guard_secret_script` (#8879), split out for the 500-SLOC cap (#10566).
//!
//! Why: the script-body rule judges each script a segment runs; #10566 adds
//! the scripts a `$( … )` or backtick body runs — in the argv text and in an
//! unquoted here-document body — which made the walk its own unit.
//! What: [`each_script`] and the pass it runs ([`each_script_in`], [`Walk`]).
//! Test: `pm_guard_secret_script_tests.rs`, `pm_guard_secret_script_10566_tests.rs`.

use std::path::PathBuf;

use super::{Candidate, Role, Unread, candidates, existing_paths, expand, is_computed};
use super::{locate, segment_bases, unresolved};
use crate::commands::pm_guard_bash::{
    MAX_SUBSTITUTION_DEPTH, Substitution, blank_shell_comments, heredoc_expansions,
    segment_substitution_spans, split_heredoc_bodies, split_shell_segments, tokenize,
};

/// The callback [`each_script`] hands each located (or unresolved) script.
pub(super) type Judge<'a, T> =
    dyn FnMut(&str, Result<PathBuf, Unread>, Option<&str>, &[PathBuf]) -> Option<T> + 'a;

/// Call `judge` on each located script of `command` until one answers. A
/// script the command runs but whose path cannot be resolved is handed over
/// as an [`Unread`] error ([`unresolved`]).
pub(super) fn each_script<T>(
    command: &str,
    bases: &[PathBuf],
    mut judge: impl FnMut(&str, Result<PathBuf, Unread>, Option<&str>, &[PathBuf]) -> Option<T>,
) -> Option<T> {
    let walk = Walk {
        root: command,
        written: command,
        strict: true,
        own: true,
        subs: true,
    };
    each_script_in(command, bases, walk, 0, &mut judge)
}

/// What one [`each_script_in`] pass reads.
#[derive(Clone, Copy)]
pub(super) struct Walk<'a> {
    /// The text whose stages may write a script ([`unresolved`]).
    pub(super) root: &'a str,
    /// The body as written, before its self-location idioms were resolved:
    /// a lenient pass's literal-word test ([`lenient_unresolved`]).
    pub(super) written: &'a str,
    /// Hand every unresolved script over as an [`Unread`] error. A lenient
    /// pass excuses only a literal script word that is missing (residual 10).
    pub(super) strict: bool,
    /// Judge the scripts the text's own segments run.
    pub(super) own: bool,
    /// Judge the scripts its substitution bodies run (#10566).
    pub(super) subs: bool,
}

/// [`each_script`] over `command`, a substitution body `sub_depth` deep.
///
/// #10566: a script run inside a `$( … )` or backtick body (`T=$(bash s.sh)`)
/// was never located, so its credential read allowed. With `walk.subs`, the
/// bodies of every unquoted here-document and of every segment (read with its
/// shell comments blanked, [`blank_shell_comments`]) are judged as commands
/// of their own, first, so the refusal names the script inside. Past
/// [`MAX_SUBSTITUTION_DEPTH`] a body is not followed and refuses
/// ([`Unread::SubstitutionTooDeep`]).
/// Test: `a_script_run_in_a_substitution_is_judged_10566`,
/// `substitutions_nested_past_the_bound_refuse_10566`,
/// `an_unquoted_heredoc_substitution_is_judged_10566`.
pub(super) fn each_script_in<T>(
    command: &str,
    bases: &[PathBuf],
    walk: Walk<'_>,
    sub_depth: usize,
    judge: &mut Judge<'_, T>,
) -> Option<T> {
    let inner = Walk {
        own: true,
        subs: true,
        ..walk
    };
    let (argv_text, _) = split_heredoc_bodies(command);
    let argv_text = if walk.subs {
        // #10566: an expanding here-document runs its substitutions too.
        let expanded = heredoc_expansions(command);
        if let Some(answer) = bodies(expanded, bases, inner, sub_depth, judge) {
            return Some(answer);
        }
        blank_shell_comments(&argv_text)
    } else {
        argv_text
    };
    for segment in split_shell_segments(&argv_text) {
        let trimmed = segment.trim();
        let spans: Vec<Substitution> = if walk.subs {
            segment_substitution_spans(trimmed)
                .into_iter()
                .map(|(_, body)| body)
                .collect()
        } else {
            Vec::new()
        };
        let argv = if walk.own {
            tokenize(trimmed).unwrap_or_default()
        } else {
            Vec::new()
        };
        let (candidates, exact) = candidates(&argv);
        if spans.is_empty() && candidates.is_empty() {
            continue;
        }
        let seg_bases = segment_bases(&argv_text, trimmed, bases);
        if let Some(answer) = bodies(Some(spans), &seg_bases, inner, sub_depth, judge) {
            return Some(answer);
        }
        for (word, path, interpreter) in locate(&candidates, &seg_bases) {
            if let Some(answer) = judge(&word, Ok(path), interpreter.as_deref(), &seg_bases) {
                return Some(answer);
            }
        }
        // #8879: an unresolvable or missing script fails closed at the top level.
        let why = if walk.strict {
            unresolved(&candidates, &seg_bases, walk.root)
        } else {
            lenient_unresolved(&candidates, &seg_bases, walk.written)
        };
        if exact
            && let Some((word, why)) = why
            && let Some(answer) = judge(&word, Err(why), None, &seg_bases)
        {
            return Some(answer);
        }
    }
    None
}

/// Judge each substitution body one level deeper; `None` bodies (a
/// here-document walk past its own bound) and a body past
/// [`MAX_SUBSTITUTION_DEPTH`] refuse ([`Unread::SubstitutionTooDeep`]).
fn bodies<T>(
    found: Option<Vec<Substitution>>,
    bases: &[PathBuf],
    walk: Walk<'_>,
    sub_depth: usize,
    judge: &mut Judge<'_, T>,
) -> Option<T> {
    let too_deep =
        |judge: &mut Judge<'_, T>| judge("$( … )", Err(Unread::SubstitutionTooDeep), None, bases);
    let Some(found) = found else {
        return too_deep(judge);
    };
    for body in found {
        let answer = if sub_depth >= MAX_SUBSTITUTION_DEPTH {
            // #10566: the guard cannot see what a body this deep runs.
            too_deep(judge)
        } else {
            each_script_in(body.text(), bases, walk, sub_depth + 1, judge)
        };
        if answer.is_some() {
            return answer;
        }
    }
    None
}

/// The lenient pass's unresolved script (#10566, residual 10): an
/// interpreter's script or loaded file that does not exist refuses as
/// [`Unread::Unresolvable`] when its word is computed (`"$B"`, a glob,
/// `<(…)`), names a `/dev/fd/` or `/proc/self/fd/` pipe, or was not written
/// literally in the body (a resolved variable). Only a literal word that is
/// missing — prose and here-documents quote code in backticks — allows.
/// Test: `a_computed_script_in_a_body_substitution_refuses_10566`.
fn lenient_unresolved(
    candidates: &[Candidate],
    bases: &[PathBuf],
    written: &str,
) -> Option<(String, Unread)> {
    let script = candidates
        .iter()
        .find(|c| c.role == Role::Positional)
        .or_else(|| candidates.iter().find(|c| c.role == Role::Stdin));
    let loaded = candidates.iter().filter(|c| c.role == Role::Loaded);
    loaded
        .chain(script)
        .filter(|c| c.interpreter.is_some() && existing_paths(&c.word, bases).is_empty())
        .find(|c| {
            let word = expand(&c.word, bases);
            is_computed(&word)
                || ["/dev/fd/", "/proc/self/fd/"]
                    .iter()
                    .any(|p| word.starts_with(p))
                || !written_literally(written, &c.word)
        })
        .map(|c| (c.word.clone(), Unread::Unresolvable))
}

/// Whether `word` appears in `text` as a word of its own — not as the value
/// of an assignment (`B=./b.sh`).
fn written_literally(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        at == 0
            || matches!(
                text.as_bytes()[at - 1],
                b' ' | b'\t' | b'\n' | b'`' | b'(' | b'"' | b'\''
            )
    })
}
