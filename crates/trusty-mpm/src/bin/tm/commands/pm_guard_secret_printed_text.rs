//! The inert operands of a command made only of `printf`/`echo` calls (#10685).
//!
//! Why: the #7266 file rule cuts every argv token at each byte a path cannot
//! hold and judges each fragment as a filename. A printed regex
//! (`'(secret|token|[.]env)'`), a regex-escaped name (`'process\.env\.TZ'`)
//! and Markdown emphasis (`'**e.** item'`) were refused, though `printf` and
//! `echo` open no operand. The #8723 release covered only a lone printer that
//! writes to a file.
//! What: [`closed_printer_text`] answers whether every segment of a command is
//! a printer call that can run, redirect or expand nothing, and
//! [`inert_operand_indices`] names the operands that are text, not a filename.
//! Both are consulted only to GRANT, after the shell tokenization succeeded;
//! any doubt keeps the deny scan. Design: `docs/briefs/2026-11-04-10685-parser-design.md`
//! rules A, B and D (rule C, a stdout `cat` here-document, is out of scope).
//! Test: `pm_guard_secret_script::fp_10685_tests`.

use crate::commands::pm_guard_bash::{
    TEXT_PRINTERS, runs_a_nested_command, split_shell_segments, tokenize,
};
use crate::commands::pm_guard_secret_read::command_basename;
use crate::commands::pm_guard_secret_words::is_path_byte;

/// Whether `command` is made only of closed `printf`/`echo` calls.
///
/// Why: a printer beside any other segment can feed what it prints to a
/// reader or a shell (`printf 'cat .env' | sh`, `exec > s.sh; printf ...;
/// bash s.sh`), so the release needs a command that has no writer and no
/// reader at all (design rule A).
/// What: `false` unless the command carries no C0 control byte other than
/// LF and TAB (rule D), no shell comment or unclosed quote
/// ([`has_comment_or_open_quote`]), and every non-empty segment of [`split_shell_segments`]
/// tokenizes, has a [`TEXT_PRINTERS`] basename as its first word, runs nothing
/// nested ([`runs_a_nested_command`]), has no token carrying `<` or `>`, and,
/// for `printf`, carries no `-v`. At least one printer call is required.
/// Test: `near_miss_rows_still_refuse_10685`,
/// `cr_in_printer_text_refuses_10685`,
/// `unbalanced_quote_printer_refuses_10685`,
/// `multi_line_printer_script_allows_10685`,
/// `comment_hidden_reader_refuses_10685`.
pub(crate) fn closed_printer_text(command: &str) -> bool {
    if command
        .bytes()
        .any(|b| b < 0x20 && b != b'\n' && b != b'\t')
    {
        return false;
    }
    // #10685 review H1: a `'` or `"` inside a shell comment is no quote, so the
    // quote-only scan and the segment split misread the lines after it.
    if has_comment_or_open_quote(command) {
        return false;
    }
    let mut printers = 0usize;
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(argv) = tokenize(trimmed) else {
            return false;
        };
        if !is_closed_printer_call(trimmed, &argv) {
            return false;
        }
        printers += 1;
    }
    printers > 0
}

/// Whether `command` has an unquoted word-initial `#` or a quote it never closes.
///
/// Why: `QuoteScan` and the segment split know `'` and `"` but not comments,
/// so `echo x # it's⏎dd if=.env⏎# '` reads as one printer segment while the
/// shell runs `dd` (#10685 review H1). Any comment withdraws the release.
/// What: a comment-aware walk over `'`, `"` and `\`; `#` opens a comment
/// when it starts a word (after start, whitespace or `;&|()<>`). An open
/// quote at the end also answers `true` (fail closed).
/// Test: `comment_hidden_reader_refuses_10685`,
/// `trailing_comment_printer_release_withdrawn_10685`.
fn has_comment_or_open_quote(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(b'"') if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'\\' => i += 1,
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'#' => {
                let word_start = i == 0
                    || bytes[i - 1].is_ascii_whitespace()
                    || b";&|()<>".contains(&bytes[i - 1]);
                if word_start {
                    return true;
                }
            }
            None => {}
        }
        i += 1;
    }
    quote.is_some()
}

/// Whether one tokenized segment is a printer call that only prints.
///
/// Test: `near_miss_rows_still_refuse_10685`, `path_qualified_printer_refuses_10685`.
fn is_closed_printer_call(segment: &str, argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    // #10685 review: only the shell's own printer, bare or from a system bin
    // directory; `./echo` or `x/printf` can be any executable.
    let program = command_basename(first);
    let system = ["", "/bin/", "/usr/bin/"]
        .iter()
        .any(|dir| first.strip_prefix(dir) == Some(program.as_str()));
    system
        && TEXT_PRINTERS.contains(&program.as_str())
        && !runs_a_nested_command(segment)
        && !argv.iter().any(|t| t.contains(['<', '>']))
        && !(program == "printf" && argv.iter().skip(1).any(|t| t.starts_with("-v")))
}

/// The operand indices of a closed printer call that are text, not a filename.
///
/// Why: design rule B. A token holding a byte no path can hold (a space, a
/// quote, `(`, `|`, `\`, `%`, `:`) is prose, a regex, JSON or a format
/// string. A token made only of path bytes stays screened, so `echo .env` and
/// `printf '%s' .env` still refuse (#8723 pin).
/// What: every index after the program whose token has a byte
/// [`is_path_byte`] rejects. The caller owns the gate: it applies these only
/// when [`closed_printer_text`] holds for the whole command.
/// Test: `literal_regex_printf_allows_10685`,
/// `escaped_dot_timezone_printf_allows_10685`,
/// `near_miss_rows_still_refuse_10685`.
pub(crate) fn inert_operand_indices(argv: &[String]) -> Vec<usize> {
    argv.iter()
        .enumerate()
        .skip(1)
        .filter(|(_, token)| token.chars().any(|c| !is_path_byte(c)))
        .map(|(index, _)| index)
        .collect()
}
