//! Refuse a Bash command that would print a credential VALUE into tool output
//! (#8596, #8248).
//!
//! Why: a Bash tool result is transcript text. `security find-generic-password
//! -s svc -w | head -c 50` (#8596) and a bare `gcloud auth print-access-token`
//! (#8248) each put a live credential there, and no rule looked at them: the
//! secret-file rule (#7266) screens file NAMES, and these commands name no
//! file. Agent prose alone did not stop either leak.
//! What: [`evaluate_credential_print_command`] finds every credential-printing
//! call — `security find-{generic,internet}-password` with `-w` (value on
//! stdout) or `-g` (value on stderr), and `gcloud auth [application-default]
//! print-{access,identity}-token` — and follows the descriptor carrying the
//! value through redirections, pipes and command substitutions. The command is
//! allowed when every such value ends in a file, `/dev/null`, a variable
//! assignment, an argument to a non-printing program, or a non-printing stdin
//! consumer ([`STDIN_CONSUMERS`], `--password-stdin`, `--with-token`, `grep
//! -q`). It is refused when a value reaches the terminal, a printing filter
//! (`head`, `cut`, `tee`, any program not known to swallow its input), an
//! `echo`/`printf`/`cat` argument, or the command-name position.
//!
//! Fail-closed: once the quote-stripped text names a credential subcommand,
//! anything the scanner cannot read — unbalanced quoting, an unclosed
//! substitution, nesting past [`MAX_DEPTH`], an unlexable `sh -c` string —
//! denies. A command naming none of [`TRIGGERS`] is never parsed at all.
//! Test: `credential_print_tests` (sibling module).

use super::bash_tokens::{RedirectRole, redirect_role, tokenize};
use super::heredoc::HeredocBodies;
use super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use crate::commands::hook_rewrite::strip_wrapper_prefix;

/// Subcommand names whose presence makes the command worth parsing.
const TRIGGERS: &[&str] = &[
    "find-generic-password",
    "find-internet-password",
    "print-access-token",
    "print-identity-token",
];

/// `security find-*-password` options that take a value (BSD getopt).
const SECURITY_OPTS_WITH_ARG: &[char] = &[
    'a', 'c', 'C', 'd', 'D', 'G', 'j', 'l', 'p', 'P', 'r', 's', 't',
];

/// Programs that print their arguments (or the files they name) to stdout.
const ARG_PRINTERS: &[&str] = &["echo", "printf", "print", "cat"];

/// Programs that read stdin and print none of it.
const STDIN_CONSUMERS: &[&str] = &["wc", "pbcopy"];

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

/// A lifted substitution: its opener and whether its value carries a credential.
#[derive(Clone, Copy)]
struct Sub {
    output_process: bool,
    yields: bool,
}

/// Refuse a Bash command that prints a credential value: `Some(reason)` denies.
///
/// Why: see the module doc — the value must never reach tool output, and
/// `pm_guard` is the one place that sees the command before it runs.
/// What: a cheap trigger test over the quote-stripped text, then [`scan`] at
/// the top level, where stdout and stderr are both tool output.
/// Test: `credential_print_tests::denies_the_reported_leaks`,
/// `credential_print_tests::allows_the_capturing_forms`,
/// `credential_print_tests::denies_what_it_cannot_read`.
pub(crate) fn evaluate_credential_print_command(command: &str) -> Option<String> {
    let flat: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    if !TRIGGERS.iter().any(|t| flat.contains(t)) {
        return None;
    }
    let why = match scan(command, Sink::Terminal, Sink::Terminal, 0) {
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
     consume a token inside the command that needs it \
     (`curl -H \"Authorization: Bearer $(gcloud auth print-access-token)\" …`, or \
     `TOKEN=$(…)` without echoing it).";

/// Scan `text` run with `stdout`/`stderr` as its sinks; `Ok(true)` when a
/// credential value flows into a [`Sink::Captured`] stdout.
///
/// What: lifts every substitution ([`lift_substitutions`]), splits the rest
/// into stages ([`split_stages`]) and judges each ([`judge_stage`]), carrying
/// "stdin holds a credential" from a stage piped into the next.
fn scan(text: &str, stdout: Sink, stderr: Sink, depth: usize) -> Result<bool, Refusal> {
    if depth > MAX_DEPTH {
        return Err(Refusal::Unreadable("nesting this deep"));
    }
    let (flat, subs) = lift_substitutions(text, stdout, stderr, depth)?;
    let stages = split_stages(&flat);
    let mut yields = false;
    let mut stdin_carries = false;
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
            depth,
        };
        let emitted = judge_stage(stage.trim(), &subs, ctx)?;
        yields |= emitted.captured;
        stdin_carries = emitted.piped;
    }
    Ok(yields)
}

/// The sinks a stage starts with, before its own redirections.
#[derive(Clone, Copy)]
struct StageCtx {
    out: Sink,
    err: Sink,
    stdin_carries: bool,
    depth: usize,
}

