//! Refuse a Bash command that would print a credential VALUE into tool output
//! (#8596, #8248).
//!
//! Why: a Bash tool result is transcript text. `security find-generic-password
//! -s svc -w | head -c 50` (#8596) and a bare `gcloud auth print-access-token`
//! (#8248) each put a live credential there, and no rule looked at them: the
//! secret-file rule (#7266) screens file NAMES, and these commands name no
//! file. Agent prose alone did not stop either leak.
//! What: [`evaluate_credential_print_command`] finds every credential-printing
//! call (`credential_print_programs::credential_fds`) and follows the
//! descriptor carrying the value through redirections, pipes, command and
//! process substitutions and `sh -c`/`xargs`/`env -S` wrappers. The command is
//! allowed when every such value ends in a file, `/dev/null`, a variable
//! assignment, an argument to a non-printing program, or a non-printing stdin
//! consumer. It is refused when a value reaches the terminal, a reader that
//! prints (`head`, `diff`, any program not known to swallow its input), an
//! `echo`/`printf`/`cat` argument, or the command-name position; when xtrace is
//! on; and when credential-command text is handed to an evaluator (`eval`,
//! `sh` on stdin, `ssh`, `python3 -c`, …) or run through a `$`-named program.
//!
//! Fail-closed: once the text names a credential subcommand, anything the
//! scanner cannot read — broken quoting, an unclosed substitution, nesting past
//! [`MAX_DEPTH`], an unlexable wrapper string, a descriptor chosen at run
//! time, a panic — denies. A command naming none of [`TRIGGERS`] is never
//! parsed at all.
//!
//! Known residual (unscored default): a program the rule has no fact for is
//! treated as non-printing when a credential is its argument. `awk -v t=… '…'`,
//! `jq --arg t …`, and a shell function or alias can each print the value and
//! are allowed; only [`ARG_PRINTERS`] are known to print their arguments.
//! Test: `credential_print_tests` (sibling module).

#[path = "credential_print_heredoc.rs"]
mod credential_print_heredoc;
#[path = "credential_print_programs.rs"]
mod credential_print_programs;
#[path = "credential_print_redirect.rs"]
mod credential_print_redirect;

use super::bash_tokens::tokenize;
use super::heredoc::HeredocBodies;
use super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use crate::commands::hook_rewrite::strip_wrapper_prefix;
use credential_print_heredoc::strip_comments_and_heredocs;
use credential_print_programs::{
    KEYWORDS, basename, code_operands, consumes_stdin, credential_fds, enables_xtrace,
    first_credential_program, is_evaluator,
};
use credential_print_redirect::{apply_redirections, terminal_name_sink};

/// Subcommand names whose presence makes the command worth parsing.
const TRIGGERS: &[&str] = &[
    "find-generic-password",
    "find-internet-password",
    "dump-keychain",
    "print-access-token",
    "print-identity-token",
    "config-helper",
];

/// Programs that print their arguments (or the files they name) to stdout.
const ARG_PRINTERS: &[&str] = &["echo", "printf", "print", "cat"];

/// Substitution and wrapper nesting the scanner follows before refusing.
const MAX_DEPTH: usize = 8;

/// Placeholder a lifted substitution leaves in the outer text.
const MARK: &str = "__TMCRED";

/// Where a descriptor's bytes end up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sink {
    /// Tool output.
    Terminal,
    /// The value of an enclosing `$(…)`/backtick/`<(…)`.
    Captured,
    /// A file, `/dev/null`, or a closed descriptor.
    Discarded,
    /// The next pipeline stage's stdin.
    Pipe,
}

/// Why a command was refused.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// A credential value reaches tool output.
    Prints,
    /// The scanner could not read the command; the value's path is unknown.
    Unreadable(&'static str),
}

/// A substitution's opener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubKind {
    /// `$(…)` or backticks: the value becomes text.
    Command,
    /// `<(…)`: the value becomes a file the program reads.
    Input,
    /// `>(…)`: a file the program writes, run with the outer stdout.
    Output,
}

/// A lifted substitution: its opener and whether its value carries a credential.
#[derive(Clone, Copy)]
struct Sub {
    kind: SubKind,
    yields: bool,
}

/// A stripped quoted-delimiter here-document.
#[derive(Clone, Copy)]
struct Heredoc {
    /// The body is text an evaluator reading it would run as a credential call.
    program_text: bool,
}

/// Placeholders known at one scan level; inner levels extend a clone, so an
/// outer index stays valid inside a wrapper string (#8596 finding 2).
#[derive(Clone, Default)]
struct Lifted {
    subs: Vec<Sub>,
    heredocs: Vec<Heredoc>,
}

