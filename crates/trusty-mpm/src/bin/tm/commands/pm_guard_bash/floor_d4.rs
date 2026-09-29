//! The D4-remainder floor of `tm hook --pm-guard` (#8878, D4 and D8).
//!
//! Why: design of record D4 adds three hard-floor classes beside the
//! trust-anchor rule — network exfiltration, destructive disk tools, and a
//! force-push to the default branch — and D8 makes them hold under
//! `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`. The deny-sets
//! are the ones proposed on #8878 (comment 5889247312); the process-bound
//! Architect is exempt, which `pm_guard_floor` decides, not this module.
//! What: [`evaluate_d4_floor`] walks every segment of a Bash command — through
//! `sh -c` / `env -S` / `xargs` / `eval` wrappers, remembering which segment
//! reads a pipe — and returns the first deny from [`super::floor_d4_rules`]
//! (uploads and disk tools) or [`super::force_push`].
//! FAIL-CLOSED: a command the guard cannot classify, or a segment that does not
//! lex, denies when its text names any program this floor covers. A wrapper
//! followed by a flag (`sudo -u root dd …`) makes every token a candidate
//! program.
//! Residual: a script file the command runs, a shell function or alias, and a
//! program reached through an interpreter (`python -c`) are not seen.
//! Test: `floor_d4_tests.rs`.

use std::path::{Path, PathBuf};

use super::floor_d4_rules::{disk_tool_reason, exfiltration_reason};
use super::force_push::{GitProbe, force_push_reason};
use super::shell_lex::{WrappedCommand, wrapped_command};
use super::{MAX_WRAPPER_DEPTH, split_shell_segments_raw, unclassifiable_command};
use crate::commands::hook_rewrite::{COMMAND_WRAPPERS, first_command_token, strip_wrapper_prefix};

/// The words whose presence makes an unparseable command a D4 candidate.
const D4_WORDS: &[&str] = &[
    "curl", "wget", "scp", "rsync", "nc", "ncat", "netcat", "ssh", "diskutil", "dd", "mkfs",
    "newfs", "fdisk", "hdiutil", "asr", "push",
];

/// Tokens after which the next token runs as a program (`find -exec`).
const EXEC_FLAGS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// One command segment, with what the classifier needs to know about it.
#[derive(Debug)]
pub(super) struct Segment {
    /// The segment text.
    pub(super) text: String,
    /// Whether its standard input is a pipe (`a | b`, `a |& b`).
    pub(super) piped: bool,
    /// Whether a `cd` ran earlier in the command, so the directory is unknown.
    pub(super) after_cd: bool,
}

/// Classify a Bash command against the D4-remainder floor: `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command.
/// What: an unclassifiable command naming a [`D4_WORDS`] word denies; then each
/// [`Segment`] is lexed — a lex failure naming a D4 word denies — and judged by
/// the upload, disk-tool and force-push rules in that order. `cwd` is the hook
/// cwd; `git` answers the default and current branch for the force-push rule.
/// Test: `an_upload_of_local_content_is_denied`, `a_destructive_disk_tool_is_denied`,
/// `a_force_push_to_the_default_branch_is_denied`,
/// `an_unparseable_d4_command_is_denied`.
pub(crate) fn evaluate_d4_floor(command: &str, cwd: &Path, git: &dyn GitProbe) -> Option<String> {
    // #8878: fail closed — a command the guard cannot read could be any of these.
    if unclassifiable_command(command).is_some() && mentions_d4_word(command) {
        return Some(unparseable_reason());
    }
    for seg in segments(command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if mentions_d4_word(&seg.text) {
                return Some(unparseable_reason());
            }
            continue;
        };
        let dir: Option<PathBuf> = (!seg.after_cd).then(|| cwd.to_path_buf());
        let reason = exfiltration_reason(&argv, seg.piped)
            .or_else(|| disk_tool_reason(&argv))
            .or_else(|| force_push_reason(&argv, dir.as_deref(), git));
        if reason.is_some() {
            return reason;
        }
    }
    None
}

/// Every segment of `command`, wrapper bodies included, in order.
pub(super) fn segments(command: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut after_cd = false;
    walk(command, false, 0, &mut after_cd, &mut out);
    out
}

/// Depth-bounded core of [`segments`]; `piped_in` is inherited by the first
/// segment, as a wrapper's stdin is its first command's.
fn walk(command: &str, piped_in: bool, depth: usize, after_cd: &mut bool, out: &mut Vec<Segment>) {
    for (n, raw) in split_shell_segments_raw(command).into_iter().enumerate() {
        let piped = (n == 0 && piped_in) || follows_pipe(command, raw);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_command_token(trimmed).as_deref() == Some("cd") {
            *after_cd = true;
        }
        out.push(Segment {
            text: trimmed.to_string(),
            piped,
            after_cd: *after_cd,
        });
        if depth < MAX_WRAPPER_DEPTH
            && let WrappedCommand::Inner(inner) = wrapped_command(trimmed)
        {
            walk(&inner, piped, depth + 1, after_cd, out);
        }
    }
}

/// Whether `segment`, a sub-slice of `command`, follows a `|` or `|&`.
fn follows_pipe(command: &str, segment: &str) -> bool {
    let offset = (segment.as_ptr() as usize).wrapping_sub(command.as_ptr() as usize);
    let Some(before) = command.get(..offset) else {
        return false;
    };
    let before = before.trim_end();
    (before.ends_with('|') && !before.ends_with("||")) || before.ends_with("|&")
}

/// Whether `text` names a [`D4_WORDS`] word, split on every non-alphanumeric.
fn mentions_d4_word(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| D4_WORDS.contains(&word))
}

/// The indices of `argv` that run as a program, and each one's basename.
///
/// What: the position past env assignments and [`COMMAND_WRAPPERS`], plus
/// every token right after a wrapper word, a `timeout` duration, or a
/// [`EXEC_FLAGS`] flag. A wrapper followed by a flag makes EVERY token a
/// candidate — fail closed.
/// Test: `a_wrapped_program_is_still_found`.
pub(super) fn program_positions(argv: &[String]) -> Vec<(usize, String)> {
    let base = |tok: &str| {
        let tok = tok.strip_prefix('\\').unwrap_or(tok);
        tok.rsplit('/').next().unwrap_or(tok).to_string()
    };
    let Some(first) = strip_wrapper_prefix(argv) else {
        return argv.iter().enumerate().map(|(i, t)| (i, base(t))).collect();
    };
    let mut out: Vec<(usize, String)> = Vec::new();
    for (i, tok) in argv.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| base(&argv[p]));
        let after_timeout = i >= 2 && base(&argv[i - 2]) == "timeout";
        let in_position = i == first
            || prev
                .as_deref()
                .is_some_and(|p| COMMAND_WRAPPERS.contains(&p) || EXEC_FLAGS.contains(&p))
            || after_timeout;
        if in_position && !tok.starts_with('-') {
            out.push((i, base(tok)));
        }
    }
    out
}

/// The deny for a command the guard cannot read.
fn unparseable_reason() -> String {
    format!(
        "Hard-floor deny (#8878 D4): this command names a network-upload, disk or `git push` \
         program, but the guard cannot parse it, so it cannot rule out an upload, a disk erase \
         or a force-push to the default branch. {D4_REMEDY}"
    )
}

/// The closing sentence every D4 deny carries.
pub(super) const D4_REMEDY: &str = "This floor binds every session but the Architect's main \
     thread, and `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift it. \
     Ask the operator, or run it from the Architect session.";

#[cfg(test)]
#[path = "floor_d4_tests.rs"]
mod tests;
