//! Shell grouping syntax peeled off each segment before the git-verb rules
//! read it (#9127).
//!
//! Why: the main-checkout walker classified each composition segment on its
//! first word, so `(git -C <main> commit -a)` lexed as the program `(git` and
//! `{ git -C <main> commit -a; }` as the program `{`, and the commit rule
//! (ADR-0061: no commit lands on a local main checkout) never saw the commit.
//! A reserved word in command position (`then git commit`, `! git commit`)
//! hid it the same way. The walker also has to know where a subshell ends: a
//! `cd` inside `( … )` or a `sh -c` string does not move the caller, and
//! reading it as if it did cleared `sh -c 'cd <wt>' && git commit -a` against
//! the worktree while the commit ran in the main checkout.
//!
//! What: [`grouped_steps`] turns a command into [`Step`]s — each segment's
//! command text with its leading `(`, `{`, `}`, `)` and reserved words peeled
//! off and its trailing group closers cut, bracketed by [`Step::Enter`] and
//! [`Step::Leave`] wherever a subshell or a wrapper's child shell begins and
//! ends. A brace group runs in the current shell, so it brackets nothing. A
//! command whose grouping does not balance comes back flat and marked
//! unparsed; the commit rule refuses it when it carries a `git commit`
//! ([`mentions_git_commit`]).
//!
//! Residuals: a `case` pattern's `)` reads as a stray closer, so a `case`
//! carrying a commit is refused as unparsed; a function body runs where the
//! function is CALLED, which this does not follow, so its stray `}` marks it
//! unparsed instead; `$(…)` and backticks are not descended, as the module
//! doc of `main_checkout` already states.
//!
//! Test: `shell_groups::tests`, and the `#9127` rows in `main_checkout`'s
//! suite.

use super::heredoc::HeredocBodies;
use super::shell_lex::{self, QuoteScan};
use super::{MAX_WRAPPER_DEPTH, split_shell_segments, split_shell_segments_raw};

/// Shell words that may stand in command position without being the command.
const LEADING_KEYWORDS: &[&str] = &[
    "!", "if", "then", "else", "elif", "do", "while", "until", "time",
];

/// Bytes that end a shell word as well as whitespace does.
const WORD_BREAKS: &[u8] = b"();&|<>";

/// One unit of the walk [`grouped_steps`] hands the git-verb rules.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// A simple command's text, grouping syntax removed.
    Command(String),
    /// A child shell starts: a `(` subshell or a wrapper's command string.
    Enter,
    /// The child shell [`Step::Enter`] opened ends; its `cd`s end with it.
    Leave,
}

/// The steps of a command, and whether its grouping parsed.
pub(super) struct Groups {
    /// In command order. Flat segments, no `Enter`/`Leave`, when unparsed.
    pub(super) steps: Vec<Step>,
    /// `false` when a group opener or closer did not balance.
    pub(super) parsed: bool,
}

/// Which kind of group an opener started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Subshell,
    Brace,
}

/// The grouping did not balance.
struct Unbalanced;