/// Refuse a Bash command that prints a credential value: `Some(reason)` denies.
///
/// Why: see the module doc — the value must never reach tool output, and
/// `pm_guard` is the one place that sees the command before it runs.
/// What: a cheap case-insensitive trigger test over the quote-stripped text,
/// then [`scan`] at the top level, where stdout and stderr are both tool
/// output. A panic inside the scan is caught and refuses, so it can never exit
/// the hook 101 and fail open.
/// Test: `credential_print_tests::denies_the_reported_leaks`,
/// `credential_print_tests::allows_the_capturing_forms`,
/// `credential_print_tests::denies_what_it_cannot_read`,
/// `credential_print_tests::denies_the_round_two_bypasses`,
/// `credential_print_tests::no_prefix_of_a_command_panics`.
pub(crate) fn evaluate_credential_print_command(command: &str) -> Option<String> {
    if !has_trigger(command) {
        return None;
    }
    let result = std::panic::catch_unwind(|| {
        scan(
            command,
            Sink::Terminal,
            Sink::Terminal,
            0,
            &Lifted::default(),
        )
    })
    .unwrap_or(Err(Refusal::Unreadable("it (the scanner failed)")));
    let why = match result {
        Ok(_) => return None,
        Err(Refusal::Prints) => "it would print a credential value into the tool output, \
             where it becomes transcript text"
            .to_string(),
        Err(Refusal::Unreadable(what)) => format!(
            "it runs a credential-printing command and the guard cannot read {what}, so \
             it cannot prove the value stays out of the tool output"
        ),
    };
    Some(format!(
        "`tm hook --pm-guard` refused this command: {why} (#8596, #8248). {HOW_TO}"
    ))
}

/// The capture forms every deny reason points at.
const HOW_TO: &str = "Check existence by exit status alone \
     (`security find-generic-password -s <service> >/dev/null 2>&1`, no `-w`/`-g`), and \
     consume a value inside the command that needs it \
     (`curl -H \"Authorization: Bearer $(gcloud auth print-access-token)\" …`, or a pipe \
     to `--password-stdin`), never echoing it and never behind `set -x`.";

/// Whether `text`, quotes removed and lowercased, names a [`TRIGGERS`] entry.
fn has_trigger(text: &str) -> bool {
    let flat: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect::<String>()
        .to_ascii_lowercase();
    TRIGGERS.iter().any(|t| flat.contains(t))
}

/// Whether `text` handed to an evaluator could run a credential call: it names
/// a trigger, or expands something (`$`, a backtick, a lifted substitution).
fn input_is_program_text(text: &str) -> bool {
    has_trigger(text) || text.contains('$') || text.contains('`') || text.contains(MARK)
}

/// Scan `text` run with `stdout`/`stderr` as its sinks; `Ok(true)` when a
/// credential value flows into a [`Sink::Captured`] stdout.
///
/// What: strips comments and quoted here-document bodies, lifts every
/// substitution ([`lift_substitutions`]), splits the rest into stages
/// ([`split_stages`]) and judges each ([`judge_stage`]), carrying "stdin holds
/// a credential" and "stdin holds program text" from a stage piped into the
/// next; program text passes through any stage that is not a stdin consumer.
fn scan(
    text: &str,
    stdout: Sink,
    stderr: Sink,
    depth: usize,
    outer: &Lifted,
) -> Result<bool, Refusal> {
    if depth > MAX_DEPTH {
        return Err(Refusal::Unreadable("nesting this deep"));
    }
    let mut lifted = outer.clone();
    let text = strip_comments_and_heredocs(text, &mut lifted.heredocs);
    let flat = lift_substitutions(&text, stdout, stderr, depth, &mut lifted)?;
    let stages = split_stages(&flat);
    let mut yields = false;
    let (mut stdin_carries, mut stdin_text) = (false, false);
    for (idx, (stage, piped, pipe_stderr)) in stages.iter().enumerate() {
        let next_exists = stages.get(idx + 1).is_some();
        let out = if *piped && next_exists {
            Sink::Pipe
        } else {
            stdout
        };
        let err = if *pipe_stderr && next_exists {
            Sink::Pipe
        } else {
            stderr
        };
        let ctx = StageCtx {
            out,
            err,
            stdin_carries,
            stdin_text,
            depth,
        };
        let emitted = judge_stage(stage.trim(), &lifted, ctx)?;
        yields |= emitted.captured;
        stdin_carries = emitted.piped;
        stdin_text = *piped && emitted.text;
    }
    Ok(yields)
}

