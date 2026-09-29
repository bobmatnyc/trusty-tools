//! Which argument printers repeat a credential operand on stderr, for the
//! credential-print rule (#8735).
//!
//! Why: `wc -c < <(cat "$T")` routed only `cat`'s stdout, but `cat` given a
//! value as a filename fails with `cat: <value>: No such file`, written to the
//! stderr the `<(…)` body shares with the outer command. `printf` does the
//! same for an operand a numeric conversion rejects (`printf: <value>: invalid
//! number`), and zsh `print -f` likewise. zsh `print -u N` writes the value
//! itself to descriptor N.
//! What: [`reports_operand_on_stderr`] says whether a `super::ARG_PRINTERS`
//! call given a carrying operand may write that operand to stderr. `echo`
//! never does, and `cat` only for an operand that holds the value, not a
//! `<(…)` file it names by path. A `printf` format chosen at run time, or a
//! `print` flag this module does not know, counts as reporting: the guard
//! cannot read it. [`route_print_unit`] routes the descriptor `print -u N`
//! writes to, and refuses a run-time `N` or the `-p` coprocess.
//! Test: `credential_print_tests::denies_a_credential_operand_reported_on_stderr_8735`,
//! `credential_print_tests::denies_an_unreadable_stderr_of_a_reporting_printer_8735`,
//! `credential_print_tests::denies_a_credential_written_by_print_u_8735`,
//! `credential_print_tests::denies_an_unreadable_print_unit_8735`,
//! `credential_print_tests::allows_the_8735_neighbours`.

use super::credential_print_taint::expands_tainted;
use super::{Emitted, Lifted, MARK, Refusal, Sink, SubKind, carries, carries_kind, route};

/// zsh `print` flags that take no operand and report no error.
const PRINT_QUIET_FLAGS: &str = "rnlNcaoOiRPEebD";

/// `printf` conversions that copy an operand as text and never reject it.
const TEXT_CONVERSIONS: &str = "sbqc";

/// Whether `program`, given a carrying operand in `args`, may write it to
/// stderr in an error message.
pub(super) fn reports_operand_on_stderr(program: &str, args: &[String], lifted: &Lifted) -> bool {
    match program {
        // #8735 round 1: `cat <(…)` names `/dev/fd/63`, never the value.
        "cat" => args.iter().any(|a| {
            expands_tainted(a, &lifted.names) || carries_kind(a, lifted, Some(SubKind::Command))
        }),
        "printf" => printf_reports(args, lifted),
        "print" => print_reports(args, lifted),
        _ => false,
    }
}

/// `printf [-v NAME] [--] FORMAT …`: a carrying `-v` name is "not a valid
/// identifier"; otherwise the format decides.
fn printf_reports(args: &[String], lifted: &Lifted) -> bool {
    let mut rest = args;
    while let Some(word) = rest.first().map(String::as_str) {
        let name = match word {
            "--" => {
                rest = rest.get(1..).unwrap_or_default();
                break;
            }
            "-v" => {
                let name = rest.get(1).map(String::as_str).unwrap_or_default();
                rest = rest.get(2..).unwrap_or_default();
                name
            }
            _ => match word.strip_prefix("-v") {
                Some(name) => {
                    rest = rest.get(1..).unwrap_or_default();
                    name
                }
                None => break,
            },
        };
        if carries(name, lifted) {
            return true;
        }
    }
    rest.first().is_some_and(|f| format_reports(f, lifted))
}

/// zsh `print` flags that take an operand, attached (`-u3`) or next (`-u 3`).
const PRINT_OPERAND_FLAGS: &str = "fuCxXv";

/// zsh `print`'s option flags, each with its operand, up to the first
/// non-option word, `-` or `--`.
fn print_flags(args: &[String]) -> Vec<(char, Option<&str>)> {
    let mut flags = Vec::new();
    let mut n = 0;
    while let Some(word) = args.get(n) {
        n += 1;
        let Some(cluster) = word
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && *f != "-")
        else {
            break;
        };
        for (at, flag) in cluster.char_indices() {
            if !PRINT_OPERAND_FLAGS.contains(flag) {
                flags.push((flag, None));
                continue;
            }
            let attached = &cluster[at + flag.len_utf8()..];
            let operand = if attached.is_empty() {
                n += 1;
                args.get(n - 1).map(String::as_str)
            } else {
                Some(attached)
            };
            flags.push((flag, operand));
            break;
        }
    }
    flags
}

/// zsh `print`: `-f FORMAT` behaves as `printf`; `-u N` is routed by
/// [`route_print_unit`]; any other flag outside [`PRINT_QUIET_FLAGS`]
/// (`-v NAME`, `-m`, `-C N`, …) counts as reporting.
fn print_reports(args: &[String], lifted: &Lifted) -> bool {
    print_flags(args)
        .into_iter()
        .any(|(flag, operand)| match flag {
            'f' => format_reports(operand.unwrap_or_default(), lifted),
            'u' => false,
            _ => !PRINT_QUIET_FLAGS.contains(flag),
        })
}

/// Route the descriptor a carrying zsh `print -u N` writes the value to
/// (#8735 round 1): `N` a literal digit routes that descriptor's sink.
pub(super) fn route_print_unit(
    args: &[String],
    fds: &[Sink; 10],
    emitted: &mut Emitted,
) -> Result<(), Refusal> {
    let mut unit = None;
    for (flag, operand) in print_flags(args) {
        match flag {
            'p' => return Err(Refusal::Unreadable("the coprocess `print -p` writes to")),
            'u' => unit = Some(operand),
            _ => {}
        }
    }
    let Some(operand) = unit else {
        return Ok(());
    };
    let literal = operand
        .filter(|o| o.len() == 1)
        .and_then(|o| o.parse::<usize>().ok());
    match literal.and_then(|n| fds.get(n)) {
        Some(&sink) => route(sink, emitted),
        // #8735 round 1: fail closed — a run-time descriptor may be the terminal.
        None => Err(Refusal::Unreadable("the descriptor `print -u` writes to")),
    }
}

/// Whether a `printf` format may make it quote an operand on stderr: the
/// format carries or is chosen at run time, a width or precision is read from
/// an operand (`*`), or a conversion is not in [`TEXT_CONVERSIONS`].
fn format_reports(format: &str, lifted: &Lifted) -> bool {
    // #8735: fail closed — a run-time format could hold any conversion.
    if carries(format, lifted)
        || format.contains('$')
        || format.contains('`')
        || format.contains(MARK)
    {
        return true;
    }
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        if chars.next_if_eq(&'%').is_some() {
            continue;
        }
        while chars.next_if(|f| "-+ #0'".contains(*f)).is_some() {}
        let mut star = false;
        while let Some(w) = chars.next_if(|w| w.is_ascii_digit() || matches!(w, '*' | '.')) {
            star |= w == '*';
        }
        let conversion = chars.next();
        if star || conversion.is_none_or(|v| !TEXT_CONVERSIONS.contains(v)) {
            return true;
        }
    }
    false
}