/// Walk `command` into [`Step`]s (#9127).
///
/// Why: see the module doc.
/// What: every segment [`split_shell_segments_raw`] cuts, peeled by
/// [`peel_segment`], followed — as `expand_shell_segments` does — by the
/// segments of a leading `sh -c`/`bash -c`/`env -S`/`xargs`/`eval` wrapper's
/// string, bracketed by `Enter`/`Leave`, up to [`MAX_WRAPPER_DEPTH`] layers.
/// When any level's openers and closers do not pair up, the answer is the flat
/// [`split_shell_segments`] list with `parsed: false`, so a caller that ignores
/// the flag reads exactly what it read before #9127.
/// Test: `grouped_steps_peels_subshells_and_brace_groups`,
/// `grouped_steps_brackets_a_wrapper_string`,
/// `grouped_steps_reports_an_unbalanced_group`,
/// `grouped_steps_ignores_quoted_and_heredoc_parens`.
pub(super) fn grouped_steps(command: &str) -> Groups {
    let mut steps = Vec::new();
    match steps_into(command, 0, &mut steps) {
        Ok(()) => Groups {
            steps,
            parsed: true,
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
fn steps_into(command: &str, depth: usize, out: &mut Vec<Step>) -> Result<(), Unbalanced> {
    let mut open = Vec::new();
    for raw in split_shell_segments_raw(command) {
        let (text, leaves) = peel_segment(raw, &mut open, out)?;
        if let Some(text) = text {
            let inner = match shell_lex::wrapped_command(&text) {
                shell_lex::WrappedCommand::Inner(inner) if depth < MAX_WRAPPER_DEPTH => Some(inner),
                _ => None,
            };
            out.push(Step::Command(text));
            if let Some(inner) = inner {
                out.push(Step::Enter);
                steps_into(&inner, depth + 1, out)?;
                out.push(Step::Leave);
            }
        }
        out.extend(std::iter::repeat_with(|| Step::Leave).take(leaves));
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
/// `{` word (pushes a brace group), a `}` word or `)` (pops its own kind,
/// `Leave` for a subshell) and a [`LEADING_KEYWORDS`] word (`time` with its
/// `-p`). After a leading closer the rest is the group's redirections, not a
/// command. Otherwise the rest is cut at its first live `)` that closes a
/// paren this segment did not open ([`trailing_closers`]). Returns the
/// command, if any, and how many subshells close after it.
fn peel_segment(
    raw: &str,
    open: &mut Vec<Group>,
    out: &mut Vec<Step>,
) -> Result<(Option<String>, usize), Unbalanced> {
    let mut rest = raw;
    let mut closed = false;
    loop {
        let t = rest.trim_start();
        let word_end = t
            .bytes()
            .position(|b| b.is_ascii_whitespace() || WORD_BREAKS.contains(&b))
            .unwrap_or(t.len());
        let word = &t[..word_end];
        rest = if let Some(after) = t.strip_prefix('(') {
            open.push(Group::Subshell);
            out.push(Step::Enter);
            after
        } else if let Some(after) = t.strip_prefix(')') {
            pop(open, Group::Subshell)?;
            out.push(Step::Leave);
            closed = true;
            after
        } else if word == "{" {
            open.push(Group::Brace);
            &t[1..]
        } else if word == "}" {
            pop(open, Group::Brace)?;
            closed = true;
            &t[1..]
        } else if LEADING_KEYWORDS.contains(&word) {
            let after = t[word_end..].trim_start();
            match after.strip_prefix("-p") {
                Some(p) if word == "time" && p.starts_with(char::is_whitespace) => p,
                _ => after,
            }
        } else {
            break;
        };
    }
    if closed {
        return Ok((None, 0));
    }
    let body = rest.trim();
    let (end, leaves) = trailing_closers(body, open)?;
    let command = body[..end].trim();
    Ok(((!command.is_empty()).then(|| command.to_string()), leaves))
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
/// Why: the fail-closed test for a command whose grouping did not parse. It
/// is deliberately loose — no argv, no position — because the parse that
/// would place the words is the one that failed.
/// What: a whitespace-separated word, stripped of grouping and quoting
/// punctuation, equal to `commit`, and another whose basename is `git`.
/// Test: `mentions_git_commit_reads_words_not_substrings`.
pub(super) fn mentions_git_commit(command: &str) -> bool {
    let words: Vec<&str> = command
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| "(){}!;&|'\"\\".contains(c)))
        .collect();
    let git = words
        .iter()
        .any(|w| w.rsplit('/').next().is_some_and(|base| base == "git"));
    git && words.contains(&"commit")
}

/// The refusal for a `git commit` inside grouping the guard cannot parse.
pub(super) const UNPARSED_GROUP_COMMIT_REASON: &str = "Commit denied because its shell grouping \
     does not parse (ADR-0061, #9127): this command carries `git commit` inside a `( … )` \
     subshell, a `{ …; }` brace group, a function body or a `case` arm whose openers and closers \
     the guard cannot pair up, so it cannot tell which checkout the commit lands in, and a commit \
     never lands on a local main checkout. Run the commit as a plain command — `git -C \
     /abs/path/.claude/worktrees/<name> commit …`, or `cd` into the worktree first — with no \
     grouping around it. Nothing is lost; the changes are still in the tree.";

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

    #[test]
    fn mentions_git_commit_reads_words_not_substrings() {
        for command in [
            "(git commit",
            "{ /usr/bin/git -C x commit",
            "(\"git\" commit",
        ] {
            assert!(mentions_git_commit(command), "{command}");
        }
        for command in ["(git status", "git log --grep=commit", "(commit-msg"] {
            assert!(!mentions_git_commit(command), "{command}");
        }
    }
}
