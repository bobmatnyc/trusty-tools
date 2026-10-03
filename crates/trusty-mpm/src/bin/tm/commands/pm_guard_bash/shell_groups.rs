//! Shell grouping syntax peeled off each segment before the git-verb rules
//! read it (#9127).
//!
//! Why: the main-checkout walker classified each composition segment on its
//! first word, so `(git -C <main> commit -a)` lexed as the program `(git` and
//! `{ git -C <main> commit -a; }` as the program `{`, and the commit rule
//! (ADR-0061: no commit lands on a local main checkout) never saw the commit.
//! A reserved word in command position (`then git commit`, `! git commit`,
//! `coproc git commit`) hid it the same way. The walker also has to know where
//! a child shell ends: a `cd` inside `( … )`, a coproc or a `sh -c` string does
//! not move the caller, and reading it as if it did cleared
//! `sh -c 'cd <wt>' && git commit -a` against the worktree while the commit ran
//! in the main checkout. `eval` is the opposite case: it runs in the current
//! shell, so its `cd` does persist.
//!
//! What: [`grouped_steps`] turns a command into [`Step`]s — each segment's
//! command text with its leading `(`, `{`, `}`, `)` and reserved words
//! ([`KEYWORDS`], shared with the credential rules) peeled off and its trailing
//! group closers cut, bracketed by [`Step::Enter`] and [`Step::Leave`] wherever
//! a subshell, a coproc or a child-process wrapper's string begins and ends. A
//! brace group and an `eval` string run in the current shell, so they bracket
//! nothing. A command whose grouping does not balance comes back flat and
//! marked unparsed. A balanced command holding a shape the walker cannot place
//! — a `case`, a function definition, a coproc, an `eval` behind `command` or
//! `noglob` ([`Scope::Unplaceable`]) — comes back peeled but also marked
//! unparsed. The commit rule refuses either when it carries a
//! `git commit` ([`mentions_git_commit`]), and the destructive rule when it
//! carries a destructive git verb ([`mentions_git_verb`]).
//!
//! Residuals: a function body runs where the function is CALLED, which this
//! does not follow, so a definition is unparsed rather than placed; `$(…)` and
//! backticks are not descended, as the module doc of `main_checkout` already
//! states.
//!
//! Test: `shell_groups::tests`, and the `#9127` rows in `main_checkout`'s
//! suite.

use super::credential_print::{COMPOUND_OPENERS, KEYWORDS, is_identifier};
use super::heredoc::HeredocBodies;
use super::shell_lex::{self, QuoteScan, Scope};
use super::{MAX_WRAPPER_DEPTH, split_shell_segments, split_shell_segments_raw};

/// Bytes that end a shell word as well as whitespace does.
const WORD_BREAKS: &[u8] = b"();&|<>";

/// One unit of the walk [`grouped_steps`] hands the git-verb rules.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// A simple command's text, grouping syntax removed.
    Command(String),
    /// A child shell starts: a `(` subshell, a coproc, or a child-process
    /// wrapper's command string.
    Enter,
    /// The child shell [`Step::Enter`] opened ends; its `cd`s end with it.
    Leave,
}

/// The steps of a command, and whether the walker can place every command.
pub(super) struct Groups {
    /// In command order. Flat segments, no `Enter`/`Leave`, when the grouping
    /// does not balance.
    pub(super) steps: Vec<Step>,
    /// `false` when a group opener or closer did not balance, or when a
    /// segment is a `case`, a function definition, a coproc or an
    /// unplaceable `eval`.
    pub(super) parsed: bool,
}

/// Which kind of group an opener started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Subshell,
    /// `scoped` when a coproc runs it, which makes it a child shell.
    Brace {
        scoped: bool,
    },
}

/// The grouping did not balance.
struct Unbalanced;

/// One peeled segment: its command, if any, how many subshells close after
/// it, and whether a coproc runs it as a child shell.
struct Peeled {
    command: Option<String>,
    leaves: usize,
    coproc: bool,
}

