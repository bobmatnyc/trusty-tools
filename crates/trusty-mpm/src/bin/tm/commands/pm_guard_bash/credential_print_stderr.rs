//! Which argument printers repeat a credential operand on stderr, for the
//! credential-print rule (#8735).
//!
//! Why: `wc -c < <(cat "$T")` routed only `cat`'s stdout, but `cat` given a
//! value as a filename fails with `cat: <value>: No such file`, written to the
//! stderr the `<(…)` body shares with the outer command. `printf` does the
//! same for an operand a numeric conversion rejects (`printf: <value>: invalid
//! number`), and zsh `print -f` likewise.
//! What: [`reports_operand_on_stderr`] says whether a `super::ARG_PRINTERS`
//! call given a carrying operand may write that operand to stderr. `echo`
//! never does. A `printf` format chosen at run time, or a `print` flag this
//! module does not know, counts as reporting: the guard cannot read it.
//! Test: `credential_print_tests::denies_a_credential_operand_reported_on_stderr_8735`,
//! `credential_print_tests::denies_an_unreadable_stderr_of_a_reporting_printer_8735`,
//! `credential_print_tests::allows_the_8735_neighbours`.

use super::{Lifted, MARK, carries};

/// zsh `print` flags that take no operand and report no error.
const PRINT_QUIET_FLAGS: &str = "rnlNcaoOiRPEebD";

/// `printf` conversions that copy an operand as text and never reject it.
const TEXT_CONVERSIONS: &str = "sbqc";

/// Whether `program`, given a carrying operand in `args`, may write it to
/// stderr in an error message.
pub(super) fn reports_operand_on_stderr(program: &str, args: &[String], lifted: &Lifted) -> bool {
    match program {
        "cat" => true,
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

/// zsh `print`: `-f FORMAT` behaves as `printf`; any flag outside
/// [`PRINT_QUIET_FLAGS`] (`-u FD`, `-v NAME`, `-m PATTERN`, …) refuses.
fn print_reports(args: &[String], lifted: &Lifted) -> bool {
    for (n, word) in args.iter().enumerate() {
        let Some(flags) = word
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && *f != "-")
        else {
            return false;
        };
        for (at, flag) in flags.char_indices() {
            if flag == 'f' {
                let attached = &flags[at + 1..];
                let format = match attached {
                    "" => args.get(n + 1).map(String::as_str).unwrap_or_default(),
                    _ => attached,
                };
                return format_reports(format, lifted);
            }
            if !PRINT_QUIET_FLAGS.contains(flag) {
                return true;
            }
        }
    }
    false
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