/// The sinks a stage starts with, before its own redirections.
#[derive(Clone, Copy)]
struct StageCtx {
    out: Sink,
    err: Sink,
    stdin_carries: bool,
    /// The previous stage piped program text in (`echo '…' | sh`).
    stdin_text: bool,
    depth: usize,
}

/// Where a stage's credential value went, and whether it wrote program text.
#[derive(Default)]
struct Emitted {
    captured: bool,
    piped: bool,
    text: bool,
}

/// Judge one pipeline stage.
///
/// What: resolves the stage's redirections, refuses xtrace, a run-time program
/// name and program text handed to an evaluator, descends into a wrapper, then
/// decides which descriptors carry a credential: the ones a
/// [`credential_fds`] call prints, stdout of an [`ARG_PRINTERS`] call given a
/// credential argument, and stdout of any stage whose stdin — or a `<(…)` file
/// it reads — carries one unless it is a non-printing consumer. Each carrying
/// descriptor is routed by [`route`].
fn judge_stage(stage: &str, lifted: &Lifted, ctx: StageCtx) -> Result<Emitted, Refusal> {
    let mut emitted = Emitted::default();
    if stage.is_empty() {
        return Ok(emitted);
    }
    let stage = ungroup(stage);
    let tokens = tokenize(&stage).map_err(|_| Refusal::Unreadable("its quoting"))?;
    let routed = apply_redirections(&tokens, ctx.out, ctx.err, lifted)?;
    let argv = &routed.argv;
    let reads_input = argv
        .iter()
        .any(|w| carries_kind(w, lifted, Some(SubKind::Input)));
    let stdin_carries = ctx.stdin_carries || routed.here_carries || reads_input;
    emitted.text = argv.iter().any(|w| input_is_program_text(w)) || routed.here_program_text;
    let kw = argv
        .iter()
        .take_while(|w| KEYWORDS.contains(&w.as_str()))
        .count();
    let resolved = argv.get(kw..).and_then(strip_wrapper_prefix);
    let start = kw + resolved.unwrap_or(0);
    let program_word = argv.get(start).map(String::as_str).unwrap_or_default();
    let program = basename(program_word);
    let args = argv.get(start + 1..).unwrap_or_default();
    let consumer = consumes_stdin(&program, args);
    // #8596 round 3: program text piped through a filter (`| cat | sh`).
    emitted.text |= ctx.stdin_text && !consumer;
    if enables_xtrace(&program, argv, args) {
        return Err(Refusal::Prints);
    }
    if program_word.is_empty() {
        // Assignments only (`T=$(…)`): nothing is printed.
        return Ok(emitted);
    }
    if carries(program_word, lifted) {
        // `$(print-access-token)` runs the value; the shell prints it in its
        // "command not found" error.
        return Err(Refusal::Prints);
    }
    let call = first_credential_program(argv);
    // #8596 finding 5: the program is chosen at run time (`$S find-…`).
    let dynamic_before_trigger = call.is_none()
        && argv.iter().position(|w| has_trigger(w)).is_some_and(|t| {
            argv.get(..t)
                .unwrap_or_default()
                .iter()
                .any(|w| w.starts_with('$'))
        });
    if program_word.starts_with('$') || program_word.starts_with(MARK) || dynamic_before_trigger {
        return Err(Refusal::Unreadable("a program name chosen at run time"));
    }
    let wrapped = wrapped_command(&stage);
    // A wrapper with flags (`sudo -u x bash -c …`) hides its program, so the
    // first evaluator word after it stands in.
    let evaluator_at = if resolved.is_some() {
        Some(start).filter(|_| is_evaluator(&program))
    } else {
        argv.iter()
            .skip(start)
            .position(|w| is_evaluator(&basename(w)))
            .map(|p| p + start)
    };
    if let Some(at) = evaluator_at.filter(|&at| at != start || wrapped == WrappedCommand::None) {
        let evaluator = basename(&argv[at]);
        // #8596 round 3: only code operands; `python3 up.py --token "$T"` is data.
        let fed = matches!(evaluator.as_str(), "eval" | "source" | ".")
            || code_operands(&evaluator, argv.get(at + 1..).unwrap_or_default())
                .into_iter()
                .any(input_is_program_text)
            || routed.here_program_text
            || ctx.stdin_text;
        if fed {
            return Err(Refusal::Unreadable("text handed to a program that runs it"));
        }
    }
    let (out, err) = (routed.out, routed.err);
    match wrapped {
        WrappedCommand::Unlexable => return Err(Refusal::Unreadable("its wrapped command")),
        WrappedCommand::Inner(inner) => {
            let inner_err = if err == Sink::Terminal {
                Sink::Terminal
            } else {
                Sink::Discarded
            };
            let prints = if program == "xargs" && stdin_carries {
                // #8596 finding 10: xargs hands stdin to its program as
                // arguments; only a printing program leaks them.
                let mut with_value = lifted.clone();
                let mark = format!("{MARK}{}__", with_value.subs.len());
                with_value.subs.push(Sub {
                    kind: SubKind::Command,
                    yields: true,
                });
                let text = inject_xargs_value(&inner, args, &mark);
                scan(&text, Sink::Captured, inner_err, ctx.depth + 1, &with_value)?
            } else {
                // A wrapper reading a credential on stdin (`| sh -c cat`)
                // may print it, so stdin carry routes too.
                scan(&inner, Sink::Captured, inner_err, ctx.depth + 1, lifted)? || stdin_carries
            };
            if prints {
                route(out, &mut emitted)?;
            }
            return Ok(emitted);
        }
        WrappedCommand::None => {}
    }
    // Found anywhere, not only at `start`: `timeout 5 gcloud …` resolves its
    // program to `5`.
    let (mut fd1, fd2) = call.map_or((false, false), |at| {
        credential_fds(&basename(&argv[at]), argv.get(at + 1..).unwrap_or_default())
    });
    if ARG_PRINTERS.contains(&program.as_str()) && args.iter().any(|a| carries(a, lifted)) {
        fd1 = true;
    }
    if stdin_carries && !consumer {
        fd1 = true;
        // #8596 round 3: a file operand naming a descriptor or the terminal
        // (`tee /dev/stderr`, `dd of=/dev/tty`, `tee >(cat)`).
        for a in args {
            let path = a.strip_prefix("of=").unwrap_or(a);
            if let Some(sink) = terminal_name_sink(path, &routed.fds, lifted, ctx.out) {
                route(sink, &mut emitted)?;
            }
        }
    }
    if fd1 {
        route(out, &mut emitted)?;
    }
    if fd2 {
        route(err, &mut emitted)?;
    }
    Ok(emitted)
}

