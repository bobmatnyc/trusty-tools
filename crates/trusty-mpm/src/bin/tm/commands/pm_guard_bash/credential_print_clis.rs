//! Credential calls beyond the `security`/`gcloud` token calls, for the
//! credential-print rule (#8677).
//!
//! Why: #8673's review left three shapes open. `security -i` runs whatever
//! commands its stdin carries, so `echo 'find-generic-password -w' | security
//! -i` printed the value while no stage looked like a credential call. `curl
//! -v` echoes its request headers on stderr, so a token the command consumed
//! correctly still reached tool output. And four sibling CLIs print a secret
//! on stdout: `gcloud secrets versions access`, `gh auth token`, `op read`,
//! and `aws configure get …secret…`.
//! What: [`names_cli_trigger`] makes those commands worth parsing;
//! [`interactive_security`] finds a `security` stage in interactive mode, which
//! the caller refuses outright because the guard cannot read the commands it
//! will run; [`judge_cli_echoes`] routes every value a sibling CLI or a
//! verbose `curl` writes, to stdout, stderr or a named path.
//! Test: `credential_print_tests::denies_security_reading_commands_on_stdin_8677`,
//! `credential_print_tests::refuses_an_unclassifiable_interactive_security_8677`,
//! `credential_print_tests::denies_a_verbose_curl_carrying_a_credential_8677`,
//! `credential_print_tests::denies_the_sibling_credential_clis_8677`,
//! `credential_print_tests::allows_the_8677_neighbours`.

use super::credential_print_programs::basename;
use super::credential_print_redirect::{Routed, redirect_target_sink};
use super::{Emitted, Lifted, MARK, Refusal, Sink, carries, route};

/// Word sequences, in order, that name a sibling credential call.
const WORD_TRIGGERS: &[&[&str]] = &[
    &["gcloud", "secrets", "access"],
    &["gh", "auth", "token"],
    &["gh", "auth", "--show-token"],
    &["gh", "auth", "-t"],
    &["op", "read"],
    &["aws", "configure", "get"],
    &["aws", "configure", "export-credentials"],
];

/// curl short options that take a value: the rest of the cluster, else the
/// next word.
const CURL_SHORT_WITH_ARG: &[char] = &[
    'A', 'b', 'c', 'C', 'd', 'D', 'e', 'E', 'F', 'H', 'K', 'm', 'o', 'P', 'Q', 'r', 't', 'T', 'u',
    'U', 'w', 'x', 'X', 'y', 'Y', 'z',
];

/// Where a CLI writes a value it echoes.
#[derive(Debug, PartialEq, Eq)]
enum Echo<'a> {
    Out,
    Err,
    /// A file the CLI opens by name (`--trace FILE`, `--out-file FILE`).
    Path(&'a str),
}

/// Whether the quote-stripped, lowercased `flat` text names an interactive
/// `security` or a [`WORD_TRIGGERS`] sequence.
pub(super) fn names_cli_trigger(flat: &str) -> bool {
    let words: Vec<String> = flat
        .split(|c: char| c.is_whitespace() || ";|&()<>{}`$".contains(c))
        .filter(|w| !w.is_empty())
        .map(basename)
        .collect();
    let interactive = words
        .iter()
        .enumerate()
        .any(|(at, w)| w == "security" && security_is_interactive(&words[at + 1..]));
    interactive
        || WORD_TRIGGERS.iter().any(|seq| {
            let mut rest = words.iter();
            seq.iter().all(|want| rest.any(|w| w == want))
        })
}

/// Whether `security`'s own leading options start interactive mode, where it
/// reads commands from stdin: `-i`, or `-p prompt`, which implies it. A `-h`
/// in the same run does not clear it: the guard reads it closed.
fn security_is_interactive<S: AsRef<str>>(args: &[S]) -> bool {
    for a in args {
        let Some(cluster) = a
            .as_ref()
            .strip_prefix('-')
            .filter(|c| !c.is_empty() && !c.starts_with('-'))
        else {
            return false;
        };
        if cluster.contains(['i', 'p']) {
            return true;
        }
    }
    false
}

/// Programs that read or print a `security -i` operand and never run it.
const NON_EXECUTING_READERS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "ag", "ack", "man", "info", "apropos", "whatis", "which",
    "whereis", "echo", "printf",
];

/// Whether a stage runs `security` in interactive mode (#8677).
///
/// What: a `security` word anywhere in `argv` whose leading options are
/// interactive, since any unknown wrapper (`arch -arm64`, `script -q f`,
/// `launchctl asuser N`) can run it (#8677 review round 2). The one exception
/// is a stage whose program at `start` is in [`NON_EXECUTING_READERS`] (`grep
/// security -i notes.txt`).
/// Test: `credential_print_tests::denies_interactive_security_behind_an_unknown_wrapper_8677`,
/// `credential_print_tests::allows_the_8677_round_two_neighbours`.
pub(super) fn interactive_security(argv: &[String], start: usize) -> bool {
    let reader = argv
        .get(start)
        .is_some_and(|w| NON_EXECUTING_READERS.contains(&basename(w).as_str()));
    !reader
        && argv.iter().enumerate().any(|(at, w)| {
            basename(w) == "security"
                && security_is_interactive(argv.get(at + 1..).unwrap_or_default())
        })
}

