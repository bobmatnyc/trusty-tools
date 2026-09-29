//! The one program-word resolver behind the pm-guard rules (#8735).
//!
//! Why: the credential rules resolved a stage's program with
//! [`super::hook_rewrite::strip_wrapper_prefix`], which advanced past a wrapper
//! one token at a time and gave up at the first flag. `timeout 5 echo "$T"`
//! resolved to `5`, `nice -n 5 echo "$T"` to `nice`, and `noglob echo "$T"` to
//! `noglob`, so a credential printer hid behind every one of them. Each rule
//! that grew its own skip loop missed a different wrapper, so the wrapper names
//! and their option grammars live here, once.
//! What: [`COMMAND_WRAPPERS`] is the one list of wrapper and precommand words,
//! generated with each word's option grammar. [`resolve_program_word`] skips
//! leading `KEY=value` words and every wrapper with its options, option values,
//! assignments (`env`, `sudo`) and operands (`timeout`'s duration), plus
//! `xargs` and its options. An option the grammar does not know, a value-taking
//! option with no value, or a `timeout` duration that is not a duration FAILS
//! the resolution rather than guessing where the program starts.
//! Test: `resolves_past_each_wrapper_and_its_options`,
//! `an_unknown_or_unmeasurable_option_fails`, `every_wrapper_has_a_grammar`.

use super::hook_rewrite::is_env_assignment;

/// The options one wrapper accepts before its program word.
struct Grammar {
    /// Options that take no value, short (`-p`) and long (`--verbose`).
    flags: &'static [&'static str],
    /// Options that take the next token, or an attached (`-n5`, `--signal=9`) value.
    valued: &'static [&'static str],
    /// `KEY=value` words may follow the options (`env`, `sudo`).
    assignments: bool,
    /// A bare numeric option is valid (`nice -5`).
    numeric: bool,
    /// One duration operand precedes the program (`timeout 5`).
    duration: bool,
}

/// A wrapper with no options (`nohup`, `noglob`).
const BARE: Grammar = Grammar {
    flags: &[],
    valued: &[],
    assignments: false,
    numeric: false,
    duration: false,
};

/// Declare [`COMMAND_WRAPPERS`] and its grammars from one table, so a wrapper
/// cannot gain a name without a grammar or the reverse.
macro_rules! wrappers {
    ($($name:literal => $grammar:expr,)*) => {
        /// Wrapper and precommand words that run the command after them.
        ///
        /// Why: #4031 review pass 2 — each wrapper enumerated alone (`sudo`, then
        /// `env`, then `command`) left the next one (`nice rm -rf /`) through,
        /// so every caller shares this one list. #8735 adds the zsh
        /// precommand modifiers `noglob` and `nocorrect`.
        /// What: matched exactly, after a leading `\` and any path are stripped.
        pub(crate) const COMMAND_WRAPPERS: &[&str] = &[$($name,)*];

        /// The grammar of each [`COMMAND_WRAPPERS`] entry, index for index.
        const GRAMMARS: &[Grammar] = &[$($grammar,)*];
    };
}

wrappers! {
    "sudo" => Grammar {
        flags: &[
            "-A", "-b", "-B", "-E", "-H", "-i", "-n", "-N", "-P", "-S", "-s", "--askpass",
            "--background", "--bell", "--login", "--non-interactive", "--preserve-env",
            "--preserve-groups", "--set-home", "--shell", "--stdin",
        ],
        valued: &[
            "-C", "-D", "-g", "-h", "-p", "-R", "-r", "-T", "-t", "-U", "-u", "--chdir",
            "--chroot", "--close-from", "--command-timeout", "--group", "--host",
            "--other-user", "--prompt", "--role", "--type", "--user",
        ],
        assignments: true,
        ..BARE
    },
    "env" => Grammar {
        // `-S` is absent on purpose: its value is a command string, which
        // `shell_lex::wrapped_command` reads; here it fails the resolution.
        flags: &["-", "-0", "-i", "-v", "--debug", "--ignore-environment", "--null"],
        valued: &["-C", "-P", "-u", "--chdir", "--unset"],
        assignments: true,
        ..BARE
    },
    "command" => Grammar { flags: &["-p"], ..BARE },
    "builtin" => BARE,
    "doas" => Grammar { flags: &["-n", "-s"], valued: &["-u"], ..BARE },
    "nice" => Grammar { valued: &["-n", "--adjustment"], numeric: true, ..BARE },
    "time" => Grammar {
        flags: &["-a", "-l", "-p", "-q", "-v", "--append", "--portability", "--quiet", "--verbose"],
        valued: &["-f", "-o", "--format", "--output"],
        ..BARE
    },
    "nohup" => BARE,
    "exec" => Grammar { flags: &["-c", "-l"], valued: &["-a"], ..BARE },
    "ionice" => Grammar {
        flags: &["-t", "--ignore"],
        valued: &["-c", "-n", "--class", "--classdata"],
        ..BARE
    },
    "timeout" => Grammar {
        flags: &["-v", "--foreground", "--preserve-status", "--verbose"],
        valued: &["-k", "-s", "--kill-after", "--signal"],
        duration: true,
        ..BARE
    },
    "stdbuf" => Grammar {
        valued: &["-e", "-i", "-o", "--error", "--input", "--output"],
        ..BARE
    },
    "caffeinate" => Grammar { flags: &["-d", "-i", "-m", "-s", "-u"], valued: &["-t", "-w"], ..BARE },
    "noglob" => BARE,
    "nocorrect" => BARE,
}

