//! A `for` loop whose words only reach a GitHub search, for the #7266
//! secret-file read guard (#9001 case 3).
//!
//! Why: `for k in … ".env.*" …; do echo "== $k"; gh issue list --search "$k";
//! done` was refused for naming `.env.*`, though the loop opens no file: its
//! words reach only `echo` and a GitHub query. The word list is refused today
//! because the loop body decides what the words become, and the guard cannot
//! follow the variable in general.
//! What: [`only_feeds_a_search`] follows it in one strict shape. The whole
//! command must be that one loop: its header, then segments that are each
//! `echo`, `gh issue|pr list` or `done`. Every `$` in the command must be the
//! loop variable, used as an `echo` argument or as the whole `--search` value.
//! FAIL-CLOSED: anything else keeps the deny — a second loop or any other
//! program, a pipe, a redirect, `&`, a paren or brace, a here-document, a
//! substitution, `${…}`, an unbalanced quote, or a `$` that is not the loop
//! variable. Neither `echo` nor a gh search opens a path, and a glob in the
//! word list expands to names, never to a file's bytes.
//! Test: `a_for_loop_feeding_only_a_gh_search_names_no_file_9001`,
//! `a_for_loop_whose_words_reach_anything_else_still_denies_9001`.

use crate::commands::pm_guard_bash::{split_shell_segments, tokenize};
use crate::commands::pm_guard_secret_positions::{LIST_INTRODUCERS, lists_gh_issues};
use crate::commands::pm_guard_secret_read::NESTED_COMMAND_MARKERS;

/// Whether `header`, a `for` header segment of `command`, binds words that
/// reach only `echo` and a `gh issue|pr list --search` value (#9001).
///
/// What: see the module doc.
/// Test: `a_for_loop_feeding_only_a_gh_search_names_no_file_9001`,
/// `a_for_loop_whose_words_reach_anything_else_still_denies_9001`.
pub(crate) fn only_feeds_a_search(command: &str, header: &str) -> bool {
    let Some(var) = loop_variable(header) else {
        return false;
    };
    if command.contains("<<")
        || NESTED_COMMAND_MARKERS.iter().any(|m| command.contains(m))
        || !plain_separators_only(command)
    {
        return false;
    }
    let (mut headers, mut uses) = (0, 0);
    for segment in split_shell_segments(command) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let Ok(argv) = tokenize(segment) else {
            return false;
        };
        let at = argv
            .iter()
            .position(|t| !LIST_INTRODUCERS.contains(&t.as_str()))
            .unwrap_or(argv.len());
        let words = &argv[at..];
        let found = match words.first().map(String::as_str) {
            Some("for") if segment == header.trim() => {
                headers += 1;
                (!words.iter().any(|w| w.contains('$'))).then_some(0)
            }
            Some("done") if words.len() == 1 => Some(0),
            Some("echo") => echo_uses(&words[1..], &var),
            Some("gh") if lists_gh_issues(words) => search_uses(words, &var),
            _ => None,
        };
        let Some(found) = found else {
            return false;
        };
        uses += found;
    }
    headers == 1 && uses == command.matches('$').count()
}

/// The variable a `for <var> in …` header binds, when it is an identifier.
fn loop_variable(header: &str) -> Option<String> {
    let argv = tokenize(header.trim()).ok()?;
    let at = argv
        .iter()
        .position(|t| !LIST_INTRODUCERS.contains(&t.as_str()))?;
    let var = argv.get(at + 1)?;
    let ident = var.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (argv[at] == "for" && argv.get(at + 2)? == "in" && ident).then(|| var.clone())
}

/// The `$var` uses in `echo` arguments; `None` when any `$` is another name.
fn echo_uses(args: &[String], var: &str) -> Option<usize> {
    let mut uses = 0;
    for arg in args {
        for (at, _) in arg.match_indices('$') {
            let tail = arg[at + 1..].strip_prefix(var)?;
            if tail
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return None;
            }
            uses += 1;
        }
    }
    Some(uses)
}

/// The `$var` uses of a gh call, each the whole `--search` value; `None` when
/// a `$` stands anywhere else.
fn search_uses(argv: &[String], var: &str) -> Option<usize> {
    let whole = format!("${var}");
    let mut uses = 0;
    for (at, token) in argv.iter().enumerate() {
        if !token.contains('$') {
            continue;
        }
        let after_flag = at > 0 && argv[at - 1] == "--search" && *token == whole;
        if !(after_flag || *token == format!("--search={whole}")) {
            return None;
        }
        uses += 1;
    }
    Some(uses)
}

/// Whether `command` has balanced quotes and, outside them, no `<`, `>`,
/// `|`, `&`, paren or brace — so `;` and newlines are its only operators.
fn plain_separators_only(command: &str) -> bool {
    let (mut single, mut double, mut escaped) = (false, false, false);
    for c in command.chars() {
        if escaped {
            escaped = false;
        } else if single {
            single = c != '\'';
        } else if c == '\\' {
            escaped = true;
        } else if double {
            double = c != '"';
        } else if "<>|&(){}".contains(c) {
            return false;
        } else {
            single = c == '\'';
            double = c == '"';
        }
    }
    !(single || double || escaped)
}
