//! Which words of a segment name the program it runs, for
//! `pm_guard_secret_script` (#8879), split out for the 500-SLOC cap (#10672).
//!
//! Why: the guard judges the program a command runs. #10672: an interpreter
//! ends its own option parsing at the script, so a word after the script is
//! data handed to it, and reading it as a script refused Word and PDF paths.
//! What: [`candidates`] reads one segment's argv into the files it may run.
//! Test: `tests/tm_hook_pm_guard_binary_operands_10672.rs`;
//! `pm_guard_secret_script_tests.rs`.

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::{RedirectRole, input_redirect_operand, redirect_role};
use crate::commands::pm_guard_secret_read::command_basename;

/// An interpreter family: which options run inline code, and which take an
/// attached value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Family {
    Shell,
    Python,
    Node,
    Ruby,
    Perl,
}

impl Family {
    /// Short option letters that make the interpreter run inline code or stdin.
    fn inline_letters(self) -> &'static [char] {
        match self {
            Family::Shell => &['c', 's'],
            Family::Python => &['c', 'm'],
            Family::Node => &['e', 'p'],
            Family::Ruby => &['e'],
            Family::Perl => &['e', 'E'],
        }
    }

    /// Short option letters whose value is the rest of the cluster.
    fn valued_letters(self) -> &'static [char] {
        match self {
            Family::Shell => &['o', 'O'],
            Family::Python => &['W', 'X'],
            Family::Node => &['r'],
            // #10672: `-W` takes an optional attached level (`-W:no-deprecated`).
            Family::Ruby => &['I', 'r', 'C', 'E', 'F', 'K', 'W', 'x', '0'],
            Family::Perl => &['I', 'M', 'm', 'i', 'l', '0', 'C', 'F', 'd', 'D', 'x'],
        }
    }

    /// #10672: short option letters that take the next word as their value when
    /// they end a cluster (`python3 -W ignore s.py`, `ruby -wr json s.rb`); a
    /// shell's take one each wherever they sit (`bash -oe pipefail s.sh`). The
    /// other valued letters only ever attach theirs (`perl -i.bak`).
    fn next_word_letters(self) -> &'static [char] {
        match self {
            Family::Shell => &['o', 'O'],
            Family::Python => &['W', 'X'],
            Family::Node => &['C'],
            Family::Ruby => &['I', 'r', 'C', 'E', 'X'],
            Family::Perl => &['I'],
        }
    }

    /// #10672: short option letters whose value is a file loaded as code.
    fn loaded_letters(self) -> &'static [char] {
        match self {
            Family::Ruby => &['r'],
            _ => &[],
        }
    }

    /// #10672: long options known to take the next word as their value.
    fn next_word_long_options(self) -> &'static [&'static str] {
        match self {
            Family::Shell => &["--emulate"],
            Family::Python => &["--check-hash-based-pycs"],
            Family::Node | Family::Ruby | Family::Perl => &[],
        }
    }

    /// #10672: whether this family has more long value options than the module
    /// lists, so the word after an unlisted one may be its value (residual 4).
    fn has_unlisted_long_values(self) -> bool {
        matches!(self, Family::Node | Family::Ruby)
    }

    /// Options whose separate value is a file run as code.
    fn code_loading_options(self) -> &'static [&'static str] {
        match self {
            Family::Shell => &["--rcfile", "--init-file"],
            Family::Node => &[
                "-r",
                "--require",
                "--import",
                "--loader",
                "--experimental-loader",
            ],
            // #10672: Ruby `-r` is a cluster letter, read by `cluster_value`.
            Family::Python | Family::Ruby | Family::Perl => &[],
        }
    }
}

