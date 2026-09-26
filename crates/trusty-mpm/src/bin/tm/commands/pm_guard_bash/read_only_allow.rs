//! A read-only dispatch runs only allowlisted command shapes (#8439).
//!
//! Why: on 2026-09-23 a `research` subagent dispatched read-only ran
//! `plutil -extract ProgramArguments json <file>` over eight LaunchAgent
//! plists; without `-o -` that form writes into its input, and all eight were
//! truncated. Read-only existed only as prompt text. The first fix enumerated
//! writers and classified the rest of the shell as safe; three critic rounds
//! each found a construct it half-read, and its lexer could be made to spin
//! until the hook timed out and the command ran. This rule inverts that: it
//! names the reads, and a command that is not one of them is refused.
//! What: for a caller whose `agent_type` is in [`READ_ONLY_DISPATCH_AGENTS`],
//! [`evaluate_read_only_dispatch_command`] allows a `Bash` command only when
//! [`super::read_only_lex`] lexes it and it has one of three shapes:
//! - a pipeline, `cmd (| reader)*`;
//! - `if <pipeline>; then <body>; [else <body>;] fi`;
//! - `for NAME in <words>; do <body>; done`, where the words are literals not
//!   led by `-` or bare paths led by `/` or `~/`, and `"$NAME"` is the only
//!   variable the body may name.
//!
//! Any of the three may follow one leading `cd <dir> &&`, where `<dir>` is a
//! literal that [`super::path_tokens::unresolved_target`] can resolve (#8578).
//! A body is one or more pipelines separated by `;` or newlines. Every command
//! must be on the allowlist in [`super::read_only_programs`]. Everything else
//! is refused, including any other `&&`, `||`, `&`, any redirect other than `2>&1` and
//! `2>/dev/null`, an assignment or wrapper before the program, any
//! expansion outside the `for` shape, and a double-quoted `\` escape or `$`
//! outside an `rg`/`grep` argument (#8586). No filesystem or daemon is consulted, so
//! the rule has no I/O arm to fail open through.
//! Out of scope: the `Write`, `Edit` and `NotebookEdit` tools; what a user's
//! own shell aliases and functions do; `cargo metadata`/`cargo tree` updating
//! `Cargo.lock` or the cargo registry cache; and a program run by
//! configuration that already exists — git's `core.fsmonitor`,
//! `diff.external`, textconv drivers, `gpg.program`, `core.pager`, and a
//! `.cargo/config.toml` `rustc`/`rustc-wrapper` that cargo runs to read the
//! host target, and an `rg` config file named by `RIPGREP_CONFIG_PATH`, which
//! can carry `--pre`.
//! Test: `read_only_allow_tests` — `refuses_the_incident_plutil_extract_json_form`,
//! `legitimate_reads_stay_allowed`, `critic_round_three_probes_are_refused`,
//! `a_leading_cd_reaches_another_worktree`, `a_leading_cd_never_admits_a_write`.

use std::path::Path;

use super::path_tokens::unresolved_target;
use super::read_only_lex::{Tok, Word, WordKind, lex};
use super::read_only_programs::{Arg, check_command};
use super::worktree_remove::DispatchIdentity;

/// The agents the rule binds: a fixed list of read-only roles (#8439).
///
/// Why: the bundled `tools:` grant cannot derive this — `research` carries
/// `Write`, while `local-ops`, `version-control` and the other ops agents
/// change state through `Bash` by design.
/// What: matched exactly against the payload's `agent_type`.
pub(super) const READ_ONLY_DISPATCH_AGENTS: &[&str] = &[
    "research",
    "code-critic",
    "code-analyzer",
    "security",
    "Explore",
    "Plan",
];

/// Deny a read-only dispatch's command unless it is an allowlisted read.
///
/// Why: see the module doc.
/// What: `None` when `agent_type` is absent or not in
/// [`READ_ONLY_DISPATCH_AGENTS`], or the command is an allowlisted shape.
/// `Some(reason)` otherwise — every parse or classification failure is a
/// deny.
/// Test: `refuses_the_incident_plutil_extract_json_form`,
/// `only_the_read_only_agents_are_bound`, `legitimate_reads_stay_allowed`.
pub(crate) fn evaluate_read_only_dispatch_command(
    command: &str,
    identity: DispatchIdentity<'_>,
) -> Option<String> {
    let agent = identity.agent_type?;
    if !READ_ONLY_DISPATCH_AGENTS.contains(&agent) {
        return None;
    }
    judge(command).err().map(|what| deny_reason(agent, &what))
}