/// `xargs` options that consume the FOLLOWING token as their value.
///
/// Why: skipping only the flag would leave its value (`4` in `xargs -n 4 git …`)
/// read as the program. `shell_lex::wrapped_command` reads the same table, so
/// the two cannot disagree on where `xargs`' program starts.
/// What: the separated-value option spellings, short and long.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
pub(crate) const XARGS_OPTS_WITH_ARG: &[&str] = &[
    "-a",
    "-d",
    "-E",
    "-e",
    "-I",
    "-i",
    "-L",
    "-l",
    "-n",
    "-P",
    "-s",
    "--arg-file",
    "--delimiter",
    "--eof",
    "--replace",
    "--max-lines",
    "--max-args",
    "--max-procs",
    "--max-chars",
    "--process-slot-var",
];

/// `xargs`: not a [`COMMAND_WRAPPERS`] entry, because its argv also becomes a
/// wrapped command (`shell_lex::wrapped_command`) that callers read on its own.
const XARGS: Grammar = Grammar {
    flags: &[
        "-0",
        "-o",
        "-p",
        "-r",
        "-t",
        "-x",
        "--exit",
        "--interactive",
        "--no-run-if-empty",
        "--null",
        "--open-tty",
        "--verbose",
    ],
    valued: XARGS_OPTS_WITH_ARG,
    ..BARE
};

/// Where a command's program word sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgramWord {
    /// Index of the program word. `tokens.len()` for assignments alone; a
    /// wrapper's own index when nothing follows it (`env` prints the
    /// environment).
    pub(crate) index: usize,
    /// A wrapper option (a `-` word) was skipped on the way.
    pub(crate) took_options: bool,
    /// Index of the `xargs` word skipped on the way, if any.
    pub(crate) xargs_at: Option<usize>,
}

/// The program word could not be located: a wrapper carries an option whose
/// value count is unknown. A rule that reaches this denies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unresolved;

/// Locate the program word of `tokens`, past assignments, wrappers and `xargs`.
///
/// Why: see the module doc — one resolver for the credential rules, the
/// universal delete floor, and through [`super::hook_rewrite::strip_wrapper_prefix`]
/// every older caller.
/// What: a leading `\` and a path are stripped from each candidate word. Each
/// wrapper consumes its options (clustered short flags included, with an
/// attached or separate value), then its assignments or duration operand; `--`
/// ends its options. See [`ProgramWord`] for the index returned.
/// Test: `resolves_past_each_wrapper_and_its_options`,
/// `an_unknown_or_unmeasurable_option_fails`.
pub(crate) fn resolve_program_word<T: AsRef<str>>(tokens: &[T]) -> Result<ProgramWord, Unresolved> {
    resolve(tokens, true)
}

/// The `strip_wrapper_prefix` contract over [`resolve`]: `xargs` is a program,
/// and a wrapper option or a wrapper with nothing after it yields `None`.
///
/// Why: its callers (the Bash rewrite, `first_command_token`, the git-verb
/// rules) read `None` as "cannot tell" and were built on that answer; #8735
/// changes which program they see, not when they are told to be careful.
pub(crate) fn precommand_index<T: AsRef<str>>(tokens: &[T]) -> Option<usize> {
    let word = resolve(tokens, false).ok()?;
    let bare_wrapper = tokens.get(word.index).is_some_and(|t| {
        grammar_of(t.as_ref().strip_prefix('\\').unwrap_or(t.as_ref()), false).is_some()
    });
    (!word.took_options && !bare_wrapper).then_some(word.index)
}