/// Where a stage's credential value went.
#[derive(Default)]
struct Emitted {
    captured: bool,
    piped: bool,
}

/// Judge one pipeline stage.
///
/// What: resolves the stage's redirections, descends into a `sh -c`-style
/// wrapper, then decides which descriptors carry a credential: the ones a
/// [`credential_fds`] call prints, stdout of an [`ARG_PRINTERS`] call given a
/// credential argument, and stdout of any stage whose stdin carries one unless
/// it is a non-printing consumer. Each carrying descriptor is routed by
/// [`route`].
fn judge_stage(stage: &str, subs: &[Sub], ctx: StageCtx) -> Result<Emitted, Refusal> {
    let mut emitted = Emitted::default();
    if stage.is_empty() {
        return Ok(emitted);
    }
    let tokens = tokenize(stage).map_err(|_| Refusal::Unreadable("its quoting"))?;
    let (argv, out, err, here_carries) = apply_redirections(&tokens, ctx.out, ctx.err, subs)?;
    let stdin_carries = ctx.stdin_carries || here_carries;
    match wrapped_command(stage) {
        WrappedCommand::Unlexable => return Err(Refusal::Unreadable("its wrapped command")),
        WrappedCommand::Inner(inner) => {
            let inner_err = if err == Sink::Terminal {
                Sink::Terminal
            } else {
                Sink::Discarded
            };
            // A wrapper reading a credential on stdin (`| xargs echo`) may
            // print it, so stdin carry routes too.
            if scan(&inner, Sink::Captured, inner_err, ctx.depth + 1)? || stdin_carries {
                route(out, &mut emitted)?;
            }
            return Ok(emitted);
        }
        WrappedCommand::None => {}
    }
    // A wrapper with a flag (`nice -n 5 head`) resolves to no program; read
    // its first word, which is never a known non-printing consumer.
    let start = strip_wrapper_prefix(&argv).unwrap_or(0);
    let Some(program_word) = argv.get(start) else {
        // Assignments only (`T=$(…)`): nothing is printed.
        return Ok(emitted);
    };
    if carries(program_word, subs) {
        // `$(print-access-token)` runs the value; the shell prints it in its
        // "command not found" error.
        return Err(Refusal::Prints);
    }
    let program = basename(program_word);
    let args = &argv[start + 1..];
    // Found anywhere, not only at `start`: `timeout 5 gcloud …` resolves its
    // program to `5`.
    let (mut fd1, fd2) = first_credential_program(&argv).map_or((false, false), |at| {
        credential_fds(basename(&argv[at]), &argv[at + 1..])
    });
    if ARG_PRINTERS.contains(&program) && args.iter().any(|a| carries(a, subs)) {
        fd1 = true;
    }
    if stdin_carries && !consumes_stdin(program, args) {
        fd1 = true;
    }
    if fd1 {
        route(out, &mut emitted)?;
    }
    if fd2 {
        route(err, &mut emitted)?;
    }
    Ok(emitted)
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

/// Which descriptors a credential CLI call prints the value on: `(stdout, stderr)`.
///
/// What: `security find-generic-password`/`find-internet-password` reads its
/// option clusters getopt-style — `-w` puts the value on stdout, `-g` on
/// stderr, and a value-taking option ends the cluster. `gcloud` prints on
/// stdout when `auth` precedes `print-access-token`/`print-identity-token`.
fn credential_fds(program: &str, args: &[String]) -> (bool, bool) {
    match program {
        "security" => {
            let Some(sub) = args
                .iter()
                .position(|a| a == "find-generic-password" || a == "find-internet-password")
            else {
                return (false, false);
            };
            let (mut w, mut g) = (false, false);
            let mut rest = args[sub + 1..].iter();
            while let Some(tok) = rest.next() {
                let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
                    continue;
                };
                for (i, ch) in cluster.char_indices() {
                    match ch {
                        'w' => w = true,
                        'g' => g = true,
                        c if SECURITY_OPTS_WITH_ARG.contains(&c) => {
                            if i + c.len_utf8() == cluster.len() {
                                rest.next();
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
            (w, g)
        }
        "gcloud" => {
            let prints = args.iter().position(|a| a == "auth").is_some_and(|at| {
                args[at + 1..]
                    .iter()
                    .any(|a| a == "print-access-token" || a == "print-identity-token")
            });
            (prints, false)
        }
        _ => (false, false),
    }
}

/// The first word naming a credential CLI, wherever a wrapper put it
/// (`timeout 5 gcloud …`, `sudo -u x security …`) — the conservative reading.
fn first_credential_program(argv: &[String]) -> Option<usize> {
    argv.iter()
        .position(|w| matches!(basename(w), "security" | "gcloud"))
}

/// Whether a program reading a credential on stdin prints none of it.
fn consumes_stdin(program: &str, args: &[String]) -> bool {
    STDIN_CONSUMERS.contains(&program)
        || args
            .iter()
            .any(|a| a == "--password-stdin" || a == "--with-token")
        || (matches!(program, "grep" | "egrep" | "fgrep" | "rg")
            && args.iter().any(|a| {
                a == "--quiet"
                    || a == "--silent"
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('q'))
            }))
}

/// The basename of a program word.
fn basename(word: &str) -> &str {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word)
}

/// Whether `word` holds a lifted substitution whose value is a credential.
fn carries(word: &str, subs: &[Sub]) -> bool {
    marks_in(word).any(|i| subs.get(i).is_some_and(|s| s.yields))
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

/// Split a stage's words into argv and its output routing.
///
/// What: applies each output redirection in order — `>f`, `2>f`, `&>f` send a
/// descriptor to [`Sink::Discarded`] (or back to a live sink for
/// `/dev/stdout`, `/dev/stderr`, `/dev/tty`, or a `>(…)` target); `N>&M`
/// copies `M`'s current sink; `N>&-` closes. A `<<<` word carrying a
/// credential makes stdin carry one. Returns `(argv, stdout, stderr,
/// stdin_carries)`.
fn apply_redirections(
    tokens: &[String],
    mut out: Sink,
    mut err: Sink,
    subs: &[Sub],
) -> Result<(Vec<String>, Sink, Sink, bool), Refusal> {
    let mut argv = Vec::new();
    let mut here_carries = false;
    let mut i = 0;
    while let Some(tok) = tokens.get(i) {
        i += 1;
        if let Some(word) = tok.strip_prefix("<<<") {
            let word = if word.is_empty() {
                i += 1;
                tokens.get(i - 1).map(String::as_str).unwrap_or_default()
            } else {
                word
            };
            here_carries |= carries(word, subs);
            continue;
        }
        let target = match redirect_role(tok) {
            RedirectRole::None => {
                argv.push(tok.clone());
                continue;
            }
            RedirectRole::TargetFollows => {
                i += 1;
                tokens
                    .get(i - 1)
                    .ok_or(Refusal::Unreadable("a redirection with no target"))?
                    .as_str()
            }
            RedirectRole::Target(t) => t,
            RedirectRole::FileDescriptor => {
                let (fd, dup) = tok.split_once(">&").unwrap_or((tok, "-"));
                let sink = match dup {
                    "1" => out,
                    "2" => err,
                    _ => Sink::Discarded,
                };
                match fd {
                    "" | "1" => out = sink,
                    "2" => err = sink,
                    _ => {}
                }
                continue;
            }
        };
        let fd = tok.split('>').next().unwrap_or_default();
        let sink = match target {
            "/dev/stdout" | "/dev/fd/1" => out,
            "/dev/stderr" | "/dev/fd/2" => err,
            "/dev/tty" => Sink::Terminal,
            t if marks_in(t).any(|m| subs.get(m).is_some_and(|s| s.output_process)) => {
                Sink::Terminal
            }
            _ => Sink::Discarded,
        };
        match fd {
            "" | "1" => out = sink,
            "2" => err = sink,
            "&" => (out, err) = (sink, sink),
            _ => {}
        }
    }
    Ok((argv, out, err, here_carries))
}

/// Replace every live substitution in `text` with a [`MARK`] placeholder,
/// scanning each body first.
///
/// What: the same opener/closer scan as `super::classify_command_substitutions`
/// — `$(…)`/`<(…)` with paren counting, backtick pairs — on a [`QuoteScan`]
/// of `text`. `$(…)`, backtick and `<(…)` bodies run with a captured stdout;
/// a `>(…)` body writes to the outer stdout. An unclosed opener is refused.
fn lift_substitutions(
    text: &str,
    stdout: Sink,
    stderr: Sink,
    depth: usize,
) -> Result<(String, Vec<Sub>), Refusal> {
    let scan_quotes = QuoteScan::new(text);
    let live = |i: usize| !scan_quotes.balanced || scan_quotes.allows_substitution(i);
    let unquoted = |i: usize| !scan_quotes.balanced || scan_quotes.is_unquoted(i);
    let bytes = text.as_bytes();
    let mut flat = String::with_capacity(text.len());
    let mut subs = Vec::new();
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
        let output_process = paren && bytes[i] == b'>';
        let body_out = if output_process {
            stdout
        } else {
            Sink::Captured
        };
        let yields = scan(body, body_out, stderr, depth + 1)?;
        flat.push_str(&text[copied..i]);
        flat.push_str(&format!("{MARK}{}__", subs.len()));
        subs.push(Sub {
            output_process,
            yields,
        });
        i = end;
        copied = end;
    }
    flat.push_str(&text[copied..]);
    Ok((flat, subs))
}

/// Split `text` into stages: `(stage, stdout piped on, stderr piped on)`.
///
/// What: cuts at unquoted `|`, `|&`, `||`, `&&`, `;`, newline and a bare `&`
/// (never at a `>&`/`&>` redirection), leaving here-document bodies whole the
/// way `super::split_shell_segments_raw` does.
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
            (b'&', _) if i > 0 && bytes[i - 1] == b'>' => (0, false, false),
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
