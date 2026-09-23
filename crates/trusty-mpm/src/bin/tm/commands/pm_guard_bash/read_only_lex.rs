//! Fail-closed tokenizer for the read-only dispatch rule (#8439).
//!
//! Why: the first cut of #8439 lexed the whole shell grammar and tried to
//! classify every construct. Its lexer skipped only ` ` and `\t` between words
//! while its word reader stopped at any Unicode whitespace, so a `\r` or NBSP
//! left the index still — the hook looped until its 10 s timeout and the
//! command then ran. This lexer accepts a small, closed alphabet and refuses
//! every byte outside it, so there is no construct it can half-read.
//! What: [`lex`] turns a command into [`Tok`]s or an `Err` naming what it
//! refused. Accepted bytes are printable ASCII plus space, tab and newline.
//! Words are unquoted runs of [`BARE`] bytes, `'…'` spans, and `"…"` spans
//! with no `$`, backtick, `\` or `!`. Two expansions are recognised so the
//! caller can confine them: `"$NAME"`/`"${NAME}"` ([`WordKind::Var`]) and a
//! bare path word led by `/` or `~/` carrying `*`/`?` ([`WordKind::Expanding`]).
//! Operators are `|`, `;`/newline, and the exact tokens `2>&1` and
//! `2>/dev/null`. Everything else — `$`, backtick, `\`, `<`, `>`, `&`, parens,
//! braces, brackets, `#`, `!`, a leading `=` or `^`, `~` anywhere but a word's
//! start or mid-word — is an `Err`. Every loop iteration consumes at least one
//! byte, so lexing is linear in the input and always terminates.
//! Test: `read_only_allow_tests::lexer_refuses_every_byte_outside_its_alphabet`,
//! `read_only_allow_tests::lexing_terminates_on_arbitrary_bytes`.

/// Longest command the rule will read; longer is refused (#8439).
const MAX_COMMAND_BYTES: usize = 16 * 1024;

/// Bytes a bare (unquoted) word may carry beyond ASCII alphanumerics.
///
/// Why: each is inert in bash and zsh when unquoted and not at a word's start;
/// `*`/`?` are globs, confined by the caller to a `for` list.
const BARE: &[u8] = b"_-./,:@%+=*?~^";

/// One lexed token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tok {
    /// A word.
    Word(Word),
    /// `|`.
    Pipe,
    /// `;` or a newline.
    Sep,
    /// The exact token `2>&1` or `2>/dev/null`: stderr joins stdout or is dropped.
    StderrRedirect,
}

/// How much of a word's value is known before the shell runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WordKind {
    /// No expansion: the value is [`Word::text`].
    Lit,
    /// `"$NAME"` or `"${NAME}"`: the named variable's value, unsplit.
    Var(String),
    /// A bare path led by `/` or `~/`, expanded by tilde and/or glob.
    Expanding,
}

/// A lexed word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    /// The decoded value for [`WordKind::Lit`]; the raw text otherwise.
    pub(super) text: String,
    /// No quote character appears in the word.
    pub(super) bare: bool,
    /// See [`WordKind`].
    pub(super) kind: WordKind,
}

impl Word {
    /// The literal value, when the word has one.
    pub(super) fn lit(&self) -> Option<&str> {
        (self.kind == WordKind::Lit).then_some(self.text.as_str())
    }

    /// A bare literal equal to `keyword`.
    pub(super) fn is_keyword(&self, keyword: &str) -> bool {
        self.bare && self.lit() == Some(keyword)
    }
}

/// Is `b` a word delimiter?
fn delimits(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b';' | b'|')
}

/// Tokenize `command`, refusing every construct outside the closed alphabet.
///
/// Why: see the module doc.
/// What: `Ok(tokens)` or `Err(what was refused)`.
/// Test: as the module doc.
pub(super) fn lex(command: &str) -> Result<Vec<Tok>, String> {
    let bytes = command.as_bytes();
    if bytes.len() > MAX_COMMAND_BYTES {
        return Err("a command longer than the guard reads".into());
    }
    if let Some(bad) = bytes
        .iter()
        .find(|&&b| !(b == b'\n' || b == b'\t' || (0x20..=0x7e).contains(&b)))
    {
        return Err(format!(
            "the byte 0x{bad:02x}, which the guard does not read"
        ));
    }
    let mut toks = Vec::new();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b' ' | b'\t' => i += 1,
            b'\n' | b';' => {
                toks.push(Tok::Sep);
                i += 1;
            }
            b'|' => {
                if bytes.get(i + 1) == Some(&b'|') {
                    return Err("`||`".into());
                }
                toks.push(Tok::Pipe);
                i += 1;
            }
            _ => {
                if let Some(len) = stderr_redirect_at(bytes, i) {
                    toks.push(Tok::StderrRedirect);
                    i += len;
                    continue;
                }
                let (word, next) = word_at(bytes, i)?;
                toks.push(Tok::Word(word));
                i = next;
            }
        }
    }
    Ok(toks)
}

