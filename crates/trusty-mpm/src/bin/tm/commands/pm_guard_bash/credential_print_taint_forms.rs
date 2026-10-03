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
//! `credential_print_tests::denies_array_keys_read_as_arithmetic`,
//! `credential_print_tests::denies_arithmetic_reads`,
//! `credential_print_tests::denies_arithmetic_reads_in_offsets_and_integer_declarations`,
//! `credential_print_tests::denies_after_a_function_header`.

use std::collections::BTreeSet;

use super::super::bash_tokens::tokenize;
use super::super::shell_lex::QuoteScan;
use super::credential_print_taint::{
    ANY, INTEGER, declares_integer, expands_tainted, is_identifier, is_tainted, leading_name,
};
use super::{Lifted, Refusal, carries};

// #8771: bytes this thread handed to an arithmetic reader, so a test can
// prove the reading stays linear in the command (no uncharged rescans).
#[cfg(test)]
thread_local! {
    pub(super) static ARITH_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count `len` bytes handed to an arithmetic reader (test builds only).
fn count_read(len: usize) {
    #[cfg(test)]
    ARITH_BYTES.with(|c| c.set(c.get() + len));
    #[cfg(not(test))]
    let _ = len;
}

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
/// #8676 round 4: an indexed array evaluates an element's `[key]` as
/// arithmetic, so a key reading a tainted name refuses as [`Refusal::Prints`];
/// an unlexable list refuses when its text reads one.
pub(super) fn array_bindings(stage: &str, lifted: &Lifted) -> Result<Vec<String>, Refusal> {
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
            // #8676 round 4: the unclosed list's keys cannot be told apart.
            if reads_tainted(&stage[i + 2..], &lifted.names) {
                return Err(Refusal::Prints);
            }
            bound.push(ANY.to_string());
            return Ok(bound);
        };
        let list = &stage[i + 2..close];
        match tokenize(list) {
            Err(_) if reads_tainted(list, &lifted.names) => return Err(Refusal::Prints),
            Err(_) => bound.push(ANY.to_string()),
            // #8676 round 4: `A=([T]=1)` evaluates `T`.
            Ok(words)
                if words
                    .iter()
                    .filter_map(|w| element_key(w))
                    .any(|key| reads_tainted(key, &lifted.names)) =>
            {
                return Err(Refusal::Prints);
            }
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
    Ok(bound)
}

/// The `key` of a compound-assignment element `[key]=value` or
/// `[key]+=value` (#8676 round 4): the text before the first `]=`/`]+=`.
fn element_key(word: &str) -> Option<&str> {
    let body = word.strip_prefix('[')?;
    let end = [body.find("]="), body.find("]+=")]
        .into_iter()
        .flatten()
        .min()?;
    Some(&body[..end])
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
/// subscript (`A[…]`); `$[…]`, a `${NAME:offset:length}` operand, and an
/// assignment value while a name is integer (`declare -i`). A name there is
/// read bare or as `$NAME`; `${#NAME}` is its length and does not count. An
/// unclosed `((` reads to the stage's end.
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
        count_read(close - open);
        if reads_tainted(&stage[open..close], names) {
            return true;
        }
        from = close.max(open);
    }
    if program == "let" && argv.iter().any(|w| reads_tainted(w, names)) {
        return true;
    }
    // #8676 round 3: a subscript, `$[…]`, a `${X:offset:length}`, and an
    // assignment to an integer name (`declare -i X=T`, or `X=T` after one)
    // are arithmetic. One linear pass each: a per-opener scan was quadratic.
    let after = |at: usize, want: fn(u8) -> bool| at > 0 && want(bytes[at - 1]);
    let subscript = |at| after(at, |b| ident_byte(b) || b == b'$');
    if any_body(stage, (b'[', b']'), subscript, |s| reads_tainted(s, names))
        || any_body(
            stage,
            (b'{', b'}'),
            |at| after(at, |b| b == b'$'),
            |s| offset_operand(s).is_some_and(|o| reads_tainted(o, names)),
        )
    {
        return true;
    }
    let integer = declares_integer(program, argv) || names.contains(INTEGER);
    if integer
        && argv
            .iter()
            .filter_map(|w| w.split_once('=').map(|(_, v)| v))
            .any(|v| reads_tainted(v, names))
    {
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
    false
}

/// Bracket nesting [`any_body`] follows before it counts as reading (#8676
/// round 3); it also bounds how many bodies hold any one byte.
const MAX_NEST: usize = 32;

/// Whether `reads` holds for the body of any `open`…`close` run whose opener
/// `starts(at)` accepts, in one pass; an unclosed body runs to the end.
/// Fail-closed: nesting deeper than [`MAX_NEST`] counts as reading.
fn any_body(
    text: &str,
    (open, close): (u8, u8),
    starts: impl Fn(usize) -> bool,
    reads: impl Fn(&str) -> bool,
) -> bool {
    let mut stack: Vec<Option<usize>> = Vec::new();
    for (at, b) in text.bytes().enumerate() {
        if b == open {
            stack.push(starts(at).then_some(at + 1));
            if stack.len() > MAX_NEST {
                return true;
            }
        } else if b == close
            && let Some(Some(from)) = stack.pop()
            && {
                count_read(at - from);
                reads(&text[from..at])
            }
        {
            return true;
        }
    }
    stack.into_iter().flatten().any(|from| {
        count_read(text.len() - from);
        reads(&text[from..])
    })
}

/// The offset and length text of a `${NAME:offset[:length]}` or
/// `${NAME[i]:…}` body, which bash evaluates as arithmetic; `None` for the
/// `:-`, `:=`, `:?` and `:+` defaults and for every other form.
fn offset_operand(body: &str) -> Option<&str> {
    let name = leading_name(body);
    let mut after = body.get(name.len()..).filter(|_| !name.is_empty())?;
    if after.starts_with('[') {
        after = after.find(']').map_or("", |end| &after[end + 1..]);
    }
    let rest = after.strip_prefix(':')?;
    (!rest.starts_with(['-', '=', '?', '+'])).then_some(rest)
}

/// Whether arithmetic `text` reads a tainted name, bare or expanded.
pub(super) fn reads_tainted(text: &str, names: &BTreeSet<String>) -> bool {
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
