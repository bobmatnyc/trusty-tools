//! The secret rules applied to every command a Bash command's substitutions
//! run (#8756 round 2).
//!
//! Why: `echo "$(pm2 jlist)"`, `` echo `pm2 jlist` `` and
//! `x=$(launchctl print gui/501/x); echo "$x"` ran a process dump that the
//! argv-reading rules never saw: the lexer hands them `"$(pm2 jlist)"` as one
//! word, or `$(pm2` and `jlist)` as two, and neither names `pm2`. The pod rule
//! had the same blind spot.
//! What: [`evaluate_nested_secret_rules`] runs the pod and process dump rules
//! over the command, then over the body of each substitution
//! `pm_guard_bash::command_substitutions` finds, recursively, with the
//! secret-file rule added at every nested level. The substitution scanner is
//! the one the forbidden-verb guard uses, so both decompose the same bodies to
//! the same depth. An unclosed substitution is read from its opener to the end
//! of the text, so one naming a dump fails closed through the rule's own
//! unlexable arm. Past the depth bound the remaining text is flattened and
//! read once, which denies only when it names a dump. Here-document bodies no
//! program runs as code are blanked before the dump rules read argv, so a
//! commit message or PR body naming `pm2 jlist` is text, and a grouping paren
//! becomes a space, so a subshell group `(pm2 jlist)` reads as `pm2 jlist`.
//!
//! #8875: the `gh api` secret-DELETE rule rides the same walk, so a DELETE in
//! a substitution, a subshell group or a here-document run as code is read too.
//!
//! The credential rule (#8596) is NOT re-run on bodies: it follows
//! substitutions itself, and it allows a value captured into a variable or a
//! header (`TOKEN=$(gcloud auth print-access-token)`), which a body read alone
//! would refuse.
//! Test: `refuses_a_process_dump_inside_a_command_substitution`,
//! `refuses_an_unclosed_substitution_that_names_a_dump`,
//! `bounds_nesting_and_still_refuses_a_dump`,
//! `keeps_the_credential_capture_forms_allowed`.

use crate::commands::pm_guard_bash::{
    MAX_SUBSTITUTION_DEPTH, command_substitutions, evaluate_pod_env_dump_command,
    evaluate_process_env_dump_command, without_inert_heredoc_bodies,
};
use crate::commands::pm_guard_secret_consumers::evaluate_gh_api_secret_delete;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_command;

/// Refuse a pod or process dump anywhere in `command`, or a secret-file read
/// inside one of its substitutions: `Some(reason)` denies.
///
/// Test: `refuses_a_process_dump_inside_a_command_substitution`,
/// `allows_routine_commands_that_carry_a_substitution`.
pub(crate) fn evaluate_nested_secret_rules(command: &str) -> Option<String> {
    evaluate_at(command, 0)
}

/// [`evaluate_nested_secret_rules`] at substitution depth `depth`.
fn evaluate_at(command: &str, depth: usize) -> Option<String> {
    if let Some(reason) = argv_rules(command, depth > 0) {
        return Some(reason);
    }
    command_substitutions(command).iter().find_map(|body| {
        if depth + 1 >= MAX_SUBSTITUTION_DEPTH {
            // #8756: too deep to decompose; read what is left once, flattened.
            argv_rules(&flatten(body.text()), true)
        } else {
            evaluate_at(body.text(), depth + 1)
        }
    })
}

/// The argv-reading rules over one command text; the entry point already ran
/// the secret-file rule on the top-level text, so `with_file_rule` adds it for
/// a body only.
fn argv_rules(command: &str, with_file_rule: bool) -> Option<String> {
    let file = with_file_rule
        .then(|| evaluate_secret_file_read_command(command))
        .flatten();
    let text = ungroup(&without_inert_heredoc_bodies(command));
    file.or_else(|| evaluate_pod_env_dump_command(&text))
        .or_else(|| evaluate_process_env_dump_command(&text))
        // #8875: a `gh api` DELETE of a secret names no secret-shaped word.
        .or_else(|| evaluate_gh_api_secret_delete(&text))
}

/// `text` with each grouping paren turned into a space (#8756).
///
/// Why: a subshell `(pm2 jlist)` lexes as `(pm2` and `jlist)`, which name no
/// program. A `(` right after `$`, `<` or `>` opens a substitution, which
/// [`command_substitutions`] reads instead, so it stays; an escaped `\$(x)`
/// therefore stays the literal word `$(x`.
fn ungroup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = ' ';
    for c in text.chars() {
        let opens_substitution = c == '(' && matches!(prev, '$' | '<' | '>');
        out.push(if matches!(c, '(' | ')') && !opens_substitution {
            ' '
        } else {
            c
        });
        prev = c;
    }
    out
}

/// `text` with its substitution syntax turned into spaces, so every command
/// word inside it is an ordinary word.
fn flatten(text: &str) -> String {
    text.replace(['$', '(', ')', '`'], " ")
}

#[cfg(test)]
#[path = "pm_guard_secret_nested_tests.rs"]
mod tests;
