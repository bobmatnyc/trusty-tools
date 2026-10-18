//! How the `tm fleet relaunch` floor (#10033) reads one segment's argv.
//!
//! Why: #10033 critic r1 — bash strips redirections, a background `&` and
//! group closers before `tm` runs, expands braces and globs, and `cargo run`
//! hands its trailing words to the binary without `--`. Each of these let
//! `tm fleet relaunch` run while clap, reading the literal argv, refused it.
//! What: [`shell_words`] drops the shell syntax and marks brace and glob
//! words as expanded; [`read_tm_args`] judges a `tm` argv with the `tm` clap
//! definition; [`cargo_run_argvs`] lists the argvs a `cargo run` naming `tm`
//! may hand the binary.
//! Test: `fleet_relaunch_tests.rs` (`shell_syntax_around_a_relaunch_does_not_hide_it`,
//! `a_brace_expansion_or_glob_where_the_subcommand_goes_denies`,
//! `cargo_run_without_dashes_denies`).

use clap::error::ErrorKind;
use clap::{ArgMatches, CommandFactory};

use super::super::architect_pane_parse::Word;
use super::super::bash_tokens::{RedirectRole, redirect_role};
use super::super::shell_groups::is_brace_expansion;
use super::super::write_targets::input_redirect;
use crate::cli::Cli;

/// What one `tm` argv runs, as far as the guard can tell.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reading {
    /// `tm fleet relaunch`.
    Relaunch,
    /// Any other subcommand, help or version output.
    Other,
    /// A word the shell expands sits where the subcommand is chosen.
    Unresolved(String),
}

/// One segment's words as the program receives them, and whether a `<`
/// redirection feeds its stdin.
pub(super) struct ShellWords {
    /// The words left once shell syntax is dropped.
    pub(super) words: Vec<Word>,
    /// Whether an input redirection, here-string or here-document was dropped.
    pub(super) fed: bool,
}

/// `words` (from `with_dynamics` over `segment`) as bash hands them to the
/// program.
///
/// What: every redirection word is dropped — `2>&1`, `>log`, `&>f`, `<in`,
/// `<<EOF`, and `>`/`<<<` with the word after it — wherever it sits, as is a
/// bare `&`, `)` or `}` and a `)` or `&` glued to a word's end. A word with an
/// unquoted brace expansion or glob ([`pattern_words`]) is marked expanded.
/// Test: `shell_syntax_around_a_relaunch_does_not_hide_it`,
/// `a_brace_expansion_or_glob_where_the_subcommand_goes_denies`.
pub(super) fn shell_words(segment: &str, words: Vec<Word>) -> ShellWords {
    let patterns = pattern_words(segment.trim());
    let aligned = patterns.len() == words.len();
    let (mut out, mut fed, mut skip) = (Vec::with_capacity(words.len()), false, false);
    for (i, mut word) in words.into_iter().enumerate() {
        if std::mem::take(&mut skip) {
            continue;
        }
        // #10033 critic r1: bash removes a redirection before the program runs.
        match redirect_role(&word.text) {
            RedirectRole::TargetFollows => skip = true,
            RedirectRole::Target(_) | RedirectRole::FileDescriptor => {}
            RedirectRole::None => match input_redirect(&word.text) {
                Some(follows) => (skip, fed) = (follows, true),
                None => {
                    let kept = word.text.trim_end_matches([')', '&']).len();
                    if kept == 0 || word.text == "}" {
                        continue;
                    }
                    word.text.truncate(kept);
                    word.dynamic |= aligned && patterns[i];
                    out.push(word);
                }
            },
        }
    }
    ShellWords { words: out, fed }
}

/// For each unquoted-whitespace-separated word of `segment`, whether bash
/// expands it as a pattern: an unquoted `*` or `?`, a `[` closed later by
/// `]`, or a brace expansion ([`is_brace_expansion`]).
fn pattern_words(segment: &str) -> Vec<bool> {
    let (mut out, mut single, mut double, mut escaped) = (Vec::new(), false, false, false);
    // The word's unquoted characters, quoted ones replaced by `_`.
    let mut current: Option<String> = None;
    for c in segment.chars() {
        let quoted = single || double || escaped;
        if !quoted && c.is_whitespace() {
            out.extend(current.take().map(|w| is_pattern(&w)));
            continue;
        }
        let word = current.get_or_insert_with(String::new);
        if escaped {
            escaped = false;
            word.push('_');
            continue;
        }
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            _ if quoted => word.push('_'),
            _ => word.push(c),
        }
    }
    out.extend(current.map(|w| is_pattern(&w)));
    out
}

