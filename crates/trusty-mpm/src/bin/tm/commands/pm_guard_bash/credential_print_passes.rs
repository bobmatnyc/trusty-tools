//! When the credential-print scan must rescan a command (#8771), split out of
//! `credential_print` for the 500-SLOC cap.
//!
//! Why: #8734 re-ran the whole scan whenever a pass bound a new tainted name,
//! so a command binding one (`T=$(gcloud auth print-access-token); …`) was
//! scanned twice and refused at half the size. A rescan matters only when a
//! pass judged some text against the name set from before the binding.
//! What: [`stale_after`] answers that from the stages of one pass; [`MAX_PASSES`]
//! bounds how many passes one scan runs.
//! Test: `credential_print_tests::a_bound_command_keeps_the_one_pass_threshold_8771`,
//! `credential_print_tests::a_late_binding_still_rescans_8771`.

use super::MARK;
use crate::commands::hook_rewrite::is_env_assignment;

/// Passes one scan runs before it refuses as unreadable: each rescan follows
/// one more name bound after text that reads it, and the work budget grows by
/// one [`super::WORK_BUDGET`] per top-level pass, so the whole scan stays
/// linear in the command's length.
pub(super) const MAX_PASSES: usize = 4;

/// Words that run text later than its place in the command — a loop body, a
/// function, a trap or alias string, a coprocess, a prompt hook — so a stage
/// before a binding can still run after it.
const DEFERRING_WORDS: &[&str] = &[
    "while",
    "until",
    "for",
    "select",
    "function",
    "trap",
    "alias",
    "coproc",
    "bind",
    "complete",
    "fc",
    "PROMPT_COMMAND",
];

/// Whether the pass that produced `stages` from `flat` judged text against a
/// name set older than the one it ended with, given `first_bind`, the index
/// of the first stage that bound a new name.
///
/// What: true when `flat` defers text ([`DEFERRING_WORDS`], or a `()`
/// function header), when a lifted substitution sits after the binding — its
/// body was judged before any stage ran, though the shell runs it later — or
/// when the binding stage is more than `NAME=value` words (it was judged
/// before its own binding: `T=$(…) printenv T`) or lifts more than one.
/// Fail-closed: anything else in doubt rescans.
pub(super) fn stale_after(flat: &str, stages: &[(String, bool, bool)], first_bind: usize) -> bool {
    let defers = flat.contains("()")
        || flat
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| DEFERRING_WORDS.contains(&w));
    let binder = stages
        .get(first_bind)
        .map_or("", |(stage, ..)| stage.as_str());
    let only_assigns =
        shlex::split(binder.trim()).is_some_and(|words| words.iter().all(|w| is_env_assignment(w)));
    defers
        || !only_assigns
        || binder.matches(MARK).count() > 1
        || stages
            .iter()
            .skip(first_bind + 1)
            .any(|(stage, ..)| stage.contains(MARK))
}
