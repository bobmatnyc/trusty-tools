//! The forbidden-verb guard's verdict on substitution bodies (#2745, #8756,
//! #9180).
//!
//! Why: a segment's substitution bodies and the substitutions an unquoted
//! here-document body expands are judged the same way, as commands one level
//! down. Split out of `pm_guard_bash/mod.rs` so that file stays under the
//! 500-SLOC cap when #9180 added the here-document caller.
//! What: [`classify_command_substitutions`] for one segment, and
//! [`judge_bodies`], the shared core.
//! Test: the `evaluate_bash_command_*` substitution rows in `tests.rs`;
//! `continuation_tests::the_forbidden_verb_guard_reads_heredoc_expansions_9180`.

use std::cell::Cell;

use super::substitutions;
use super::{MAX_SUBSTITUTION_DEPTH, SHELL_EDIT_REASON, Substitution, evaluate_bash_command_inner};

/// Substitution bodies one command may judge before it denies (#9180).
///
/// Why: an unquoted `cat` here-document's `$(…)` is judged by the
/// here-document path and again by the segment path, so each nested level
/// doubled the work: 24 levels in ~500 bytes ran 2^24 evaluations, past the
/// hook's 10 s timeout, which then failed every later rule open.
/// What: [`judge_bodies`] spends one unit per body it judges and denies with
/// [`CLASSIFY_WORK_REASON`] once [`Walk::spend`] reports the budget gone, as
/// `MAX_DELETE_WORK` does for the rm-root floor. Real commands judge a few
/// dozen bodies.
pub(super) const MAX_CLASSIFY_WORK: usize = 4096;

/// Deny reason once [`MAX_CLASSIFY_WORK`] is spent (#9180). Not
/// [`SHELL_EDIT_REASON`]: that one is budget-eligible, so the PM's per-turn
/// file-change budget would allow the command the guard could not finish.
pub(crate) const CLASSIFY_WORK_REASON: &str = "this command nests more substitution and \
     here-document bodies than the guard will judge, so it cannot classify what would actually \
     run (#9180). Split it into smaller commands.";

/// The forbidden-verb guard's recursion state: the substitution `depth` and
/// the work counter one command shares across every level (#9180).
#[derive(Clone, Copy)]
pub(super) struct Walk<'a> {
    depth: usize,
    work: &'a Cell<usize>,
}

impl<'a> Walk<'a> {
    /// The depth-0 walk of one command, counting into `work`.
    pub(super) fn new(work: &'a Cell<usize>) -> Self {
        Self { depth: 0, work }
    }

    /// Spends one unit; `false` once [`MAX_CLASSIFY_WORK`] is spent.
    /// Test: `continuation_tests::the_forbidden_verb_guard_counts_its_work_9180`.
    fn spend(self) -> bool {
        self.work.set(self.work.get() + 1);
        self.work.get() <= MAX_CLASSIFY_WORK
    }
}

/// Inspect every substitution in a segment — `$(…)`, `<(…)`, `>(…)`, and
/// backticks — for hidden forbidden verbs.
///
/// Why: first-token classification sees the *outer* command only, so
/// `echo "$(sed -i s/a/b/ f)"` would pass on `echo` while the substitution
/// silently runs `sed`. Since the dangerous direction here is a MISSED deny,
/// this scans substitutions too. Design choice: rather than blanket-deny every
/// substitution (which would break trivial, ubiquitous forms like
/// `echo "$(date)"`), it recursively classifies the *body* of each with
/// [`evaluate_bash_command_inner`] — benign body allows, forbidden one denies.
/// Unbalanced substitutions (an opening `$(`/`<(`/`>(`/backtick with no
/// matching close) deny conservatively, and so does over-deep nesting: an
/// adversarial `$($($(…` chain is bounded by [`MAX_SUBSTITUTION_DEPTH`] so it
/// can never exhaust the stack and crash the guard process (a deny is the safe
/// outcome).
///
/// #2745: process substitution used to be invisible here, so
/// `diff <(sed -i s/a/b/ f) x` was ALLOWED while bash ran the `sed -i`, and
/// `>(…)` denied only incidentally — its leading `>` tripped
/// [`super::has_file_write_redirection`], never its body. Both now classify
/// like every other substitution, via `substitutions::paren_substitution_live_at`.
///
/// #8756: the byte scan moved to [`substitutions::segment_substitutions`], which
/// the secret rules share, so both read the same bodies.
/// What: past the depth cap, returns [`SHELL_EDIT_REASON`] immediately.
/// Otherwise byte-scans for each paren-delimited opener (matching `)` with
/// paren-depth tracking) and for backtick pairs, recursively evaluates each
/// balanced body at `depth + 1`, and propagates a deny; an unbalanced
/// substitution returns [`SHELL_EDIT_REASON`].
/// Test: `evaluate_bash_command_denies_hidden_substitution_verb`,
/// `evaluate_bash_command_allows_benign_substitution`,
/// `evaluate_bash_command_denies_unbalanced_substitution`,
/// `evaluate_bash_command_bounds_deep_substitution_nesting`,
/// `evaluate_bash_command_allows_quoted_substitution_prose`,
/// `evaluate_bash_command_denies_process_substitution_edit`,
/// `evaluate_bash_command_denies_output_process_substitution_by_classification`,
/// `evaluate_bash_command_allows_readonly_process_substitution`,
/// `evaluate_bash_command_denies_unbalanced_process_substitution`.
pub(super) fn classify_command_substitutions(
    segment: &str,
    walk: Walk<'_>,
) -> Option<&'static str> {
    judge_bodies(substitutions::segment_substitutions(segment), walk)
}

/// Judge each substitution body as a command `depth + 1` levels down: the core
/// of [`classify_command_substitutions`], shared with the here-document bodies
/// [`substitutions::heredoc_expansions`] lists (#9180). #9180 critic: each
/// body judged spends one unit of the command's [`Walk`] budget, and a spent
/// budget denies like the depth cap.
/// Test: `continuation_tests::the_forbidden_verb_guard_reads_heredoc_expansions_9180`,
/// `continuation_tests::the_forbidden_verb_guard_counts_its_work_9180`,
/// `continuation_tests::a_doubling_heredoc_nest_denies_in_bounded_time_9180`.
pub(super) fn judge_bodies(bodies: Vec<Substitution>, walk: Walk<'_>) -> Option<&'static str> {
    if walk.depth >= MAX_SUBSTITUTION_DEPTH {
        // Too deeply nested to safely decompose — deny conservatively rather
        // than recurse further and risk a stack overflow.
        return Some(SHELL_EDIT_REASON);
    }
    for body in bodies {
        match body {
            // Unbalanced opener — cannot decompose; deny conservatively.
            Substitution::Unclosed(_) => return Some(SHELL_EDIT_REASON),
            // #9180: a spent budget denies, so nested bodies cannot multiply.
            Substitution::Closed(_) if !walk.spend() => return Some(CLASSIFY_WORK_REASON),
            Substitution::Closed(text) => {
                let deeper = Walk {
                    depth: walk.depth + 1,
                    ..walk
                };
                if let Some(reason) = evaluate_bash_command_inner(&text, deeper) {
                    return Some(reason);
                }
            }
        }
    }
    None
}