/// The family a program basename belongs to, if it is a listed interpreter.
pub(super) fn family_of(program: &str) -> Option<Family> {
    let versioned_python = program
        .strip_prefix("python")
        .is_some_and(|v| v.is_empty() || v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    match program {
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "source" | "." => Some(Family::Shell),
        "node" | "nodejs" => Some(Family::Node),
        "ruby" => Some(Family::Ruby),
        "perl" => Some(Family::Perl),
        _ if versioned_python => Some(Family::Python),
        _ => None,
    }
}

/// How a candidate word reaches the interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// The script operand. #10672: the words after it are the script's own
    /// arguments, so there is one, unless an unlisted long option or a value
    /// letter read as taking no next word left that word in doubt; then the
    /// first that exists is the script.
    Positional,
    /// A file an option loads as code, beside the script.
    Loaded,
    /// The interpreter's stdin: the script when the command names no other.
    Stdin,
}

/// One file a segment may run: the word as written, how it is reached, and
/// the interpreter (`None` for a path run directly, decided by its shebang).
#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) word: String,
    pub(super) role: Role,
    pub(super) interpreter: Option<String>,
}

/// The candidate scripts of one segment's argv, and whether the program word
/// was resolved exactly rather than found by a scan for an interpreter name.
pub(super) fn candidates(argv: &[String]) -> (Vec<Candidate>, bool) {
    // #9037: a case arm's pattern (`pat)`) is not a command; the words after it are.
    if let Some(first) = argv.first()
        && first.ends_with(')')
        && !first.contains('(')
    {
        return candidates(&argv[1..]);
    }
    let exact = strip_wrapper_prefix(argv);
    let start = exact
        .or_else(|| {
            argv.iter()
                .position(|t| family_of(&command_basename(t)).is_some())
        })
        .unwrap_or(0);
    let Some(program) = argv.get(start) else {
        return (Vec::new(), false);
    };
    let basename = command_basename(program);
    let found = match family_of(&basename) {
        Some(family) => {
            let interpreter = if matches!(basename.as_str(), "source" | ".") {
                "sh".to_string()
            } else {
                basename
            };
            interpreter_candidates(family, &interpreter, &argv[start + 1..])
        }
        None if program.contains('/') => vec![Candidate {
            word: program.clone(),
            role: Role::Positional,
            interpreter: None,
        }],
        None => Vec::new(),
    };
    (found, exact.is_some())
}

