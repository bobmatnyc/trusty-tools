//! Where a `Bash` segment's program sits, and the argv the shell hands it,
//! as the trust-anchor rule reads them (#8878 round 2).
//!
//! Why: split out of `anchor_verbs.rs` (line cap) when round 2 made the
//! program lookup measure wrapper options (H1), skip reserved words (H4),
//! expand brace groups (H3) and keep `find`'s `\;` in one segment.
//! What: [`quote_escaped_semicolons`] respells `\;` as `';'`;
//! [`program_argv`] tokenizes a segment and places its program;
//! [`expand_braces`] expands an argv's brace groups.
//! Test: `pm_guard_trust_anchor_split_tests.rs`.

use std::borrow::Cow;

use super::anchor_verbs::names_a_verb;
use super::bash_tokens::tokenize;
use super::build_lease_program::LEADING_KEYWORDS;
use super::heredoc::split_heredoc_bodies;
use super::secret_file_copy::expand_brace_alternatives;
use super::write_targets::UnplaceableWrite;
use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard_trust_anchor_paths::has_brace_group;
use crate::commands::program_word::resolve_program_word;

/// A verb segment the guard could not lex (#8878).
const UNLEXABLE_VERB: UnplaceableWrite =
    "a `cp`/`mv`/`ln`/`install`/`sed`/`cd` whose arguments do not lex";

/// `command` with each unquoted `\;` spelled `';'` (#8878 round 2).
///
/// Why: the shared segment splitter cuts at the `;` of `find … -exec rm {} \;`,
/// leaving a find segment that does not lex; a quoted `;` is the same argv
/// word and is not cut.
/// Test: `a_find_exec_action_is_read_as_a_command`.
pub(super) fn quote_escaped_semicolons(command: &str) -> Cow<'_, str> {
    if !command.contains("\\;") {
        return Cow::Borrowed(command);
    }
    let mut out = String::with_capacity(command.len() + 8);
    let (mut single, mut double) = (false, false);
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '\\' if !single => {
                match chars.next() {
                    Some(';') if !double => out.push_str("';'"),
                    Some(next) => {
                        out.push(c);
                        out.push(next);
                    }
                    None => out.push(c),
                }
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    Cow::Owned(out)
}

/// `words` with each brace group expanded as the shell would (#8878 H3). A
/// word the expander cannot read stays whole; the judge denies it.
pub(super) fn expand_braces(words: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    for word in words {
        match has_brace_group(word)
            .then(|| expand_brace_alternatives(word))
            .flatten()
        {
            Some(readings) => out.extend(readings),
            None => out.push(word.clone()),
        }
    }
    out
}

/// A segment's words and where the programs this module reads sit.
pub(super) struct Program {
    /// The segment's words.
    pub(super) argv: Vec<String>,
    /// Each index that may be the program: one when the resolver places it,
    /// every verb word when it cannot (fail closed).
    pub(super) candidates: Vec<usize>,
    /// A wrapper option before the program changes the directory.
    pub(super) chdir: bool,
}

/// The segment's words and the program they run.
///
/// What: tokenizes with here-document bodies blanked, skips leading reserved
/// words (#8878 H4), and places the program past env assignments and
/// wrappers with the #8735 resolver, which measures valued wrapper options
/// (#8878 H1: `env -u find cp` runs `cp`). When the resolver cannot measure
/// an option, every verb or `xargs` word is a candidate. A segment that does
/// not tokenize is `Err` when its first word past reserved words is a verb or
/// `xargs`, and `None` otherwise; so is a segment whose program is none.
/// Test: `a_wrapper_option_value_is_not_the_program`,
/// `a_reserved_word_does_not_hide_the_command`.
pub(super) fn program_argv(segment: &str) -> Result<Option<Program>, UnplaceableWrite> {
    let argv = match tokenize(&split_heredoc_bodies(segment).0) {
        Ok(argv) => argv,
        Err(_)
            if first_command_token(skip_keywords(segment))
                .as_deref()
                .is_some_and(names_a_verb) =>
        {
            return Err(UNLEXABLE_VERB);
        }
        Err(_) => return Ok(None),
    };
    // #8878 H4: `if`, `then`, `do`, `{`, `!` … are not the program.
    let skip = argv
        .iter()
        .take_while(|w| LEADING_KEYWORDS.contains(&w.as_str()))
        .count();
    let words = &argv[skip..];
    let candidates: Vec<usize> = match resolve_program_word(words) {
        Ok(word) => vec![word.xargs_at.unwrap_or(word.index)],
        // #8878 H1: an unmeasured option: every verb word, unioned.
        Err(_) => (0..words.len())
            .filter(|i| names_a_verb(&words[*i]))
            .collect(),
    };
    let candidates: Vec<usize> = candidates
        .into_iter()
        .filter(|at| words.get(*at).is_some_and(|w| names_a_verb(w)))
        .collect();
    let Some(first) = candidates.first() else {
        return Ok(None);
    };
    let chdir = words[..*first].iter().any(|w| changes_dir(w));
    let candidates = candidates.iter().map(|at| at + skip).collect();
    Ok(Some(Program {
        argv,
        candidates,
        chdir,
    }))
}

/// `segment` past its leading reserved words, for the unlexable fallback.
fn skip_keywords(mut segment: &str) -> &str {
    loop {
        let trimmed = segment.trim_start();
        match trimmed.split_once(char::is_whitespace) {
            Some((first, rest)) if LEADING_KEYWORDS.contains(&first) => segment = rest,
            _ => return trimmed,
        }
    }
}

/// Whether a wrapper-prefix word changes the directory: `--chdir`, or a short
/// cluster holding `C` (`env -C`) or `D` (`sudo -D`). A misread only makes
/// relative paths unplaceable.
fn changes_dir(word: &str) -> bool {
    word == "--chdir"
        || word.starts_with("--chdir=")
        || (word.len() > 1
            && word.starts_with('-')
            && !word.starts_with("--")
            && word[1..].contains(['C', 'D']))
}