/// [`resolve_program_word`], with `xargs` read as a program when
/// `through_xargs` is false (the [`precommand_index`] contract).
fn resolve<T: AsRef<str>>(tokens: &[T], through_xargs: bool) -> Result<ProgramWord, Unresolved> {
    let mut word = ProgramWord {
        index: 0,
        took_options: false,
        xargs_at: None,
    };
    while let Some(raw) = tokens.get(word.index) {
        let tok = raw.as_ref().strip_prefix('\\').unwrap_or(raw.as_ref());
        if is_env_assignment(tok) {
            word.index += 1;
            continue;
        }
        let Some(grammar) = grammar_of(tok, through_xargs) else {
            break;
        };
        if tok.rsplit('/').next() == Some("xargs") {
            word.xargs_at = Some(word.index);
        }
        let (after, took) = skip_wrapper_args(grammar, tokens, word.index + 1)?;
        word.took_options |= took;
        if after >= tokens.len() {
            // Nothing follows: the wrapper runs as itself.
            return Ok(word);
        }
        word.index = after;
    }
    Ok(word)
}

/// The grammar of the wrapper `word` names, if it names one.
fn grammar_of(word: &str, through_xargs: bool) -> Option<&'static Grammar> {
    let base = word.rsplit('/').next().unwrap_or(word);
    if base == "xargs" {
        return through_xargs.then_some(&XARGS);
    }
    COMMAND_WRAPPERS
        .iter()
        .position(|w| *w == base)
        .and_then(|at| GRAMMARS.get(at))
}

/// The index past one wrapper's options, assignments and operand, starting at
/// `from`, and whether an option was consumed.
fn skip_wrapper_args<T: AsRef<str>>(
    grammar: &Grammar,
    tokens: &[T],
    from: usize,
) -> Result<(usize, bool), Unresolved> {
    let mut at = from;
    let mut took = false;
    while let Some(tok) = tokens.get(at).map(AsRef::as_ref) {
        if tok == "--" {
            at += 1;
            took = true;
            break;
        }
        let is_option = tok.starts_with('-') && (tok.len() > 1 || grammar.flags.contains(&"-"));
        if !is_option {
            break;
        }
        at += option_width(grammar, tok, tokens.get(at + 1).is_some())?;
        took = true;
    }
    if grammar.assignments {
        while tokens
            .get(at)
            .is_some_and(|t| is_env_assignment(t.as_ref()))
        {
            at += 1;
        }
    }
    if grammar.duration {
        let operand = tokens.get(at).ok_or(Unresolved)?;
        if !is_duration(operand.as_ref()) {
            return Err(Unresolved);
        }
        at += 1;
    }
    Ok((at, took))
}

/// How many tokens option `tok` spans (1 or 2), or [`Unresolved`] when the
/// grammar does not know it or its value is missing.
fn option_width(grammar: &Grammar, tok: &str, has_next: bool) -> Result<usize, Unresolved> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if grammar.flags.contains(&tok) {
        return Ok(1);
    }
    if grammar.numeric && digits(tok.trim_start_matches('-')) {
        return Ok(1);
    }
    if tok.starts_with("--") {
        let (name, attached) = tok.split_once('=').map_or((tok, false), |(n, _)| (n, true));
        return match (grammar.valued.contains(&name), attached) {
            (true, true) => Ok(1),
            (true, false) if has_next => Ok(2),
            // `--preserve-env=LIST`: a flag with an optional attached value.
            (false, true) if grammar.flags.contains(&name) => Ok(1),
            _ => Err(Unresolved),
        };
    }
    // A short cluster (`-vs9`): flags, then at most one value-taking option,
    // whose value is the rest of the cluster or the next token.
    let cluster = &tok[1..];
    for (at, c) in cluster.char_indices() {
        let short = |list: &[&str]| {
            list.iter()
                .any(|o| o.len() == 2 && o.ends_with(c) && !o.starts_with("--"))
        };
        if short(grammar.flags) {
            continue;
        }
        if short(grammar.valued) {
            let rest = &cluster[at + c.len_utf8()..];
            return match (rest.is_empty(), has_next) {
                (false, _) => Ok(1),
                (true, true) => Ok(2),
                (true, false) => Err(Unresolved),
            };
        }
        return Err(Unresolved);
    }
    Ok(1)
}

/// Whether `word` is a `timeout` duration: a decimal number with an optional
/// `s`/`m`/`h`/`d` suffix.
fn is_duration(word: &str) -> bool {
    let number = word.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(word);
    !number.is_empty()
        && number.bytes().any(|b| b.is_ascii_digit())
        && number.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && number.bytes().filter(|b| *b == b'.').count() <= 1
}

#[cfg(test)]
#[path = "program_word_tests.rs"]
mod tests;
