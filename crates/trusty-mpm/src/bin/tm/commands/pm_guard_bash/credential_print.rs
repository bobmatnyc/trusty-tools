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
//! A variable bound from a credential carries it into every stage of the
//! command (#8676, `credential_print_taint`). While one does, any inline code
//! handed to an evaluator refuses, as it can read the environment with no `$`.
//! #8677 (`credential_print_clis`): `security -i` refuses outright, a verbose
//! `curl` given a credential routes its echo, four sibling CLIs print like
//! `gcloud auth print-access-token`, and a redirect target chosen at run time
//! refuses once a value reaches it.
//!
//! Fail-closed: once the text names a credential subcommand, anything the
//! scanner cannot read — broken quoting, an unclosed substitution, nesting past
//! [`MAX_DEPTH`], an unlexable wrapper string, a descriptor chosen at run
//! time, a panic — denies. A command naming none of [`TRIGGERS`], no
//! interactive `security` and no sibling credential call (#8677) is never
//! parsed at all.
//!
//! Documented residual classes (#8676 exit criterion): each can print a value
//! and is allowed.
//! 1. Environment reads by arbitrary programs: a child reads an exported
//!    tainted variable without naming it on the command line. No argv-level
//!    guard can see this.
//! 2. Unlisted external programs given a tainted argument: the program echoes
//!    or logs its operand (`ls "$T"`, `timeout 5 sleep "$T"`, `[ x "$T" y ]`,
//!    a shell function or alias), or takes the value through its own flags
//!    (`awk -v t=…`, `jq --arg t …`). Only [`ARG_PRINTERS`] and the echoing
//!    builtins (`credential_print_taint_sinks`) are known to print their
//!    arguments. The #8697 follow-up replaces this class with a consumer
//!    allowlist.
//! 3. Script files run by name (`bash deploy.sh`): the guard does not read
//!    the script.
//! 4. Shell history: `history -s …; history`, and `fc`, where history is on.
//! 5. Descriptors read across stages: a descriptor opened in one stage and
//!    read in a later one (`exec N< <(…)`). The `coproc` form refuses outright
//!    while any name is tainted (#8676 rounds 4-5); the general form stays
//!    residual.
//! 6. #8677: a credential named in no command text — an exported
//!    `$GITHUB_TOKEN` under `curl -v`, a sibling CLI run by a `$`-named
//!    program — and echo flags beyond `curl`'s (`wget -d`, `http -v`), or
//!    set in a config file (`~/.curlrc`).
//!
//! Why a denylist and not an allowlist: see #8676 round 3.
//! Test: `credential_print_tests` (sibling module).

// #8677: `security -i`, verbose `curl`, and the sibling secret-printing CLIs.
#[path = "credential_print_clis.rs"]
mod credential_print_clis;
#[path = "credential_print_heredoc.rs"]
mod credential_print_heredoc;
#[path = "credential_print_programs.rs"]
mod credential_print_programs;
#[path = "credential_print_redirect.rs"]
mod credential_print_redirect;
#[path = "credential_print_split.rs"]
mod credential_print_split;
#[path = "credential_print_taint.rs"]
mod credential_print_taint;
#[path = "credential_print_taint_forms.rs"]
mod credential_print_taint_forms;
#[path = "credential_print_taint_sinks.rs"]
mod credential_print_taint_sinks;

use super::bash_tokens::tokenize;
use super::shell_lex::{WrappedCommand, wrapped_command};
use crate::commands::hook_rewrite::strip_wrapper_prefix;
use credential_print_clis::{interactive_security, judge_cli_echoes, names_cli_trigger};
use credential_print_heredoc::strip_comments_and_heredocs;
use credential_print_programs::{
    basename, code_operands, consumes_stdin, credential_fds, enables_xtrace, evaluator_name,
    first_credential_program, keyword_words,
};
// #8756: re-exported for `substitutions`, which asks which bodies run as code.
pub(super) use credential_print_programs::is_evaluator;
use credential_print_redirect::{apply_redirections, note_temp_files, terminal_name_sink};
use credential_print_split::{lift_substitutions, split_stages, ungroup};
use credential_print_taint::{bound_names, dumps_variables, expands_tainted};
use credential_print_taint_forms::{array_bindings, function_header_words, reads_in_arithmetic};
use credential_print_taint_sinks::{
    judge_builtin_sinks, reads_script_by_path, reports_unset_error,
};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

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