/// Put the value xargs reads where its program receives it: at each
/// replace-string (`-I {}`, `-i`, `--replace`), else appended.
fn inject_xargs_value(inner: &str, xargs_args: &[String], mark: &str) -> String {
    let mut replace = None;
    let mut n = 0;
    // Only xargs' own options, which end at its program word.
    while let Some(a) = xargs_args.get(n) {
        n += 1;
        match a.as_str() {
            "-I" => {
                replace = xargs_args.get(n).cloned();
                n += 1;
            }
            "-i" | "--replace" => replace = Some("{}".to_string()),
            _ if !a.starts_with('-') => break,
            _ => {
                if let Some(v) = a
                    .strip_prefix("--replace=")
                    .or_else(|| a.strip_prefix("-I"))
                {
                    replace = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("-i") {
                    replace = Some(v.to_string());
                }
            }
        }
    }
    match replace.filter(|r| !r.is_empty() && inner.contains(r.as_str())) {
        Some(r) => inner.replace(r.as_str(), mark),
        None => format!("{inner} {mark}"),
    }
}

/// Blank unquoted grouping parens: `(gcloud …)` runs `gcloud` in a subshell.
fn ungroup(stage: &str) -> String {
    let quotes = QuoteScan::new(stage);
    if !quotes.balanced {
        return stage.to_string();
    }
    stage
        .char_indices()
        .map(|(i, c)| {
            if matches!(c, '(' | ')') && quotes.is_unquoted(i) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Record where one credential-carrying descriptor lands, refusing the terminal.
fn route(sink: Sink, emitted: &mut Emitted) -> Result<(), Refusal> {
    match sink {
        Sink::Terminal => return Err(Refusal::Prints),
        Sink::Captured => emitted.captured = true,
        Sink::Pipe => emitted.piped = true,
        Sink::Discarded => {}
    }
    Ok(())
}

/// Whether `word` holds a lifted substitution whose value is a credential.
fn carries(word: &str, lifted: &Lifted) -> bool {
    carries_kind(word, lifted, None)
}

/// [`carries`], limited to one substitution kind when `kind` is set.
fn carries_kind(word: &str, lifted: &Lifted, kind: Option<SubKind>) -> bool {
    marks_in(word).any(|i| {
        lifted
            .subs
            .get(i)
            .is_some_and(|s| s.yields && kind.is_none_or(|k| s.kind == k))
    })
}

/// The indices of every [`MARK`] placeholder in `word`.
fn marks_in(word: &str) -> impl Iterator<Item = usize> + '_ {
    word.match_indices(MARK).filter_map(|(at, _)| {
        let digits: String = word[at + MARK.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// Replace every live substitution in `text` with a [`MARK`] placeholder,
/// scanning each body first and appending it to `lifted.subs`.
///
/// What: the same opener/closer scan as `super::classify_command_substitutions`
/// — `$(…)`/`<(…)`/`>(…)` with paren counting, backtick pairs — on a
/// [`QuoteScan`] of `text`. `$(…)`, backtick and `<(…)` bodies run with a
/// captured stdout; a `>(…)` body writes to the outer stdout. An unclosed
/// opener is refused. Every slice starts and ends at an ASCII byte.
fn lift_substitutions(
    text: &str,
    stdout: Sink,
    stderr: Sink,
    depth: usize,
    lifted: &mut Lifted,
) -> Result<String, Refusal> {
    let scan_quotes = QuoteScan::new(text);
    let live = |i: usize| !scan_quotes.balanced || scan_quotes.allows_substitution(i);
    let unquoted = |i: usize| !scan_quotes.balanced || scan_quotes.is_unquoted(i);
    let bytes = text.as_bytes();
    let mut flat = String::with_capacity(text.len());
    let (mut i, mut copied) = (0, 0);
    while i < bytes.len() {
        let paren = bytes.get(i + 1) == Some(&b'(')
            && match bytes[i] {
                b'$' => live(i),
                b'<' | b'>' => unquoted(i),
                _ => false,
            };
        let (body, end) = if paren {
            let mut level = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && level > 0 {
                match bytes[j] {
                    b'(' => level += 1,
                    b')' => level -= 1,
                    _ => {}
                }
                j += 1;
            }
            if level != 0 {
                return Err(Refusal::Unreadable("an unclosed substitution"));
            }
            (&text[i + 2..j - 1], j)
        } else if bytes[i] == b'`' && live(i) {
            let close = text[i + 1..]
                .find('`')
                .ok_or(Refusal::Unreadable("an unclosed backtick"))?;
            (&text[i + 1..i + 1 + close], i + close + 2)
        } else {
            i += 1;
            continue;
        };
        let kind = match (paren, bytes[i]) {
            (true, b'<') => SubKind::Input,
            (true, b'>') => SubKind::Output,
            _ => SubKind::Command,
        };
        let body_out = if kind == SubKind::Output {
            stdout
        } else {
            Sink::Captured
        };
        let yields = scan(body, body_out, stderr, depth + 1, lifted)?;
        flat.push_str(&text[copied..i]);
        flat.push_str(&format!("{MARK}{}__", lifted.subs.len()));
        lifted.subs.push(Sub { kind, yields });
        i = end;
        copied = end;
    }
    flat.push_str(&text[copied..]);
    Ok(flat)
}

/// Split `text` into stages: `(stage, stdout piped on, stderr piped on)`.
///
/// What: cuts at unquoted `|`, `|&`, `||`, `&&`, `;`, newline and a bare `&`
/// (never at a `>&`/`<&`/`&>` redirection), leaving unquoted here-document bodies
/// whole the way `super::split_shell_segments_raw` does.
fn split_stages(text: &str) -> Vec<(String, bool, bool)> {
    let quotes = QuoteScan::new(text);
    let bodies = HeredocBodies::scan(text);
    let bytes = text.as_bytes();
    let mut stages = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if (quotes.balanced && !quotes.is_unquoted(i)) || bodies.suppresses_separator(i) {
            i += 1;
            continue;
        }
        let next = bytes.get(i + 1).copied();
        let (width, piped, pipe_stderr) = match (bytes[i], next) {
            (b'|', Some(b'|')) | (b'&', Some(b'&')) => (2, false, false),
            (b'|', Some(b'&')) => (2, true, true),
            (b'|', _) => (1, true, false),
            (b';' | b'\n', _) => (1, false, false),
            (b'&', _) if i > 0 && matches!(bytes[i - 1], b'>' | b'<') => (0, false, false),
            (b'&', Some(b'>')) => (0, false, false),
            (b'&', _) => (1, false, false),
            _ => (0, false, false),
        };
        if width == 0 {
            i += 1;
            continue;
        }
        stages.push((text[start..i].to_string(), piped, pipe_stderr));
        i += width;
        start = i;
    }
    stages.push((text[start..].to_string(), false, false));
    stages
}

#[cfg(test)]
#[path = "credential_print_tests.rs"]
mod credential_print_tests;
