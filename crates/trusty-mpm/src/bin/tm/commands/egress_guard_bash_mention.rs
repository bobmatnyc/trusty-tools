//! Mention-only Bash commands, which GP-044 passes for every role (#11273).
//!
//! Why: the Bash arm read a family host in ANY word as a destination, so a
//! `gh issue create` whose title named a family host, or a `grep` for one,
//! was denied though it reaches nothing. Owner ruling 2026-11-14 (#11273):
//! the rule judges the destination. A command that only runs programs which
//! open no connection to a host its words name, and run no other program,
//! cannot reach a family endpoint through those words.
//! What: [`mention_only`] is true only when every segment of the command, at
//! every nesting depth, is one [`mention_segment`] accepts, and neither the
//! text nor any quote-removed word names a `/dev/tcp/` or `/dev/udp/` path
//! (bash opens a socket for a redirect there). A segment carrying any
//! unquoted `<` or `>` other than a here-document operator ([`has_redirect`])
//! is not mention-only, whatever its target's quoting. A segment is accepted
//! when its program is the first word (no assignment, wrapper, `xargs` or
//! lookup in front) and is:
//! - a text program in [`TEXT_PROGRAMS`], or `rg` without `--pre`;
//! - `git` with a [`GIT_SUBCOMMANDS`] subcommand (none opens a remote
//!   connection) carrying no [`EXEC_OPTIONS`] word, so a commit message,
//!   branch name or path that names a family host is inert;
//! - `gh` with a [`GH_SUBCOMMANDS`] subcommand, carrying no [`EXEC_OPTIONS`]
//!   word, and naming a family host only in the value of a
//!   [`GH_PROSE_OPTIONS`] option that holds whitespace — never in a URL,
//!   `--repo` value or other word, however padded, since `gh` can be pointed
//!   at another host.
//!
//! A program that runs project code (`cargo`, `make`, an interpreter) is not
//! listed: its words can reach whatever that code does with them.
//!
//! A data here-document body holding `$` or a backtick fails the segment,
//! because an unquoted delimiter expands it. Anything else — an unknown
//! program, a lex error, a spent budget — is not mention-only, and the
//! command is judged as before.
//! Test: `mention_only_commands_pass_for_every_role_11273`,
//! `look_alike_mentions_stay_denied_11273`,
//! `quoted_socket_redirects_stay_denied_11273`,
//! `padded_gh_hosts_and_git_repo_options_stay_denied_11273`.

use std::cell::Cell;

use crate::commands::pm_guard_bash::{
    UnplaceableWrite, git_argv_at_subcommand, shell_segments_map, split_heredoc_bodies, tokenize,
};
use crate::commands::program_word::resolve_program_word;

/// Programs that read files or print their words and run nothing else.
const TEXT_PROGRAMS: &[&str] = &[
    "ls", "grep", "egrep", "fgrep", "cat", "head", "tail", "wc", "echo", "printf", "cd", "pwd",
    "true",
];

/// `gh` subcommands that talk only to the GitHub host gh is logged in to.
const GH_SUBCOMMANDS: &[&str] = &[
    "issue", "pr", "run", "workflow", "label", "release", "search", "repo", "project", "status",
];

/// `git` subcommands that open no remote connection.
const GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "checkout",
    "switch",
    "add",
    "commit",
    "restore",
    "rev-parse",
    "merge-base",
    "ls-files",
    "blame",
    "stash",
    "tag",
    "worktree",
    "mv",
    "rm",
    "describe",
    "shortlog",
];

/// `gh` options whose value is free text; only there may a host appear.
const GH_PROSE_OPTIONS: &[&str] = &["--title", "--body", "--notes", "--subject"];

/// Option prefixes through which `gh` or `git` run a program, change the
/// host they reach, or read another repository (and its hooks); any word
/// starting with one fails the segment.
const EXEC_OPTIONS: &[&str] = &[
    "-c",
    "-C",
    "--work-tree",
    "--config",
    "--exec",
    "--upload-pack",
    "--receive-pack",
    "--hostname",
    "-O",
    "--open-files-in-pager",
    "-Z",
    "--git",
    "--index",
    "--registry",
];

/// Paths bash opens a socket for in a redirect.
const SOCKET_PATHS: &[&str] = &["/dev/tcp/", "/dev/udp/"];

/// The error a segment that is not mention-only returns, ending the walk.
const NOT_MENTION: UnplaceableWrite = "a segment that may reach a host its words name";

/// The most segment bytes one [`mention_only`] walk reads.
const BYTE_BUDGET: usize = 1024 * 1024;