/// Work units one whole scan may spend before refusing (#8676): a pass costs
/// one unit plus one per [`BYTES_PER_UNIT`] of its text.
const WORK_BUDGET: usize = 4_000;

/// Text bytes one work unit covers.
const BYTES_PER_UNIT: usize = 1_024;

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
    /// #8677: a target the shell picks at run time (`> "$OUT"`); a value
    /// routed here refuses as unreadable.
    Unknown,
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
    /// #8677: the body is a lone `mktemp` call, whose value names a file.
    mktemp: bool,
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
    /// #8676: shell variables that hold a credential.
    names: BTreeSet<String>,
    /// #8676: work units spent so far, shared by every clone in one scan.
    spent: Rc<Cell<usize>>,
    /// #8677: names an earlier stage bound to `$(mktemp)`, so `> "$tmp"`
    /// names a file.
    temp_files: BTreeSet<String>,
}

/// Refuse a Bash command that prints a credential value: `Some(reason)` denies.
///
/// Why: see the module doc — the value must never reach tool output, and
/// `pm_guard` is the one place that sees the command before it runs.
/// What: a cheap case-insensitive trigger test over the quote-stripped text,
/// then [`scan`] at the top level, where stdout and stderr are both tool
/// output. A panic inside the scan is caught and refuses, so it can never exit
/// the hook 101 and fail open; recursion is depth-bounded and the whole scan
/// shares one work budget, so neither a stack overflow nor a slow scan can.
/// Test: `credential_print_tests::denies_the_reported_leaks`,
/// `credential_print_tests::allows_the_capturing_forms`,
/// `credential_print_tests::denies_what_it_cannot_read`,
/// `credential_print_tests::denies_the_round_two_bypasses`,
/// `credential_print_tests::denies_a_credential_carried_by_a_variable`,
/// `credential_print_tests::denies_inline_code_reading_a_tainted_name`,
/// `credential_print_tests::denies_a_coproc_that_carries`,
/// `credential_print_tests::allows_the_round_four_neighbours`,
/// `credential_print_tests::denies_the_round_five_bypasses`,
/// `credential_print_tests::allows_the_round_five_neighbours`,
/// `credential_print_tests::scan_work_is_bounded`,
/// `credential_print_tests::deny_reason_never_echoes_the_command`,
/// `credential_print_tests::no_prefix_of_a_command_panics`,
/// `credential_print_tests::denies_security_reading_commands_on_stdin_8677`,
/// `credential_print_tests::refuses_an_unclassifiable_interactive_security_8677`,
/// `credential_print_tests::denies_a_verbose_curl_carrying_a_credential_8677`,
/// `credential_print_tests::denies_a_credential_redirected_to_an_unread_target_8677`,
/// `credential_print_tests::denies_the_sibling_credential_clis_8677`,
/// `credential_print_tests::allows_the_8677_neighbours`.
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

/// Whether `text`, quotes removed and lowercased, names a [`TRIGGERS`] entry,
/// an interactive `security`, or a sibling credential call (#8677).
fn has_trigger(text: &str) -> bool {
    let flat: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect::<String>()
        .to_ascii_lowercase();
    TRIGGERS.iter().any(|t| flat.contains(t)) || names_cli_trigger(&flat)
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
/// #8676: a name a stage binds is seen by every later stage of the same pass;
/// a pass that binds a new name is re-run with it, so every earlier stage and
/// substitution body sees it too; the set only grows. The passes of the whole
/// scan share [`WORK_BUDGET`], and exhausting it refuses as unreadable.
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
    let mut names = outer.names.clone();
    loop {
        let mut lifted = outer.clone();
        lifted.names.clone_from(&names);
        let (yields, bound) = scan_pass(text, stdout, stderr, depth, &mut lifted)?;
        let known = names.len();
        names.extend(bound);
        if names.len() == known {
            return Ok(yields);
        }
    }
}