/// Walk `command` into [`Step`]s (#9127).
///
/// Why: see the module doc.
/// What: every segment [`split_shell_segments_raw`] cuts, peeled by
/// [`peel_segment`], followed — as `expand_shell_segments` does — by the
/// segments of a leading `sh -c`/`bash -c`/`env -S`/`flock -c`/`xargs`/`eval`
/// wrapper's string, up to [`MAX_WRAPPER_DEPTH`] layers. The string is
/// bracketed by `Enter`/`Leave` unless its carrier is `eval`
/// ([`shell_lex::wrapped_command_scoped`]); a [`Scope::Unplaceable`] `eval`
/// marks the command unparsed. When any level's openers and
/// closers do not pair up, the answer is the flat [`split_shell_segments`]
/// list with `parsed: false`, so a caller that ignores the flag reads exactly
/// what it read before #9127.
/// Test: `grouped_steps_peels_subshells_and_brace_groups`,
/// `grouped_steps_brackets_a_wrapper_string`,
/// `grouped_steps_scopes_a_child_shell_but_not_eval`,
/// `grouped_steps_peels_and_scopes_a_coproc`,
/// `grouped_steps_marks_shapes_it_cannot_place`,
/// `grouped_steps_reports_an_unbalanced_group`,
/// `grouped_steps_ignores_quoted_and_heredoc_parens`.
pub(super) fn grouped_steps(command: &str) -> Groups {
    let mut steps = Vec::new();
    let mut opaque = false;
    match steps_into(command, 0, &mut steps, &mut opaque) {
        Ok(()) => Groups {
            steps,
            parsed: !opaque,
        },
        Err(Unbalanced) => Groups {
            steps: split_shell_segments(command)
                .into_iter()
                .map(Step::Command)
                .collect(),
            parsed: false,
        },
    }
}

/// [`grouped_steps`] for one shell program: `command` at wrapper `depth`.
fn steps_into(
    command: &str,
    depth: usize,
    out: &mut Vec<Step>,
    opaque: &mut bool,
) -> Result<(), Unbalanced> {
    let mut open = Vec::new();
    for raw in split_shell_segments_raw(command) {
        let peeled = peel_segment(raw, &mut open, out, opaque)?;
        if let Some(text) = peeled.command {
            let (wrapped, scope) = shell_lex::wrapped_command_scoped(&text);
            // #9127 critic r3: bash and zsh disagree on where `command eval`
            // or `noglob eval` runs, so its `cd` cannot be placed.
            *opaque |= scope == Scope::Unplaceable;
            let inner = match wrapped {
                shell_lex::WrappedCommand::Inner(inner) if depth < MAX_WRAPPER_DEPTH => Some(inner),
                _ => None,
            };
            if peeled.coproc {
                out.push(Step::Enter);
            }
            out.push(Step::Command(text));
            if let Some(inner) = inner {
                // #9127: `eval` runs in this shell, so its `cd` persists.
                let child = scope == Scope::Child;
                if child {
                    out.push(Step::Enter);
                }
                steps_into(&inner, depth + 1, out, opaque)?;
                if child {
                    out.push(Step::Leave);
                }
            }
            if peeled.coproc {
                out.push(Step::Leave);
            }
        }
        out.extend(std::iter::repeat_with(|| Step::Leave).take(peeled.leaves));
    }
    if open.is_empty() {
        Ok(())
    } else {
        Err(Unbalanced)
    }
}

