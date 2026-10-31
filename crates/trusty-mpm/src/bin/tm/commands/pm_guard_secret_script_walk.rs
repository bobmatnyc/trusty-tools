//! The walk that locates every script a command text runs, for
//! `pm_guard_secret_script` (#8879), split out for the 500-SLOC cap (#10566).
//!
//! Why: the script-body rule judges each script a segment runs; #10566 adds
//! the scripts a `$( … )` or backtick body runs, which made the walk its own
//! unit.
//! What: [`each_script`] and the pass it runs ([`each_script_in`], [`Walk`]).
//! Test: `pm_guard_secret_script_tests.rs`, `pm_guard_secret_script_10566_tests.rs`.

use std::path::PathBuf;

use super::{Unread, candidates, locate, segment_bases, unresolved};
use crate::commands::pm_guard_bash::{
    MAX_SUBSTITUTION_DEPTH, split_heredoc_bodies, split_shell_segments, tokenize,
    uncommented_substitution_spans,
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
    /// Hand an unresolved script over as an [`Unread`] error; a lenient pass
    /// judges existing regular files only.
    pub(super) strict: bool,
    /// Judge the scripts the text's own segments run.
    pub(super) own: bool,
    /// Judge the scripts its substitution bodies run (#10566).
    pub(super) subs: bool,
}

/// [`each_script`] over `command`, a substitution body `sub_depth` deep.
///
/// #10566: a script run inside a `$( … )` or backtick body (`T=$(bash s.sh)`)
/// was never located, so its credential read allowed. With `walk.subs`, each
/// segment's substitution bodies are judged as commands of their own, first,
/// so the refusal names the script inside. Past [`MAX_SUBSTITUTION_DEPTH`] a
/// body is not followed and refuses ([`Unread::SubstitutionTooDeep`]).
/// Test: `a_script_run_in_a_substitution_is_judged_10566`,
/// `substitutions_nested_past_the_bound_refuse_10566`.
pub(super) fn each_script_in<T>(
    command: &str,
    bases: &[PathBuf],
    walk: Walk<'_>,
    sub_depth: usize,
    judge: &mut Judge<'_, T>,
) -> Option<T> {
    let (argv_text, _) = split_heredoc_bodies(command);
    for segment in split_shell_segments(&argv_text) {
        let trimmed = segment.trim();
        let spans = if walk.subs {
            uncommented_substitution_spans(trimmed)
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
        let inner = Walk {
            own: true,
            subs: true,
            ..walk
        };
        for (_, body) in spans {
            let answer = if sub_depth >= MAX_SUBSTITUTION_DEPTH {
                // #10566: the guard cannot see what a body this deep runs.
                let why = Err(Unread::SubstitutionTooDeep);
                judge("$( … )", why, None, &seg_bases)
            } else {
                each_script_in(body.text(), &seg_bases, inner, sub_depth + 1, judge)
            };
            if answer.is_some() {
                return answer;
            }
        }
        for (word, path, interpreter) in locate(&candidates, &seg_bases) {
            // #10566: a lenient pass judges regular files only (residual 10).
            if !walk.strict && !path.is_file() {
                continue;
            }
            if let Some(answer) = judge(&word, Ok(path), interpreter.as_deref(), &seg_bases) {
                return Some(answer);
            }
        }
        // #8879: an unresolvable or missing script fails closed at the top level.
        if walk.strict
            && exact
            && let Some((word, why)) = unresolved(&candidates, &seg_bases, walk.root)
            && let Some(answer) = judge(&word, Err(why), None, &seg_bases)
        {
            return Some(answer);
        }
    }
    None
}