/// The candidates an interpreter's arguments name; empty after an option that
/// runs inline code, because the inline rules already read that code.
///
/// Why: #10672 — every listed interpreter ends its own option parsing at the
/// script operand. Reading on took a data argument for the script when the
/// script word did not resolve, and read `-c` after a script as inline code.
/// What: stops at the script operand, skipping the value word of an option
/// that takes one. A redirected stdin counts only when no script is named.
/// When a word is in doubt, the next operand is a candidate too and a later
/// inline option no longer drops them, so a misread value falls back to the
/// first-that-exists reading.
/// Test: `plain_script_with_binary_args_allows_10672`,
/// `an_unresolved_script_does_not_fall_through_to_data_10672`,
/// `a_credential_script_is_denied_whatever_its_arguments_10672`.
fn interpreter_candidates(family: Family, interpreter: &str, args: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<Candidate>, word: &str, role| {
        out.push(Candidate {
            word: word.to_string(),
            role,
            interpreter: Some(interpreter.to_string()),
        });
    };
    let mut options_done = false;
    let mut maybe_a_value = false;
    let mut in_doubt = false;
    let mut i = 0;
    while let Some(token) = args.get(i) {
        i += 1;
        // A here-document or here-string is inline text the inline rules read.
        if token.starts_with("<<") {
            i += usize::from(matches!(token.as_str(), "<<" | "<<-" | "<<<"));
            continue;
        }
        if let Some((0, attached)) = input_redirect_operand(token) {
            let word = if attached.is_empty() {
                i += 1;
                args.get(i - 1).map_or("", String::as_str)
            } else {
                attached
            };
            push(&mut out, word, Role::Stdin);
            continue;
        }
        match redirect_role(token) {
            RedirectRole::None => {}
            RedirectRole::TargetFollows => {
                i += 1;
                continue;
            }
            _ => continue,
        }
        let after_unlisted = std::mem::take(&mut maybe_a_value);
        if options_done || !is_option(family, token) {
            push(&mut out, token, Role::Positional);
            // #10672: what follows the script is its arguments, not more scripts.
            if after_unlisted {
                in_doubt = true;
                continue;
            }
            break;
        }
        if token == "--" {
            options_done = true;
        } else if token == "-" || runs_inline(family, token) {
            // #10672: after a word in doubt, this option may be the script's
            // own argument, so the words before it stay judged.
            if !in_doubt {
                out.retain(|c| c.role != Role::Positional);
            }
            return out;
        } else if family.code_loading_options().contains(&token.as_str()) {
            if let Some(value) = args.get(i) {
                push(&mut out, value, Role::Loaded);
            }
            i += 1;
        } else if let Some((flag, value)) = token.split_once('=')
            && family.code_loading_options().contains(&flag)
        {
            push(&mut out, value, Role::Loaded);
        } else if family.next_word_long_options().contains(&token.as_str()) {
            i += 1;
        } else if let Some((letter, rest, words)) = cluster_value(family, token) {
            // #10672: Ruby `-r` loads its value as code, attached or the next word.
            if family.loaded_letters().contains(&letter) {
                let value = if rest.is_empty() {
                    args.get(i).map(String::as_str)
                } else {
                    Some(rest)
                };
                if let Some(value) = value {
                    push(&mut out, value, Role::Loaded);
                }
            }
            i += words;
            // #10672: a value letter read as taking no next word may still take
            // it, so the next word gets the first-that-exists reading.
            maybe_a_value = words == 0;
        } else {
            maybe_a_value = family.has_unlisted_long_values()
                && token.starts_with("--")
                && !token.contains('=');
        }
    }
    // #10672: a script named on the command is the program; stdin is its data.
    if out.iter().any(|c| c.role == Role::Positional) {
        out.retain(|c| c.role != Role::Stdin);
    }
    out
}

/// Whether `token` is an interpreter option; a shell also takes `+x` and `+o`.
fn is_option(family: Family, token: &str) -> bool {
    token.starts_with('-') || (family == Family::Shell && token.len() > 1 && token.starts_with('+'))
}

/// #10672: the value a short option cluster takes: its first value-taking
/// letter, the value attached after it, and how many next words it takes.
/// `None` for a cluster with no value-taking letter. Elsewhere the first such
/// letter takes the rest of the cluster, or the next word when it ends it; a
/// shell's `o`/`O` each take one next word (`bash -oe pipefail s.sh`).
fn cluster_value(family: Family, token: &str) -> Option<(char, &str, usize)> {
    let cluster = token
        .strip_prefix(['-', '+'])
        .filter(|c| !c.is_empty() && !c.starts_with('-'))?;
    let next_word = family.next_word_letters();
    if family == Family::Shell {
        let letter = cluster.chars().find(|c| next_word.contains(c))?;
        let words = cluster.chars().filter(|c| next_word.contains(c)).count();
        return Some((letter, "", words));
    }
    let valued = |c: char| next_word.contains(&c) || family.valued_letters().contains(&c);
    let (at, letter) = cluster.char_indices().find(|&(_, c)| valued(c))?;
    let rest = &cluster[at + letter.len_utf8()..];
    let words = usize::from(rest.is_empty() && next_word.contains(&letter));
    Some((letter, rest, words))
}

/// Whether option `token` makes `family` run inline code or read stdin.
fn runs_inline(family: Family, token: &str) -> bool {
    if family == Family::Node {
        return matches!(token, "-e" | "-p" | "--eval" | "--print")
            || token.starts_with("--eval=")
            || token.starts_with("--print=");
    }
    let Some(cluster) = token.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
        return false;
    };
    for c in cluster.chars() {
        if family.valued_letters().contains(&c) {
            return false;
        }
        if family.inline_letters().contains(&c) {
            return true;
        }
    }
    false
}