/// Peel one segment: its leading grouping words, then the group closers that
/// end its command.
///
/// What: strips, in any order, a `(` (pushes a subshell, emits `Enter`), a
/// `{` word (pushes a brace group, a child shell after `coproc`), a `}` word
/// or `)` (pops its own kind, `Leave` for a child shell) and a [`KEYWORDS`]
/// word (`time` with its `-p`; `coproc` with its NAME). A coproc of a
/// compound command other than `( … )` or `{ …; }` is [`Unbalanced`], since
/// its end is not this segment's. After a leading closer the rest is the
/// group's redirections, not a command. Otherwise the rest is cut at its first
/// live `)` that closes a paren this segment did not open
/// ([`trailing_closers`]), and a command [`cannot_place`] sets `opaque`.
fn peel_segment(
    raw: &str,
    open: &mut Vec<Group>,
    out: &mut Vec<Step>,
    opaque: &mut bool,
) -> Result<Peeled, Unbalanced> {
    let mut rest = raw;
    let mut closed = false;
    let mut coproc = false;
    loop {
        let t = rest.trim_start();
        let word = first_word(t);
        rest = if let Some(after) = t.strip_prefix('(') {
            open.push(Group::Subshell);
            out.push(Step::Enter);
            coproc = false;
            after
        } else if let Some(after) = t.strip_prefix(')') {
            pop(open, Group::Subshell)?;
            out.push(Step::Leave);
            closed = true;
            after
        } else if word == "{" {
            open.push(Group::Brace { scoped: coproc });
            if coproc {
                out.push(Step::Enter);
            }
            coproc = false;
            &t[1..]
        } else if word == "}" {
            match open.pop() {
                Some(Group::Brace { scoped }) => {
                    if scoped {
                        out.push(Step::Leave);
                    }
                }
                _ => return Err(Unbalanced),
            }
            closed = true;
            &t[1..]
        } else if coproc && COMPOUND_OPENERS.contains(&word) {
            return Err(Unbalanced);
        } else if KEYWORDS.contains(&word) {
            let after = t[word.len()..].trim_start();
            if word == "coproc" {
                coproc = true;
                *opaque = true;
                coproc_body(after)
            } else {
                match after.strip_prefix("-p") {
                    Some(p) if word == "time" && p.starts_with(char::is_whitespace) => p,
                    _ => after,
                }
            }
        } else {
            break;
        };
    }
    if closed {
        return Ok(Peeled {
            command: None,
            leaves: 0,
            coproc: false,
        });
    }
    let body = rest.trim();
    let (end, leaves) = trailing_closers(body, open)?;
    let command = body[..end].trim();
    *opaque |= cannot_place(command);
    Ok(Peeled {
        command: (!command.is_empty()).then(|| command.to_string()),
        leaves,
        coproc,
    })
}

/// The first shell word of `t`: up to whitespace or a [`WORD_BREAKS`] byte.
fn first_word(t: &str) -> &str {
    let end = t
        .bytes()
        .position(|b| b.is_ascii_whitespace() || WORD_BREAKS.contains(&b))
        .unwrap_or(t.len());
    &t[..end]
}

/// What follows `coproc`: past its NAME when an identifier precedes a `(` or
/// a [`COMPOUND_OPENERS`] word, as `keyword_words` reads it.
fn coproc_body(after: &str) -> &str {
    let name = first_word(after);
    let next = after[name.len()..].trim_start();
    if is_identifier(name)
        && (next.starts_with('(') || COMPOUND_OPENERS.contains(&first_word(next)))
    {
        next
    } else {
        after
    }
}

/// Whether a peeled command is a shape whose commands the walker cannot place
/// (#9127): a `case` (its paren-form patterns read as balanced groups), or a
/// function definition (`function f …`, `f() …`), whose body runs where the
/// function is called.
fn cannot_place(command: &str) -> bool {
    let word = first_word(command);
    let after = command[word.len()..].trim_start();
    matches!(word, "case" | "function")
        // `arr=()` is an empty array, not a definition.
        || (!word.is_empty()
            && !word.contains('=')
            && after
                .strip_prefix('(')
                .is_some_and(|p| p.trim_start().starts_with(')')))
}