/// True when `text` only mentions family hosts and programs (#11273).
///
/// Why / What: see the module doc. `host_in` says whether a word names a
/// family host; the Bash arm passes its own host matcher.
/// Test: `mention_only_commands_pass_for_every_role_11273`,
/// `look_alike_mentions_stay_denied_11273`.
pub(crate) fn mention_only(text: &str, host_in: &dyn Fn(&str) -> bool) -> bool {
    if SOCKET_PATHS.iter().any(|path| text.contains(path)) {
        return false;
    }
    let budget = Cell::new(BYTE_BUDGET);
    let walked = shell_segments_map(text, &|segment: &str| {
        let left = budget.get().checked_sub(segment.len()).ok_or(NOT_MENTION)?;
        budget.set(left);
        if mention_segment(segment, host_in) {
            Ok(Vec::<()>::new())
        } else {
            Err(NOT_MENTION)
        }
    });
    walked.is_ok()
}

/// Whether one command segment is mention-only.
fn mention_segment(segment: &str, host_in: &dyn Fn(&str) -> bool) -> bool {
    if segment.is_empty() {
        return true;
    }
    let (argv_text, bodies) = split_heredoc_bodies(segment);
    // #11273: an unquoted delimiter expands `$(…)` and backticks in a body.
    if bodies.iter().any(|body| body.contains(['$', '`'])) {
        return false;
    }
    // #11273: a redirect opens its target, quoted or not.
    if has_redirect(&argv_text) {
        return false;
    }
    let Ok(argv) = tokenize(&argv_text) else {
        return false;
    };
    if argv
        .iter()
        .any(|word| SOCKET_PATHS.iter().any(|path| word.contains(path)))
    {
        return false;
    }
    let Ok(word) = resolve_program_word(&argv) else {
        return false;
    };
    if word.index != 0 || word.took_options || word.xargs_at.is_some() || word.lookup {
        return false;
    }
    let Some(program) = argv.first().map(|p| p.to_ascii_lowercase()) else {
        return false;
    };
    match program.as_str() {
        p if TEXT_PROGRAMS.contains(&p) => true,
        "rg" => !argv.iter().any(|w| w.starts_with("--pre")),
        "gh" => tool_mention(&argv, argv.get(1), GH_SUBCOMMANDS, Some(host_in)),
        // #11273: no listed git subcommand opens a connection, so a bare host
        // word (a branch name or a path) is inert there.
        "git" => match git_argv_at_subcommand(&argv_text) {
            Some((git_argv, at)) => tool_mention(&argv, git_argv.get(at), GIT_SUBCOMMANDS, None),
            None => false,
        },
        _ => false,
    }
}

/// `gh`/`git`: a listed `subcommand` and no [`EXEC_OPTIONS`] word; with
/// `host_in` set, a family host only in a [`GH_PROSE_OPTIONS`] value that
/// holds whitespace. Any other word naming a host fails, padding included.
fn tool_mention(
    argv: &[String],
    subcommand: Option<&String>,
    listed: &[&str],
    host_in: Option<&dyn Fn(&str) -> bool>,
) -> bool {
    let Some(subcommand) = subcommand else {
        return false;
    };
    if !listed.contains(&subcommand.as_str()) {
        return false;
    }
    let mut prose_next = false;
    argv.iter().skip(1).all(|word| {
        let prose = std::mem::take(&mut prose_next);
        if EXEC_OPTIONS.iter().any(|opt| word.starts_with(opt)) {
            return false;
        }
        let Some(host_in) = host_in else {
            return true;
        };
        if GH_PROSE_OPTIONS.contains(&word.as_str()) {
            prose_next = true;
            return true;
        }
        let joined = word
            .split_once('=')
            .filter(|(option, _)| GH_PROSE_OPTIONS.contains(option))
            .map(|(_, value)| value);
        // #11273: only a prose value may name a host, and only one holding
        // whitespace (no widening past 753969b1b6).
        match joined.or(prose.then_some(word.as_str())) {
            Some(value) => value.contains(char::is_whitespace) || !host_in(word),
            None => !host_in(word),
        }
    })
}

/// Whether `text` holds an unquoted `<` or `>` other than the `<<` of a
/// here-document or the `<<<` of a here-string, neither of which opens a
/// file (#11273). A quoted or backslash-escaped one is text.
///
/// Test: `quoted_socket_redirects_stay_denied_11273`,
/// `mention_only_commands_pass_for_every_role_11273`.
fn has_redirect(text: &str) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
        AnsiC,
    }
    let mut quote = Quote::None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Quote::Single, '\'') | (Quote::AnsiC, '\'') | (Quote::Double, '"') => {
                quote = Quote::None;
            }
            (Quote::Single, _) => {}
            (Quote::None | Quote::Double | Quote::AnsiC, '\\') => {
                chars.next();
            }
            (Quote::Double | Quote::AnsiC, _) => {}
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::None, '$') if chars.peek() == Some(&'\'') => {
                chars.next();
                quote = Quote::AnsiC;
            }
            (Quote::None, '>') => return true,
            (Quote::None, '<') => {
                if chars.peek() != Some(&'<') {
                    return true;
                }
                while chars.peek() == Some(&'<') {
                    chars.next();
                }
            }
            (Quote::None, _) => {}
        }
    }
    false
}

#[cfg(test)]
#[path = "egress_guard_bash_mention_tests.rs"]
mod tests;