/// Route every value a sibling credential CLI or a verbose `curl` echoes.
///
/// What: each word naming `gcloud`, `gh`, `op`, `aws` or `curl` is read as a
/// call, wherever a wrapper put it, like `first_credential_program`. A CLI's
/// [`Echo`]s route to the stage's stdout, stderr, or the sink its path names
/// ([`redirect_target_sink`]); a plain file is discarded. `curl` echoes only
/// when one of its arguments carries a credential.
pub(super) fn judge_cli_echoes(
    argv: &[String],
    routed: &Routed,
    lifted: &Lifted,
    stage_out: Sink,
    emitted: &mut Emitted,
) -> Result<(), Refusal> {
    for (at, word) in argv.iter().enumerate() {
        let args = argv.get(at + 1..).unwrap_or_default();
        // #8677 round 2: `gh auth $(echo token)` picks its subcommand at run time.
        let slots = match basename(word).as_str() {
            "gcloud" => 3,
            "gh" | "aws" => 2,
            "op" => 1,
            _ => 0,
        };
        let dynamic = args
            .iter()
            .filter(|a| !a.starts_with('-'))
            .take(slots)
            .any(|a| a.contains(['$', '`']) || a.contains(MARK));
        if dynamic {
            return Err(Refusal::Unreadable(
                "a credential CLI subcommand chosen at run time",
            ));
        }
        let echoes = match basename(word).as_str() {
            "curl" if args.iter().any(|a| carries(a, lifted)) => curl_echoes(args),
            program => cli_echoes(program, args),
        };
        for echo in echoes {
            let sink = match echo {
                Echo::Out => routed.out,
                Echo::Err => routed.err,
                Echo::Path(p) => redirect_target_sink(p, &routed.fds, lifted, stage_out),
            };
            route(sink, emitted)?;
        }
    }
    Ok(())
}

/// The values a sibling credential CLI prints.
///
/// What: `gcloud secrets … access`, `op read` and `aws configure get <key>`
/// with a key naming a secret or token, or `export-credentials`, print on
/// stdout; `gcloud`'s `--out-file` and `op`'s `--out-file`/`-o` write the
/// named path instead. `gh auth token` prints on stdout; `gh auth status` with
/// `-t`/`--show-token` is read as both descriptors, as its stream changed
/// between gh releases.
fn cli_echoes<'a>(program: &str, args: &'a [String]) -> Vec<Echo<'a>> {
    let after = |word: &str| args.iter().position(|a| a.eq_ignore_ascii_case(word));
    let follows = |first: &str, then: &[&str]| {
        after(first).is_some_and(|at| {
            args[at + 1..]
                .iter()
                .any(|a| then.iter().any(|t| a.eq_ignore_ascii_case(t)))
        })
    };
    match program {
        "gcloud" if follows("secrets", &["access"]) => out_or_file(args, &["--out-file"]),
        "op" if after("read").is_some() => out_or_file(args, &["--out-file", "-o"]),
        "gh" if follows("auth", &["token"]) => vec![Echo::Out],
        "gh" if follows("auth", &["status"]) && follows("auth", &["-t", "--show-token"]) => {
            vec![Echo::Out, Echo::Err]
        }
        "aws" if follows("configure", &["export-credentials"]) => vec![Echo::Out],
        "aws" if follows("configure", &["get"]) => {
            let secret = args.iter().any(|a| {
                let a = a.to_ascii_lowercase();
                a.contains("secret") || a.contains("token")
            });
            if secret { vec![Echo::Out] } else { Vec::new() }
        }
        _ => Vec::new(),
    }
}

/// Stdout, or the path an out-file flag in `flags` names (`--flag=PATH`,
/// `--flag PATH`); `-` is stdout.
fn out_or_file<'a>(args: &'a [String], flags: &[&str]) -> Vec<Echo<'a>> {
    let path = args.iter().enumerate().find_map(|(n, a)| {
        flags.iter().find_map(|f| {
            a.strip_prefix(f)
                .and_then(|rest| rest.strip_prefix('='))
                .or_else(|| (a == f).then(|| args.get(n + 1).map_or("-", String::as_str)))
        })
    });
    match path {
        None | Some("-") => vec![Echo::Out],
        Some(p) => vec![Echo::Path(p)],
    }
}

/// Where `curl` echoes its request, headers and `-u` credentials included.
///
/// What: `-v`/`--verbose` and `--trace-config` write to curl's stderr, which
/// `--stderr -` moves to stdout and `--stderr FILE` to a file; `--trace` and
/// `--trace-ascii` write their value (`-` stdout, `%` curl's stderr, else a
/// file); `--libcurl` writes source naming every option to its value. Short
/// options are read getopt-style, so `-d -v` sends the data `-v`.
fn curl_echoes(args: &[String]) -> Vec<Echo<'_>> {
    let mut stderr_to: Option<&str> = None;
    let (mut verbose, mut named) = (false, Vec::new());
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        let value = |i: &mut usize| {
            *i += 1;
            args.get(*i - 1).map_or("-", String::as_str)
        };
        match a.as_str() {
            "--" => break,
            "--verbose" | "--trace-config" => verbose = true,
            "--no-verbose" => verbose = false,
            "--stderr" => stderr_to = Some(value(&mut i)),
            "--trace" | "--trace-ascii" | "--libcurl" => named.push(value(&mut i)),
            long if long.starts_with("--") => {}
            short => {
                let cluster = short.strip_prefix('-').unwrap_or_default();
                for (at, ch) in cluster.char_indices() {
                    if ch == 'v' {
                        verbose = true;
                    }
                    if CURL_SHORT_WITH_ARG.contains(&ch) {
                        i += usize::from(at + ch.len_utf8() == cluster.len());
                        break;
                    }
                }
            }
        }
    }
    let to_stderr = || match stderr_to {
        None => Echo::Err,
        Some("-") => Echo::Out,
        Some(p) => Echo::Path(p),
    };
    let mut echoes: Vec<Echo<'_>> = named
        .into_iter()
        .map(|v| match v {
            "-" => Echo::Out,
            "%" => to_stderr(),
            p => Echo::Path(p),
        })
        .collect();
    if verbose {
        echoes.push(to_stderr());
    }
    echoes
}