/// Cut `body` at its first live `)` that closes a paren `body` did not open,
/// popping that subshell and every one a later live `)` closes.
///
/// What: `(command length, subshells closed)`. Quoted bytes, here-document
/// bodies and backslash-escaped bytes are not live. A `(` the body opens and
/// never closes, or one after the command ended, is [`Unbalanced`].
fn trailing_closers(body: &str, open: &mut Vec<Group>) -> Result<(usize, usize), Unbalanced> {
    let quotes = QuoteScan::new(body);
    let heredocs = HeredocBodies::scan(body);
    let live = |i: usize| {
        (!quotes.balanced || quotes.is_unquoted(i))
            && !heredocs.contains(i)
            && !heredocs.suppresses_separator(i)
    };
    let bytes = body.as_bytes();
    let (mut depth, mut leaves) = (0usize, 0usize);
    let mut end = None;
    let mut i = 0;
    while i < bytes.len() {
        if !live(i) {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\\' => i += 1,
            b'(' if end.is_some() => return Err(Unbalanced),
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b')' => {
                pop(open, Group::Subshell)?;
                leaves += 1;
                end.get_or_insert(i);
            }
            _ => {}
        }
        i += 1;
    }
    if depth > 0 {
        return Err(Unbalanced);
    }
    Ok((end.unwrap_or(bytes.len()), leaves))
}

/// Close the innermost group, which must be of kind `kind`.
fn pop(open: &mut Vec<Group>, kind: Group) -> Result<(), Unbalanced> {
    if open.pop() == Some(kind) {
        Ok(())
    } else {
        Err(Unbalanced)
    }
}

/// Whether `command` names `git` and `commit` as words anywhere (#9127).
///
/// Why: the fail-closed test for a command the walker could not place. It is
/// deliberately loose — no argv, no position — because the parse that would
/// place the words is the one that failed.
/// What: [`mentions_git_verb`] asking for the word `commit`.
/// Test: `mentions_git_commit_reads_words_not_substrings`.
pub(super) fn mentions_git_commit(command: &str) -> bool {
    mentions_git_verb(command, |verb, _| verb == "commit").is_some()
}