/// `Ok` when `command` is an allowlisted shape; `Err` names the first refusal.
pub(super) fn judge(command: &str) -> Result<(), String> {
    let toks = lex(command)?;
    let mut p = Parser { toks: &toks, at: 0 };
    p.skip_seps();
    // #8578: a read-only agent inherits the PM's cwd; one leading `cd` lets it
    // point at another worktree. Every other `&&` stays refused.
    p.cd_prefix()?;
    if toks[p.at..].contains(&Tok::AndIf) {
        return Err("`&&` other than after one leading `cd <dir>`".into());
    }
    if p.keyword("if") {
        p.if_shape()?;
    } else if p.keyword("for") {
        p.for_shape()?;
    } else {
        p.pipeline(None)?;
    }
    p.skip_seps();
    if p.at < toks.len() {
        return Err(
            "more than one command, which a read-only agent runs one call at a time".into(),
        );
    }
    Ok(())
}

/// A cursor over the token list. Every method advances or returns `Err`.
struct Parser<'a> {
    toks: &'a [Tok],
    at: usize,
}

impl Parser<'_> {
    /// Skip any run of separators.
    fn skip_seps(&mut self) {
        while self.toks.get(self.at) == Some(&Tok::Sep) {
            self.at += 1;
        }
    }

    /// Consume the bare keyword `kw` when it stands next.
    fn keyword(&mut self, kw: &str) -> bool {
        let hit = matches!(self.toks.get(self.at), Some(Tok::Word(w)) if w.is_keyword(kw));
        if hit {
            self.at += 1;
        }
        hit
    }

    /// Require the bare keyword `kw`.
    fn expect(&mut self, kw: &str) -> Result<(), String> {
        if self.keyword(kw) {
            return Ok(());
        }
        Err(format!("a compound command without its `{kw}`"))
    }

    /// Consume a leading `cd <dir> &&` (#8578).
    ///
    /// Why: a read-only dispatch inherits the PM's cwd, so scanning another
    /// worktree needs `cd <wt> && git diff A..B`, which the `&&` refusal broke.
    /// What: no-op unless the next word is `cd`. Then requires exactly one
    /// literal, non-option `<dir>` that `unresolved_target` resolves (no `$`,
    /// `~`, `$(…)` or backtick), then `&&`, then a word. `cd` itself changes
    /// no file; what follows is judged as a whole command.
    /// Test: `a_leading_cd_reaches_another_worktree`,
    /// `a_leading_cd_never_admits_a_write`.
    fn cd_prefix(&mut self) -> Result<(), String> {
        if !matches!(self.toks.get(self.at), Some(Tok::Word(w)) if w.is_keyword("cd")) {
            return Ok(());
        }
        let dir = match self.toks.get(self.at + 1..self.at + 4) {
            // #8586: pattern text is an rg/grep argument, never a directory.
            Some([Tok::Word(d), Tok::AndIf, Tok::Word(_)]) if !d.pattern => d.lit(),
            _ => None,
        };
        let plain = dir.filter(|d| {
            !d.is_empty() && !d.starts_with('-') && unresolved_target(Path::new(d)).is_none()
        });
        if plain.is_none() {
            return Err("a `cd` other than `cd <plain directory> && <read>`".into());
        }
        self.at += 3;
        Ok(())
    }

    /// Require at least one separator.
    fn separator(&mut self) -> Result<(), String> {
        if self.toks.get(self.at) != Some(&Tok::Sep) {
            return Err("a compound command missing a `;`".into());
        }
        self.skip_seps();
        Ok(())
    }

    /// `if <pipeline>; then <body> [else <body>] fi`.
    fn if_shape(&mut self) -> Result<(), String> {
        self.pipeline(None)?;
        self.separator()?;
        self.expect("then")?;
        self.body(None, &["else", "fi"])?;
        if self.keyword("else") {
            self.body(None, &["fi"])?;
        }
        self.expect("fi")
    }

    /// `for NAME in <words>; do <body> done`.
    fn for_shape(&mut self) -> Result<(), String> {
        let name = match self.toks.get(self.at) {
            // #8439 round 2: a fixed set of names, so a loop can never assign
            // `HOME`, `PATH`, `IFS` or a zsh special such as `path`.
            Some(Tok::Word(w)) if w.bare && w.lit().is_some_and(|t| LOOP_NAMES.contains(&t)) => {
                w.text.clone()
            }
            _ => return Err("a `for` loop variable outside the guard's name list".into()),
        };
        self.at += 1;
        self.expect("in")?;
        let mut words = 0;
        while let Some(Tok::Word(w)) = self.toks.get(self.at) {
            let flag_like = w.lit().is_none_or(|t| t.starts_with('-'));
            // #8586: pattern text never reaches a loop variable.
            if w.pattern || (w.kind != WordKind::Expanding && flag_like) {
                return Err("a `for` list word that is not a literal operand".into());
            }
            words += 1;
            self.at += 1;
        }
        if words == 0 {
            return Err("a `for` loop with an empty list".into());
        }
        self.separator()?;
        self.expect("do")?;
        self.body(Some(&name), &["done"])?;
        self.expect("done")
    }

    /// Pipelines separated by `;`, ending before one of the keywords `end`.
    fn body(&mut self, var: Option<&str>, end: &[&str]) -> Result<(), String> {
        loop {
            self.pipeline(var)?;
            self.separator()?;
            let at_end = matches!(
                self.toks.get(self.at),
                Some(Tok::Word(w)) if end.iter().any(|kw| w.is_keyword(kw))
            );
            if at_end {
                return Ok(());
            }
        }
    }

    /// `cmd (| reader)*`; `var` is the one `for` variable a word may name.
    fn pipeline(&mut self, var: Option<&str>) -> Result<(), String> {
        let mut stage = 0;
        loop {
            let args = self.command(var)?;
            check_command(&args, stage > 0)?;
            stage += 1;
            if self.toks.get(self.at) != Some(&Tok::Pipe) {
                return Ok(());
            }
            self.at += 1;
        }
    }

    /// One simple command's words, then any `2>&1`/`2>/dev/null`.
    ///
    /// What: a word carrying [`Word::pattern`] text is accepted only when the
    /// program is `rg` or `grep` (#8586).
    /// Test: `read_only_allow_tests::shell_syntax_around_quoted_patterns_is_refused`.
    fn command(&mut self, var: Option<&str>) -> Result<Vec<Arg>, String> {
        let mut args = Vec::new();
        let mut pattern = false;
        while let Some(Tok::Word(w)) = self.toks.get(self.at) {
            if args.is_empty() && is_reserved(w) {
                return Err(format!("the shell keyword `{}` here", w.text));
            }
            pattern |= w.pattern;
            args.push(to_arg(w, var)?);
            self.at += 1;
        }
        while self.toks.get(self.at) == Some(&Tok::StderrRedirect) {
            self.at += 1;
        }
        if args.is_empty() {
            return Err("an empty command".into());
        }
        // #8586: a double-quoted `\` escape or literal `$` is regex text for
        // rg/grep; every other program keeps the strict quoting rule.
        if pattern && !matches!(args[0].text(), Some("rg" | "grep")) {
            return Err(
                "a `\\` escape or `$` inside double quotes outside an `rg`/`grep` argument".into(),
            );
        }
        Ok(args)
    }
}