/// Whether the unquoted text of one word is a glob or brace expansion.
fn is_pattern(word: &str) -> bool {
    word.contains(['*', '?'])
        || word
            .find('[')
            .is_some_and(|at| word[at + 1..].contains(']'))
        || is_brace_expansion(word)
}

/// Read a `tm` argv (program word excluded) with the `tm` clap definition.
///
/// What: with no expanded word, clap parses the argv strictly. Help and
/// version output never run the verb ([`Reading::Other`]); any other refusal
/// is read again loosely, as the words before the first expanded one always
/// are: errors ignored, the subcommand path clap reached decides. A path
/// that already chose a leaf subcommand, or a group other than `fleet`,
/// cannot reach `fleet relaunch`; an expanded word at the top level or under
/// `fleet` is [`Reading::Unresolved`].
/// Test: `an_expanded_word_where_the_subcommand_goes_denies`,
/// `an_expanded_word_after_another_subcommand_passes`,
/// `shell_syntax_around_a_relaunch_does_not_hide_it`.
pub(super) fn read_tm_args(args: &[Word]) -> Reading {
    let first_dynamic = args.iter().position(|w| w.dynamic);
    let fixed = &args[..first_dynamic.unwrap_or(args.len())];
    let argv = || std::iter::once("tm").chain(fixed.iter().map(|w| w.text.as_str()));
    if first_dynamic.is_none() {
        match Cli::command().try_get_matches_from(argv()) {
            Ok(m) if subcommand_path(&m) == ["fleet", "relaunch"] => return Reading::Relaunch,
            Ok(_) => return Reading::Other,
            Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
                return Reading::Other;
            }
            // #10033 critic r1: a refusal from syntax the guard did not strip
            // must not pass; read the argv loosely instead.
            Err(_) => {}
        }
    }
    let path = Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(argv())
        .map(|m| subcommand_path(&m))
        .unwrap_or_default();
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    match (path.as_slice(), first_dynamic) {
        (["fleet", "relaunch"], _) => Reading::Relaunch,
        ([] | ["fleet"], Some(at)) => Reading::Unresolved(args[at].text.clone()),
        _ => Reading::Other,
    }
}

/// The chain of subcommand names `m` selected, outermost first.
fn subcommand_path(m: &ArgMatches) -> Vec<String> {
    let mut path = Vec::new();
    let mut at = m;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name.to_string());
        at = sub;
    }
    path
}

/// `cargo run` options whose value is the next word.
const CARGO_VALUED: &[&str] = &[
    "--bin",
    "--example",
    "-p",
    "--package",
    "-F",
    "--features",
    "--target",
    "--target-dir",
    "--manifest-path",
    "--profile",
    "-j",
    "--jobs",
    "--color",
    "--message-format",
    "--config",
    "-Z",
    "--lockfile-path",
];

/// Suffixes of a `cargo run` argv read as a `tm` argv.
const MAX_CARGO_SUFFIXES: usize = 16;

/// The argvs a `cargo run` at `pos` naming `tm` or `trusty-mpm` may hand the
/// binary; empty for any other cargo command.
///
/// What: cargo passes the words after `--` and every positional after its
/// options to the binary (`cargo run [OPTIONS] [ARGS]...`). The words after
/// the first `--` are one candidate. Rather than parse every cargo option,
/// each suffix before that `--` starting at a word that is neither an option
/// nor a [`CARGO_VALUED`] value is a candidate too, `--` dropped, so an
/// option the table does not know cannot hide the argv.
/// Test: `every_spelling_of_tm_fleet_relaunch_is_denied`,
/// `cargo_run_without_dashes_denies`.
pub(super) fn cargo_run_argvs(words: &[Word], pos: usize) -> Vec<Vec<Word>> {
    let rest = &words[pos + 1..];
    let Some(run) = rest.iter().position(|w| w.text == "run" || w.text == "r") else {
        return Vec::new();
    };
    let names_tm = rest.iter().any(|w| {
        let value = w.text.rsplit('=').next().unwrap_or(&w.text);
        value.eq_ignore_ascii_case("tm") || value.eq_ignore_ascii_case("trusty-mpm")
    });
    if !names_tm {
        return Vec::new();
    }
    let after: Vec<Word> = rest[run + 1..]
        .iter()
        .filter(|w| w.text != "--")
        .cloned()
        .collect();
    let dashes = rest[run + 1..].iter().position(|w| w.text == "--");
    (0..after.len())
        .filter(|&i| {
            let value = i > 0 && CARGO_VALUED.contains(&after[i - 1].text.as_str());
            let positional = !(after[i].text.starts_with('-') || value);
            dashes == Some(i) || (positional && dashes.is_none_or(|d| i < d))
        })
        .take(MAX_CARGO_SUFFIXES)
        .map(|i| after[i..].to_vec())
        .collect()
}
