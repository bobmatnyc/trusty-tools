//! Here-document delimiter reading for the PM Bash guard's body scanner.
//!
//! Why: [`super::HeredocBodies`] and the credential-print walk must split a
//! delimiter word one way (#9150), and `heredoc.rs` had no room left under
//! the 500-SLOC cap for the #9720 fixes (#9720).
//! What: [`delimiters_on`] lists the delimiters one operator line opens;
//! [`delimiter_word`] reads one word by the #9150 allowlist; [`loose_word`]
//! reads an ambiguous `<<`'s word only to compare it with later lines.
//! Test: `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`,
//! `heredoc_bodies_skip_an_operator_outside_code`.

use super::super::credential_print::OperatorCtx;
use super::super::shell_lex::QuoteScan;

/// A here-document delimiter word plus its `<<-` tab-stripping mode.
pub(super) struct Delimiter {
    pub(super) word: String,
    /// `<<-` lets the terminator line be indented with tabs.
    pub(super) strip_tabs: bool,
    /// #8756: a quoted delimiter (`<<'EOF'`) leaves the body unexpanded.
    pub(super) quoted: bool,
    /// #9150: an [`OperatorCtx::Ambiguous`] `<<`, read loosely.
    pub(super) ambiguous: bool,
}

/// The here-document delimiters opened by one line, in the order the shell
/// reads their bodies.
///
/// What: scans for an unquoted `<<` that is not the `<<<` here-string
/// operator, honours the `<<-` tab-stripping form, and reads the delimiter
/// word with its quoting removed (`<<'PY'`, `<<"PY"`, and `<<PY` name the same
/// terminator). `at` is the line's byte offset in the command and the offset
/// `quotes` was built over: the whole command's, or `0` for the line's own.
///
/// #9150: a `<<` opens a body only where [`super::heredoc_operators`] read it as
/// shell code; one in a comment, `${…}` text or after a `\` is skipped, and an
/// [`OperatorCtx::Ambiguous`] one is read loosely for [`super::HeredocBodies::collect`]
/// to refuse. `None` — no confidence — when a code `<<` has a word
/// [`delimiter_word`] does not accept.
/// Test: `heredoc_bodies_ignore_a_here_string`,
/// `heredoc_bodies_ignore_quoted_operator`,
/// `heredoc_bodies_handle_tab_stripped_delimiter`,
/// `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`,
/// `heredoc_bodies_skip_an_operator_outside_code`.
pub(super) fn delimiters_on(
    line: &str,
    (at, offset): (usize, usize),
    quotes: &QuoteScan,
    ops: &[(usize, OperatorCtx)],
) -> Option<Vec<Delimiter>> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if !quotes.is_unquoted(offset + i) || bytes[i] != b'<' || bytes[i + 1] != b'<' {
            i += 1;
            continue;
        }
        if bytes.get(i + 2) == Some(&b'<') {
            // `<<<` is a here-string: its word is on this line, no body follows.
            i += 3;
            continue;
        }
        let Some(&(_, ctx)) = ops.iter().find(|(pos, _)| *pos == at + i) else {
            // #9150: a comment, `${…}` text or `\<<` opens no body.
            i += 2;
            continue;
        };
        let mut j = i + 2;
        let strip_tabs = bytes.get(j) == Some(&b'-');
        if strip_tabs {
            j += 1;
        }
        while matches!(bytes.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let ambiguous = ctx == OperatorCtx::Ambiguous;
        let (word, quoted, end) = if ambiguous {
            loose_word(bytes, j)
        } else {
            delimiter_word(bytes, j)?
        };
        if !word.is_empty() {
            found.push(Delimiter {
                word,
                strip_tabs,
                quoted,
                ambiguous,
            });
        }
        i = end.max(i + 2);
    }
    Some(found)
}

/// The delimiter word starting at `start`: the word, whether any of it was
/// quoted (#8756), and where it ends.
///
/// What: an allowlist (#9150). The word is a run of [`is_word_byte`] bytes,
/// `'…'` or `"…"` around such bytes, and `\` before one, in any
/// concatenation (`E'O'F`); it ends at a blank, `<`, `>`, `|`, `;`, `&`, `)`
/// or the line's end. Anything else — `$`, a backtick, `(`, `<(`, a `\` inside
/// quotes, an empty quoted word, a quote left open — is `None`, because the
/// shell's word there is not one this scan can compare a line with. An empty
/// unquoted word is `Some` and opens no body. `bytes` is the operator line,
/// newline excluded, so a CRLF line's `\r` is its last byte.
///
/// #9150: the credential-print walk reads its operators through this too,
/// so both scanners split a delimiter word one way.
/// Test: `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`,
/// `credential_print_tests::denies_a_body_behind_a_refused_delimiter`.
pub(crate) fn delimiter_word(bytes: &[u8], start: usize) -> Option<(String, bool, usize)> {
    let mut word = String::new();
    let mut quoted = false;
    let mut j = start;
    while let Some(&byte) = bytes.get(j) {
        match byte {
            b'\'' | b'"' => {
                let len = bytes[j + 1..].iter().position(|b| *b == byte)?;
                let inner = &bytes[j + 1..j + 1 + len];
                if inner.is_empty() || !inner.iter().all(|b| is_word_byte(*b)) {
                    return None;
                }
                word.extend(inner.iter().map(|b| char::from(*b)));
                quoted = true;
                j += len + 2;
            }
            b'\\' => {
                let next = bytes.get(j + 1).filter(|b| is_word_byte(**b))?;
                word.push(char::from(*next));
                quoted = true;
                j += 2;
            }
            // A CRLF line keeps its `\r`, as the shell's word does.
            b'\r' if j + 1 == bytes.len() => {
                word.push('\r');
                j += 1;
            }
            // A `<(…)` or `>(…)` is process substitution inside the word.
            b'<' | b'>' if bytes.get(j + 1) == Some(&b'(') => return None,
            b' ' | b'\t' | b'<' | b'>' | b'|' | b';' | b'&' | b')' => break,
            _ if is_word_byte(byte) => {
                word.push(char::from(byte));
                j += 1;
            }
            _ => return None,
        }
    }
    Some((word, quoted, j))
}

/// Whether `byte` may appear in a delimiter word: `[A-Za-z0-9_.-]` (#9150).
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
}

/// An ambiguous `<<`'s word read loosely — quotes and `\` removed, cut at an
/// unquoted break byte — only to ask whether a later line matches it (#9150).
fn loose_word(bytes: &[u8], start: usize) -> (String, bool, usize) {
    let mut word = Vec::new();
    let mut quote = None;
    let mut j = start;
    while let Some(&byte) = bytes.get(j) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => word.push(byte),
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if byte == b'\\' => {
                j += 1;
                word.extend(bytes.get(j));
            }
            None if is_word_break(byte) => break,
            None => word.push(byte),
        }
        j += 1;
    }
    let word = String::from_utf8_lossy(&word).into_owned();
    (word, true, j.min(bytes.len()))
}

/// Whether `byte` ends a loosely read word.
fn is_word_break(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'<' | b'>' | b'|' | b';' | b'&' | b'(' | b')' | b']'
    )
}