/// One [`scan`] pass with a fixed name set: whether a value is captured, and
/// every name a stage bound.
fn scan_pass(
    text: &str,
    stdout: Sink,
    stderr: Sink,
    depth: usize,
    lifted: &mut Lifted,
) -> Result<(bool, Vec<String>), Refusal> {
    let text = strip_comments_and_heredocs(text, &mut lifted.heredocs);
    let flat = lift_substitutions(&text, stdout, stderr, depth, lifted)?;
    let stages = split_stages(&flat);
    let cost = 1 + text.len() / BYTES_PER_UNIT;
    lifted.spent.set(lifted.spent.get() + cost);
    if lifted.spent.get() > WORK_BUDGET {
        return Err(Refusal::Unreadable("a command this costly to follow"));
    }
    let (mut yields, mut bound) = (false, Vec::new());
    let (mut stdin_carries, mut stdin_text) = (false, false);
    for (idx, (stage, piped, pipe_stderr)) in stages.iter().enumerate() {
        // #8677: `tmp=$(mktemp)` names a file for every later stage.
        note_temp_files(stage, lifted);
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
            stdin_piped: idx > 0 && stages[idx - 1].1,
            depth,
        };
        let emitted = judge_stage(stage.trim(), lifted, ctx)?;
        yields |= emitted.captured;
        stdin_carries = emitted.piped;
        stdin_text = *piped && emitted.text;
        for name in emitted.bound {
            if lifted.names.insert(name.clone()) {
                bound.push(name);
            }
        }
    }
    Ok((yields, bound))
}

/// The sinks a stage starts with, before its own redirections.
#[derive(Clone, Copy)]
struct StageCtx {
    out: Sink,
    err: Sink,
    stdin_carries: bool,
    /// The previous stage piped program text in (`echo '…' | sh`).
    stdin_text: bool,
    /// #8676: the previous stage piped anything in.
    stdin_piped: bool,
    depth: usize,
}