/// A word as an argument: literal, or the `for` variable as an operand.
fn to_arg(w: &Word, var: Option<&str>) -> Result<Arg, String> {
    match &w.kind {
        WordKind::Lit => Ok(Arg::Lit {
            text: w.text.clone(),
            bare: w.bare,
        }),
        WordKind::Var(name) if Some(name.as_str()) == var => Ok(Arg::Operand),
        WordKind::Var(_) => Err("a variable other than the `for` loop's own".into()),
        WordKind::Expanding => Err("a glob or `~` outside a `for` list".into()),
    }
}

/// Words the shell reads as grammar in command position.
fn is_reserved(w: &Word) -> bool {
    const RESERVED: &[&str] = &[
        "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case",
        "esac", "select", "function", "coproc", "time", "repeat", "foreach", "end", "!", "{", "}",
        "[[", "]]",
    ];
    w.bare && w.lit().is_some_and(|t| RESERVED.contains(&t))
}

/// The names a `for` loop may bind (#8439 round 2).
///
/// Why: a loop assigns its variable, so `for HOME in /tmp` points git at
/// another user config and `for PATH in …` or zsh's `path` changes which
/// program runs. None of these names means anything to bash, zsh or a tool.
const LOOP_NAMES: &[&str] = &[
    "f", "file", "p", "plist", "d", "dir", "x", "i", "n", "line", "name", "item",
];

/// The deny text for a read-only dispatch's refused command (#8439).
fn deny_reason(agent: &str, what: &str) -> String {
    // #8567: names the gh read verbs and `date`. #8586: the quoted-pattern hint.
    format!(
        "Read-only dispatch refused a command (#8439): `{agent}` is a read-only agent, and this \
         command has {what}. A read-only agent runs only allowlisted reads, one per call, with \
         literal arguments: git [-C <dir>] status/log/diff/show/grep/rev-parse/ls-files/\
         merge-base/ls-remote/branch --list/worktree list; cat/head/tail/wc/ls/grep/rg; find without \
         -exec/-delete/-fprint; sed -n with a print script; plutil -p/-lint; defaults read; \
         launchctl print/list; tmux capture-pane -p; cargo metadata/tree; gh issue view/list, \
         gh pr view/list/diff/checks, gh run view/list, gh api (GET only); date; echo; pwd. A \
         pipe into cat/head/tail/wc/grep/rg/sed is allowed, and so are `2>&1`, `2>/dev/null` \
         and one leading `cd <dir> &&` with a literal path. An rg/grep pattern may carry `\\` \
         escapes and an end-of-line `$` inside quotes; single quotes are the safest. If the \
         task needs a write, report back so the PM dispatches a writing agent."
    )
}

#[cfg(test)]
#[path = "read_only_allow_tests.rs"]
mod read_only_allow_tests;