/// The length of a `2>&1` / `2>/dev/null` token at `i`, when one stands there.
fn stderr_redirect_at(bytes: &[u8], i: usize) -> Option<usize> {
    [b"2>&1".as_slice(), b"2>/dev/null".as_slice()]
        .into_iter()
        .find(|t| bytes[i..].starts_with(t) && bytes.get(i + t.len()).is_none_or(|&b| delimits(b)))
        .map(<[u8]>::len)
}

/// Lex one word starting at `start`; returns it and the index after it.
///
/// What: every branch advances `j` or returns, so the loop terminates.
fn word_at(bytes: &[u8], start: usize) -> Result<(Word, usize), String> {
    if let Some(var) = var_word_at(bytes, start) {
        return Ok(var);
    }
    let mut text = String::new();
    let mut bare = true;
    let mut expands = false;
    let mut j = start;
    while let Some(&b) = bytes.get(j) {
        if delimits(b) {
            break;
        }
        match b {
            b'\'' | b'"' => {
                let close = quoted_span_end(bytes, j)?;
                text.push_str(&String::from_utf8_lossy(&bytes[j + 1..close]));
                bare = false;
                j = close + 1;
            }
            b'~' if j == start && bytes.get(j + 1) == Some(&b'/') => {
                expands = true;
                text.push('~');
                j += 1;
            }
            b'~' if j == start || matches!(bytes[j - 1], b'=' | b':') => {
                return Err("a `~` expansion".into());
            }
            b'=' | b'^' if j == start => {
                return Err(format!("a word led by `{}`", char::from(b)));
            }
            b'^' if bytes[j - 1] == b'/' => return Err("a `/^` glob".into()),
            b'*' | b'?' => {
                expands = true;
                text.push(char::from(b));
                j += 1;
            }
            _ if b.is_ascii_alphanumeric() || BARE.contains(&b) => {
                text.push(char::from(b));
                j += 1;
            }
            _ => return Err(format!("`{}`", char::from(b))),
        }
    }
    if !expands {
        return Ok((
            Word {
                text,
                bare,
                kind: WordKind::Lit,
            },
            j,
        ));
    }
    // #8439: a glob or tilde word is kept only as a bare path, so its expansion
    // always starts with `/` (or stays a literal `~/…`) and can never be a flag.
    let path = text.starts_with('/') || text.starts_with("~/");
    if !bare || !path {
        return Err("a glob outside a bare absolute path".into());
    }
    Ok((
        Word {
            text,
            bare,
            kind: WordKind::Expanding,
        },
        j,
    ))
}

/// The index of the quote closing the span opened at `open`.
///
/// What: a `'…'` span may hold any accepted byte but a newline; a `"…"` span
/// also refuses `$`, backtick, `\` and `!`, each of which is live inside double
/// quotes in bash or zsh. An unclosed span is an `Err`.
fn quoted_span_end(bytes: &[u8], open: usize) -> Result<usize, String> {
    let quote = bytes[open];
    for (k, &b) in bytes.iter().enumerate().skip(open + 1) {
        if b == quote {
            return Ok(k);
        }
        if b == b'\n' {
            return Err("a newline inside quotes".into());
        }
        if quote == b'"' && matches!(b, b'$' | b'`' | b'\\' | b'!') {
            return Err(format!("`{}` inside double quotes", char::from(b)));
        }
    }
    Err("an unclosed quote".into())
}

/// A whole word that is exactly `"$NAME"` or `"${NAME}"`.
fn var_word_at(bytes: &[u8], start: usize) -> Option<(Word, usize)> {
    let rest = bytes.get(start..)?.strip_prefix(b"\"$")?;
    let close = rest.iter().position(|&b| b == b'"')?;
    let inner = &rest[..close];
    let name = match inner.strip_prefix(b"{") {
        Some(braced) => braced.strip_suffix(b"}")?,
        None => inner,
    };
    let valid = name
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let end = start + 2 + close + 1;
    if !valid || bytes.get(end).is_some_and(|&b| !delimits(b)) {
        return None;
    }
    let name = String::from_utf8_lossy(name).into_owned();
    let text = String::from_utf8_lossy(&bytes[start..end]).into_owned();
    Some((
        Word {
            text,
            bare: false,
            kind: WordKind::Var(name),
        },
        end,
    ))
}