/// Where a stage's credential value went, and whether it wrote program text.
#[derive(Default)]
struct Emitted {
    captured: bool,
    piped: bool,
    text: bool,
    /// #8676: names this stage bound from a credential.
    bound: Vec<String>,
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
    // #8676: forms `ungroup` erases, read from the text as written.
    let arrays = array_bindings(stage, lifted)?;
    let header = function_header_words(stage);
    let raw = stage;
    let stage = ungroup(stage);
    let tokens = tokenize(&stage).map_err(|_| Refusal::Unreadable("its quoting"))?;
    let routed = apply_redirections(&tokens, ctx.out, ctx.err, lifted)?;
    // #8676 round 5: `wc -c < "$T"` — a missing file's "No such file" error
    // names the carrying target on stderr.
    if routed.target_carries {
        route(routed.err, &mut emitted)?;
    }
    let argv = &routed.argv;
    let reads_input = argv
        .iter()
        .any(|w| carries_kind(w, lifted, Some(SubKind::Input)));
    let stdin_carries = ctx.stdin_carries || routed.here_carries || reads_input;
    emitted.text = argv.iter().any(|w| input_is_program_text(w)) || routed.here_program_text;
    let (keywords, coproc) = keyword_words(argv, header);
    let kw = header + keywords;
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
    // #8676: bind before any early return; `printenv T`/`env`/`set` print them.
    emitted.bound = bound_names(argv, kw, &program, args, lifted);
    emitted.bound.extend(arrays);
    if reads_in_arithmetic(raw, argv, &program, &lifted.names) {
        return Err(Refusal::Prints);
    }
    // #8676 round 4/5: a coproc's output is a descriptor a later stage reads,
    // and `coproc NAME ( … )` hides its program, so `ungroup` can leave a bare
    // NAME standing in for a declarer, `set`, or an evaluator this scan never
    // re-parses (`coproc X ( declare -p T >&2 )`). While any name is tainted,
    // refuse every coproc stage outright, not only one with a word that
    // itself carries the value.
    if coproc && (!lifted.names.is_empty() || argv.iter().any(|w| carries(w, lifted))) {
        return Err(Refusal::Prints);
    }
    if dumps_variables(argv, &program, args, &lifted.names) {
        route(routed.out, &mut emitted)?;
    }
    // #8676 round 3: `cd "$T"`, `export "$T"`, `trap 'echo $T' EXIT`.
    let sinks = (routed.out, routed.err);
    judge_builtin_sinks(&program, args, sinks, lifted, ctx.depth, &mut emitted)?;
    // #8676 round 5: `select NAME in WORD...` lists every WORD on stderr.
    if program == "select" && select_items_carry(argv, lifted) {
        route(routed.err, &mut emitted)?;
    }
    // #8676 round 5: `${NAME?word}`/`${NAME:?word}` aborts and writes a
    // carrying `word` to stderr at expansion time, before any program runs —
    // this must precede the empty check below, which is why it runs here.
    if reports_unset_error(argv, &lifted.names) {
        route(routed.err, &mut emitted)?;
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
    // #8677: `security -i` runs whatever commands its stdin carries.
    if interactive_security(argv, kw, start) {
        return Err(Refusal::Unreadable(
            "the commands `security -i` reads from its input",
        ));
    }
    // #8756: `wrapped_command` now unwraps `eval`; this rule keeps reading it
    // as an evaluator, whose operands are program text.
    let wrapped = if program == "eval" {
        WrappedCommand::None
    } else {
        wrapped_command(&stage)
    };
    // A wrapper with flags (`sudo -u x bash -c …`) hides its program, so the
    // first evaluator word after it stands in.
    let evaluator_at = if resolved.is_some() {
        is_evaluator(&program).then_some(start)
    } else {
        argv.iter()
            .skip(start)
            .position(|w| is_evaluator(&basename(w)))
            .map(|p| p + start)
    };
    if let Some(at) = evaluator_at.filter(|&at| at != start || wrapped == WrappedCommand::None) {
        let word = basename(&argv[at]);
        // #8676 round 3: `python3.12` runs as `python`.
        let evaluator = evaluator_name(&word).unwrap_or(&word);
        let operands = argv.get(at + 1..).unwrap_or_default();
        let code = code_operands(evaluator, operands);
        // #8676: with a credential in a variable, any inline code can read it
        // with no `$` (`os.environ`, `ENV`, `process.env`), so all refuse.
        let tainted = !lifted.names.is_empty();
        let reads_stdin = operands.iter().all(|a| a.starts_with('-'));
        // #8596 round 3: only code operands; `python3 up.py --token "$T"` is data.
        let fed = matches!(evaluator, "eval" | "source" | ".")
            || code.iter().any(|c| input_is_program_text(c))
            || (tainted && (!code.is_empty() || routed.here_any))
            || (tainted && reads_stdin && ctx.stdin_piped)
            // #8676 round 3: `bash /dev/stdin`, `bash <(…)`, `bash < <(…)`.
            || (tainted && reads_script_by_path(operands, lifted))
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
            // #8677: stderr piped or captured (`sh -c '… -g' 2>&1 | cat`) is
            // followed as a capture, then routed with stdout below.
            let err_carries = matches!(err, Sink::Pipe | Sink::Captured);
            let inner_err = match err {
                Sink::Terminal | Sink::Unknown => err,
                Sink::Pipe | Sink::Captured => Sink::Captured,
                Sink::Discarded => Sink::Discarded,
            };
            let prints = if program == "xargs" && stdin_carries {
                // #8596 finding 10: xargs hands stdin to its program as
                // arguments; only a printing program leaks them.
                let mut with_value = lifted.clone();
                let mark = format!("{MARK}{}__", with_value.subs.len());
                with_value.subs.push(Sub {
                    kind: SubKind::Command,
                    yields: true,
                    mktemp: false,
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
                if err_carries {
                    route(err, &mut emitted)?;
                }
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
    // #8677: sibling credential CLIs and a verbose `curl`.
    judge_cli_echoes(argv, &routed, lifted, ctx.out, &mut emitted)?;
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

/// Record where one credential-carrying descriptor lands, refusing the terminal.
fn route(sink: Sink, emitted: &mut Emitted) -> Result<(), Refusal> {
    match sink {
        Sink::Terminal => return Err(Refusal::Prints),
        Sink::Captured => emitted.captured = true,
        Sink::Pipe => emitted.piped = true,
        Sink::Discarded => {}
        // #8677: the target may be the terminal.
        Sink::Unknown => return Err(Refusal::Unreadable("an output target chosen at run time")),
    }
    Ok(())
}

/// Whether `word` holds a lifted substitution whose value is a credential, or
/// expands a variable that holds one (#8676).
fn carries(word: &str, lifted: &Lifted) -> bool {
    carries_kind(word, lifted, None) || expands_tainted(word, &lifted.names)
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

/// Whether a `select NAME in ITEM...` stage lists a carrying item (#8676
/// round 5): bash writes the numbered menu, one line per item, to stderr, and
/// no reader argument names it, so nothing else in [`judge_stage`] sees it.
fn select_items_carry(argv: &[String], lifted: &Lifted) -> bool {
    argv.iter().position(|w| w == "in").is_some_and(|at| {
        argv.get(at + 1..)
            .unwrap_or_default()
            .iter()
            .any(|w| carries(w, lifted))
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

#[cfg(test)]
#[path = "credential_print_tests.rs"]
mod credential_print_tests;