/// The first word of `command` that `matches` accepts as a git verb, given
/// every word after it, when a word whose basename is `git` is present too
/// (#9127).
///
/// What: splits on whitespace, on `;&|()<>` and backticks, and on `,{}` so a
/// brace expansion `{git,-C,<dir>,commit}` reads as its words (critic
/// MEDIUM-3); deletes every quote and backslash inside each word (so
/// `c''ommit`, `g"i"t` and `co\mmit` read as the words bash runs) and trims
/// `!$` from its ends. Empty words are dropped. Loose by design, as
/// [`mentions_git_commit`]: the words after a verb may belong to a later
/// command.
/// Test: `mentions_git_commit_reads_words_not_substrings`,
/// `mentions_git_verb_hands_the_matcher_the_words_after_it`.
pub(super) fn mentions_git_verb(
    command: &str,
    matches: impl Fn(&str, &[String]) -> bool,
) -> Option<String> {
    let words: Vec<String> = command
        .split(|c: char| c.is_whitespace() || ";&|()<>`,{}".contains(c))
        .map(|w| {
            let bare: String = w
                .chars()
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect();
            bare.trim_matches(|c: char| "!$".contains(c)).to_string()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let git = words
        .iter()
        .any(|w| w.rsplit('/').next().is_some_and(|base| base == "git"));
    if !git {
        return None;
    }
    (0..words.len())
        .find(|&i| matches(&words[i], &words[i + 1..]))
        .map(|i| words[i].clone())
}

/// Whether a segment's program word is a brace expansion (#9127).
///
/// Why: `{git,-C,<main>,commit}` is the command `git -C <main> commit` once
/// bash expands it, while every rule reads the single word `{git,-C,…}` and
/// matches nothing — the same blind spot as `$'…'` quoting, and refused the
/// same way, from `unclassifiable_command`.
/// What: shlex-splits `segment`, drops leading [`KEYWORDS`] words and the `(`
/// a subshell opens, and asks [`is_brace_expansion`] of the first word left
/// and of the program word `resolve_program_word` finds behind a wrapper
/// (`env {git,commit}`). A quoted `'{a,b}'` program reads the same; no real
/// program is named that.
/// Test: `brace_expanded_program_word_is_unclassifiable`.
pub(super) fn has_brace_expanded_program(segment: &str) -> bool {
    let Some(argv) = shlex::split(segment) else {
        return false;
    };
    let words: Vec<&str> = argv
        .iter()
        .map(|w| w.trim_start_matches('('))
        .skip_while(|w| w.is_empty() || KEYWORDS.contains(w) || *w == "-p")
        .collect();
    let resolved = crate::commands::program_word::resolve_program_word(&words)
        .ok()
        .and_then(|word| words.get(word.index).copied());
    words
        .first()
        .into_iter()
        .copied()
        .chain(resolved)
        .any(is_brace_expansion)
}

/// Whether `word` holds a `{…}` with a `,` or `..` in it that is not `${…}`.
fn is_brace_expansion(word: &str) -> bool {
    word.match_indices('{').any(|(at, _)| {
        !word[..at].ends_with('$')
            && word[at + 1..].find('}').is_some_and(|end| {
                let inner = &word[at + 1..at + 1 + end];
                inner.contains(',') || inner.contains("..")
            })
    })
}

/// The refusal for a program word bash builds by brace expansion (#9127).
pub(super) const BRACE_EXPANSION_REASON: &str = "this command's program word is a brace \
     expansion (`{git,-C,<dir>,commit}`), which bash turns into a different command than the \
     word the guard reads, so it cannot establish what would actually run. Spell the command \
     out word by word.";

/// The refusal for a `git commit` the guard cannot place.
pub(super) const UNPARSED_GROUP_COMMIT_REASON: &str = "Commit denied because the guard cannot \
     place it (ADR-0061, #9127): this command carries `git commit` inside a `( … )` subshell, \
     a `{ …; }` brace group, a function definition, a `case`, a `coproc` or an `eval` behind \
     `command`/`noglob` whose openers and closers the guard cannot pair up or whose commands \
     it cannot follow, so it cannot tell \
     which checkout the commit lands in, and a commit never lands on a local main checkout. A \
     stray `)` or `}` counts too — in a `#` comment or an unquoted regex — so quote it or drop \
     the comment. Run the commit as a plain command — `git -C \
     /abs/path/.claude/worktrees/<name> commit …`, or `cd` into the worktree first — with no \
     grouping around it. Nothing is lost; the changes are still in the tree.";

/// The refusal for a destructive git verb the guard cannot place (#9127
/// critic MEDIUM-2).
pub(super) fn unparsed_group_destructive_reason(verb: &str) -> String {
    format!(
        "Destructive git command denied because the guard cannot place it (ADR-0037, #9127): \
         this command carries `git {verb}` in a form that discards work, inside a `( … )` \
         subshell, a `{{ …; }}` brace group, a function definition, a `case`, a `coproc` or an \
         `eval` behind `command`/`noglob` whose openers and closers the guard cannot pair up or whose commands it cannot follow, so it \
         cannot tell whether it lands in a main checkout. Run it as a plain command — `git -C \
         /abs/path/.claude/worktrees/<name> {verb} …`, or `cd` into the worktree first — with \
         no grouping around it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(groups: &Groups) -> Vec<&str> {
        groups
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Command(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn grouped_steps_peels_subshells_and_brace_groups() {
        for (command, expected) in [
            ("(git commit -m x)", vec!["git commit -m x"]),
            ("( ( git commit ) )", vec!["git commit"]),
            ("((git commit))", vec!["git commit"]),
            ("{ git commit; }", vec!["git commit"]),
            ("{ git commit; } > /dev/null 2>&1", vec!["git commit"]),
            (
                "(git commit) 2>&1 && echo ok",
                vec!["git commit", "echo ok"],
            ),
            ("( { git commit; } )", vec!["git commit"]),
            ("(cd x && git commit)", vec!["cd x", "git commit"]),
            (
                "if true; then git commit; fi",
                vec!["true", "git commit", "fi"],
            ),
            ("time -p git commit", vec!["git commit"]),
            ("! git commit", vec!["git commit"]),
            ("arr=(a b); git commit", vec!["arr=(a b)", "git commit"]),
            ("echo $(date) x", vec!["echo $(date) x"]),
        ] {
            let groups = grouped_steps(command);
            assert!(groups.parsed, "`{command}` must parse");
            assert_eq!(commands(&groups), expected, "{command}");
        }
        let groups = grouped_steps("(cd x && git commit); git log");
        assert_eq!(
            groups.steps,
            [
                Step::Enter,
                Step::Command("cd x".into()),
                Step::Command("git commit".into()),
                Step::Leave,
                Step::Command("git log".into()),
            ]
        );
    }

    #[test]
    fn grouped_steps_brackets_a_wrapper_string() {
        let groups = grouped_steps("sh -c '(git commit)' && git log");
        assert!(groups.parsed);
        assert_eq!(
            groups.steps,
            [
                Step::Command("sh -c '(git commit)'".into()),
                Step::Enter,
                Step::Enter,
                Step::Command("git commit".into()),
                Step::Leave,
                Step::Leave,
                Step::Command("git log".into()),
            ]
        );
    }

    #[test]
    fn grouped_steps_reports_an_unbalanced_group() {
        for command in [
            "(git commit",
            "{ git commit",
            "git commit; }",
            "git commit )",
            "{ git commit )",
            "( git commit; }",
            "(a) (b)",
            "git commit (",
            "f() { git commit; }; f",
            "sh -c '(git commit'",
        ] {
            let groups = grouped_steps(command);
            assert!(!groups.parsed, "`{command}` must not parse");
            // Unparsed falls back to the flat pre-#9127 segments.
            assert_eq!(
                commands(&groups),
                split_shell_segments(command)
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn grouped_steps_ignores_quoted_and_heredoc_parens() {
        for command in [
            "git commit -m 'case a) and b)'",
            "git commit -m \"fix(x: y\"",
            "git commit -m a\\)",
            "git commit -F- <<'EOF'\nfix: x\n\n1) one\nEOF",
        ] {
            let groups = grouped_steps(command);
            assert!(groups.parsed, "`{command}` must parse");
            assert_eq!(commands(&groups).len(), 1, "{command}");
        }
    }

    /// #9127 critic HIGH-1: `eval` runs in the current shell, so its string is
    /// not bracketed; every child-process carrier's string is.
    #[test]
    fn grouped_steps_scopes_a_child_shell_but_not_eval() {
        let eval = grouped_steps("eval \"cd /m\"; git commit");
        assert!(eval.parsed);
        assert_eq!(
            eval.steps,
            [
                Step::Command("eval \"cd /m\"".into()),
                Step::Command("cd /m".into()),
                Step::Command("git commit".into()),
            ]
        );
        for carrier in [
            "sh -c 'cd /m'",
            "env -S 'cd /m'",
            "flock /tmp/l -c 'cd /m'",
            "xargs cd /m",
        ] {
            let groups = grouped_steps(&format!("{carrier}; git commit"));
            assert!(groups.parsed, "{carrier}");
            assert_eq!(groups.steps[1], Step::Enter, "{carrier}");
            assert_eq!(groups.steps[3], Step::Leave, "{carrier}");
        }
        // #9127 critic r3: the words before `eval` decide its scope.
        for (prefix, scope) in [
            ("A=1 ", Scope::Current),
            ("builtin ", Scope::Current),
            ("A=1 builtin ", Scope::Current),
            ("env ", Scope::Child),
            ("nohup ", Scope::Child),
            ("timeout 5 ", Scope::Child),
            ("command ", Scope::Unplaceable),
            ("command -p ", Scope::Unplaceable),
            ("command -- ", Scope::Unplaceable),
            ("builtin -- ", Scope::Unplaceable),
            ("/bin/builtin ", Scope::Unplaceable),
            ("noglob ", Scope::Unplaceable),
            ("nocorrect ", Scope::Unplaceable),
        ] {
            let command = format!("{prefix}eval 'cd /m'");
            assert_eq!(
                shell_lex::wrapped_command_scoped(&command).1,
                scope,
                "{command}"
            );
            let parsed = grouped_steps(&format!("{command}; git commit")).parsed;
            assert_eq!(parsed, scope != Scope::Unplaceable, "{command}");
        }
    }

    /// #9127 critic HIGH-2: `coproc` is a leading keyword, NAME included, and
    /// what it runs is a child shell.
    #[test]
    fn grouped_steps_peels_and_scopes_a_coproc() {
        for (command, expected) in [
            ("coproc git commit", vec!["git commit"]),
            ("coproc NAME { git commit; }", vec!["git commit"]),
            ("coproc NAME ( git commit )", vec!["git commit"]),
            ("coproc { git commit; }", vec!["git commit"]),
            ("(coproc git commit)", vec!["git commit"]),
        ] {
            let groups = grouped_steps(command);
            assert!(!groups.parsed, "`{command}` is a coproc, never placed");
            assert_eq!(commands(&groups), expected, "{command}");
            assert_eq!(groups.steps.first(), Some(&Step::Enter), "{command}");
            assert_eq!(groups.steps.last(), Some(&Step::Leave), "{command}");
        }
        let groups = grouped_steps("coproc NAME while true; do git commit; done");
        assert!(!groups.parsed);
    }

    /// #9127 critic HIGH-3a: a paren-form `case` arm and a function definition
    /// balance, but their commands do not run where they stand.
    #[test]
    fn grouped_steps_marks_shapes_it_cannot_place() {
        for command in [
            "case x in (x) git commit;; esac",
            "f() ( git commit ); f",
            "f () ( git commit )",
            "function f ( git commit )",
            "sh -c 'case x in (x) git commit;; esac'",
        ] {
            assert!(!grouped_steps(command).parsed, "`{command}` must not parse");
        }
        for command in ["arr=(); git commit", "git commit -m 'case (x) f() coproc'"] {
            assert!(grouped_steps(command).parsed, "`{command}` must parse");
        }
    }

    #[test]
    fn mentions_git_commit_reads_words_not_substrings() {
        for command in [
            "(git commit",
            "{ /usr/bin/git -C x commit",
            "(\"git\" commit",
            // #9127 critic HIGH-3b: quotes and backslashes inside a word.
            "(g''it c''ommit",
            "(git co\\mmit",
            "(g\"i\"t commit",
            "(cd x;git commit",
            // #9127 critic MEDIUM-3: a brace expansion off the program word.
            "case x in (x) {git,-C,/m,commit,-a};; esac",
            "f() { {git,commit}; }",
        ] {
            assert!(mentions_git_commit(command), "{command}");
        }
        for command in [
            "(git status",
            "git log --grep=commit",
            "(commit-msg",
            "(git add src/{a,b}.rs",
        ] {
            assert!(!mentions_git_commit(command), "{command}");
        }
    }

    /// #9127 critic MEDIUM-2: the destructive fail-closed check reads the verb
    /// with the words after it, so a harmless form is not refused.
    #[test]
    fn mentions_git_verb_hands_the_matcher_the_words_after_it() {
        let hard = |verb: &str, tail: &[String]| {
            verb == "reset" && tail.first().is_some_and(|t| t == "--hard")
        };
        assert_eq!(
            mentions_git_verb("coproc x; (git reset --hard", hard).as_deref(),
            Some("reset")
        );
        assert_eq!(mentions_git_verb("(git reset HEAD", hard), None);
        assert_eq!(mentions_git_verb("(reset --hard", hard), None);
    }
}
