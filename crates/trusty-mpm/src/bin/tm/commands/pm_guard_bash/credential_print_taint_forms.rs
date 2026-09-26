//! Shell forms that bind or read a credential-holding variable before the
//! stage's parens are blanked (#8676).
//!
//! Why: `ungroup` blanks every unquoted paren so a subshell's program is seen,
//! and that also erases `NAME=( … )`, `$(( … ))`, `(( … ))` and a `name() {`
//! header. Each is read here from the stage text as written.
//! What: [`array_bindings`] names the arrays a compound assignment fills from a
//! carrying element; [`reads_in_arithmetic`] finds a tainted name read by an
//! arithmetic context, whose error message echoes a non-numeric value;
//! [`function_header_words`] counts the words of a leading function header.
//! Test: `credential_print_tests::denies_array_assignment_copies`,
//! `credential_print_tests::denies_arithmetic_reads`,
//! `credential_print_tests::denies_after_a_function_header`.

use std::collections::BTreeSet;

use super::super::bash_tokens::tokenize;
use super::super::shell_lex::QuoteScan;
use super::credential_print_taint::{ANY, expands_tainted, is_identifier, is_tainted};
use super::{Lifted, carries};

/// `test`/`[[` operators that evaluate both operands as arithmetic.
const ARITH_TESTS: &[&str] = &["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];

/// Whether `b` can be part of a shell variable name.
fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The names `NAME=( … )` and `NAME+=( … )` bind from a carrying element,
/// including the operand of `declare -a`/`local -A`.
///
/// What: finds each unquoted `=(` whose left side is an identifier at a word
/// start, tokenizes the list up to the matching `)`, and binds `NAME` when any
/// element carries. Fail-closed: an unclosed or unlexable list binds [`ANY`].
pub(super) fn array_bindings(stage: &str, lifted: &Lifted) -> Vec<String> {
    let quotes = QuoteScan::new(stage);
    let bytes = stage.as_bytes();
    let mut bound = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let opens =
            bytes[i] == b'=' && bytes[i + 1] == b'(' && (!quotes.balanced || quotes.is_unquoted(i));
        if !opens {
            i += 1;
            continue;
        }
        let name_end = if i > 0 && bytes[i - 1] == b'+' {
            i - 1
        } else {
            i
        };
        let mut name_start = name_end;
        while name_start > 0 && ident_byte(bytes[name_start - 1]) {
            name_start -= 1;
        }
        let at_word = name_start == 0 || bytes[name_start - 1].is_ascii_whitespace();
        let name = &stage[name_start..name_end];
        let Some(close) = matching_close(bytes, i + 2, 1) else {
            bound.push(ANY.to_string());
            return bound;
        };
        let list = &stage[i + 2..close];
        match tokenize(list) {
            Err(_) => bound.push(ANY.to_string()),
            Ok(words) if words.iter().any(|w| carries(w, lifted)) => {
                if at_word && is_identifier(name) {
                    bound.push(name.to_string());
                } else {
                    bound.push(ANY.to_string());
                }
            }
            Ok(_) => {}
        }
        i = close + 1;
    }
    bound
}

/// The index of the `)` closing a paren run opened `level` deep before `from`.
fn matching_close(bytes: &[u8], from: usize, level: usize) -> Option<usize> {
    let mut level = level;
    for (at, b) in bytes.iter().enumerate().skip(from) {
        match b {
            b'(' => level += 1,
            b')' => {
                level -= 1;
                if level == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a stage reads a tainted name in an arithmetic context.
///
/// What: the text of every `(( … ))` and `$(( … ))`, every `let` operand, a
/// `[`/`[[`/`test` stage using an [`ARITH_TESTS`] operator, and every array
/// subscript (`A[…]`). A name there is read bare or as `$NAME`; `${#NAME}` is
/// its length and does not count. An unclosed `((` reads to the stage's end.
pub(super) fn reads_in_arithmetic(
    stage: &str,
    argv: &[String],
    program: &str,
    names: &BTreeSet<String>,
) -> bool {
    if names.is_empty() {
        return false;
    }
    let bytes = stage.as_bytes();
    let mut from = 0;
    while let Some(at) = stage[from..].find("((") {
        let open = from + at + 2;
        let close = matching_close(bytes, open, 2).unwrap_or(bytes.len());
        if reads_tainted(&stage[open..close], names) {
            return true;
        }
        from = close.max(open);
    }
    if program == "let" && argv.iter().any(|w| reads_tainted(w, names)) {
        return true;
    }
    let test = argv
        .iter()
        .any(|w| matches!(w.as_str(), "[" | "[[" | "test"));
    if test
        && argv.iter().any(|w| ARITH_TESTS.contains(&w.as_str()))
        && argv.iter().any(|w| reads_tainted(w, names))
    {
        return true;
    }
    subscripts(stage).any(|s| reads_tainted(s, names))
}

/// The text of each `[…]` that directly follows a name character.
fn subscripts(stage: &str) -> impl Iterator<Item = &str> {
    let bytes = stage.as_bytes();
    stage.match_indices('[').filter_map(move |(at, _)| {
        let after_name = at > 0 && ident_byte(bytes[at - 1]);
        let close = stage[at..].find(']').map_or(stage.len(), |c| at + c);
        after_name.then(|| &stage[at + 1..close])
    })
}

/// Whether arithmetic `text` reads a tainted name, bare or expanded.
fn reads_tainted(text: &str, names: &BTreeSet<String>) -> bool {
    if expands_tainted(text, names) {
        return true;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let starts = (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_')
            && (i == 0 || !ident_byte(bytes[i - 1]));
        if !starts {
            i += 1;
            continue;
        }
        let len = bytes[i..].iter().take_while(|b| ident_byte(**b)).count();
        let length_of = i > 0 && bytes[i - 1] == b'#';
        if !length_of && is_tainted(&text[i..i + len], names) {
            return true;
        }
        i += len;
    }
    false
}

/// How many leading words of `stage` are a function-definition header:
/// `name()` is one word, `function name` and `function name()` are two, and 0
/// when the stage opens with none.
pub(super) fn function_header_words(stage: &str) -> usize {
    let text = stage.trim_start();
    let (keyword, rest) = match text.strip_prefix("function") {
        Some(rest) if rest.starts_with(char::is_whitespace) => (true, rest.trim_start()),
        _ => (false, text),
    };
    let name_len = rest
        .bytes()
        .take_while(|b| ident_byte(*b) || matches!(b, b'-' | b'.' | b':'))
        .count();
    if name_len == 0 {
        return 0;
    }
    let after = rest[name_len..].trim_start();
    let parens = after
        .strip_prefix('(')
        .is_some_and(|a| a.trim_start().starts_with(')'));
    match (keyword, parens) {
        (true, _) => 2,
        (false, true) => 1,
        (false, false) => 0,
    }
}
