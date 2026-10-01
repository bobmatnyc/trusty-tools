//! Decoding of live `$'…'` (ANSI-C) quotes, for the rules that must name or
//! see through them (#9001).
//!
//! Why: `shlex` cannot decode `$'…'`, so [`super::unclassifiable_command`]
//! refuses every command that carries one (#6660). Two refusals built on that
//! answer misled the reader. The #8902 Architect pane floor classed
//! `grep -c $'\x1b' log` as an unresolvable tmux command, though it names no
//! tmux at all, and neither refusal said which token it could not read.
//! What: [`decode_ansi_c`] rewrites each live `$'…'` quote as the single-quoted
//! literal bash would produce, so a caller can judge what the command spells
//! (`$'\x74mux'` reads as `'tmux'`). [`unclassifiable_reason`] is the #6660
//! refusal text with the offending token named.
//! FAIL-CLOSED: decoding covers only escapes whose result is one printable or
//! tab/escape-class ASCII byte. `\u`, `\U`, `\c`, an unknown escape, a NUL, a
//! byte above 0x7f, a newline or carriage return, `$"…"` and an unterminated
//! quote all return [`Decoded::Undecodable`], which every caller refuses.
//! Test: `ansi_c_decode_tests.rs`.

use super::{ANSI_C_QUOTING_REASON, unclassifiable_command};

/// Longest token text a refusal quotes.
const TOKEN_CHARS: usize = 60;

/// What [`decode_ansi_c`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    /// No live `$'…'` or `$"…"` quote.
    Plain,
    /// The command with every live `$'…'` rewritten as a `'…'` literal.
    Text(String),
    /// The first token the decoder will not read.
    Undecodable(String),
}

/// Rewrite every live `$'…'` quote in `command` as the literal it decodes to.
///
/// Why: see the module doc. A rule that decodes can judge the program a quote
/// spells instead of refusing the whole command blind.
/// What: walks `command` tracking `'…'`, `"…"` and backslash escapes, as bash
/// reads them; a `$'` outside both quotes opens an ANSI-C quote. `$$` is the
/// PID, not a quote opener. The decoded text is re-quoted with `'…'`, a `'` in
/// it spelled `'\''`.
/// Test: `decodes_each_supported_escape`, `refuses_what_it_cannot_decode`,
/// `leaves_a_quoted_dollar_quote_alone`.
pub(crate) fn decode_ansi_c(command: &str) -> Decoded {
    let chars: Vec<char> = command.chars().collect();
    let (mut out, mut found, mut i) = (String::new(), false, 0);
    let (mut single, mut double) = (false, false);
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if single {
            single = c != '\'';
        } else if c == '\\' {
            out.push(c);
            out.extend(next);
            i += 2;
            continue;
        } else if double {
            double = c != '"';
        } else if c == '$' && next == Some('$') {
            out.push_str("$$");
            i += 2;
            continue;
        } else if c == '$' && next == Some('"') {
            return Decoded::Undecodable(token(&chars[i..], '"'));
        } else if c == '$' && next == Some('\'') {
            let Some((text, len)) = ansi_c_body(&chars[i + 2..]) else {
                return Decoded::Undecodable(token(&chars[i..], '\''));
            };
            out.push('\'');
            out.push_str(&text.replace('\'', "'\\''"));
            out.push('\'');
            found = true;
            i += 2 + len;
            continue;
        } else {
            single = c == '\'';
            double = c == '"';
        }
        out.push(c);
        i += 1;
    }
    if found {
        Decoded::Text(out)
    } else {
        Decoded::Plain
    }
}

/// The #6660 refusal for `command`, naming the `$'…'` token when that is the
/// cause (#9001).
///
/// Why: the bare #6660 text says the guard cannot decode `$'…'` but not which
/// token, so an agent with several quotes cannot tell what to rewrite.
/// What: [`unclassifiable_command`]'s reason; for the ANSI-C reason, plus the
/// first live `$'…'`/`$"…"` token and the `printf` route for a control byte.
/// Test: `the_refusal_names_the_token_it_cannot_decode`.
pub(crate) fn unclassifiable_reason(command: &str) -> Option<String> {
    let reason = unclassifiable_command(command)?;
    if reason != ANSI_C_QUOTING_REASON {
        return Some(reason.to_string());
    }
    let named = match decode_ansi_c(command) {
        Decoded::Undecodable(tok) => tok,
        Decoded::Text(_) | Decoded::Plain => match first_quote(command) {
            Some(tok) => tok,
            None => return Some(reason.to_string()),
        },
    };
    Some(format!(
        "{reason} The token it cannot decode is `{named}`. For a control byte, pass it \
         through printf instead: `grep -c \"$(printf '\\033')\" <file>`."
    ))
}

/// The first live `$'…'` token of `command`, as written.
fn first_quote(command: &str) -> Option<String> {
    let chars: Vec<char> = command.chars().collect();
    let (mut single, mut double, mut i) = (false, false, 0);
    while i < chars.len() {
        let c = chars[i];
        if single {
            single = c != '\'';
        } else if c == '\\' {
            i += 1;
        } else if double {
            double = c != '"';
        } else if c == '$' && chars.get(i + 1) == Some(&'\'') {
            return Some(token(&chars[i..], '\''));
        } else {
            single = c == '\'';
            double = c == '"';
        }
        i += 1;
    }
    None
}

/// The token starting at `rest[0]` (`$` then the opening quote), up to and
/// including the closing `quote`, truncated to [`TOKEN_CHARS`].
fn token(rest: &[char], quote: char) -> String {
    let mut end = rest.len();
    let mut i = 2;
    while i < rest.len() {
        if rest[i] == '\\' {
            i += 2;
            continue;
        }
        if rest[i] == quote {
            end = i + 1;
            break;
        }
        i += 1;
    }
    let text: String = rest[..end.min(rest.len())].iter().collect();
    if text.chars().count() > TOKEN_CHARS {
        format!("{}…", text.chars().take(TOKEN_CHARS).collect::<String>())
    } else {
        text
    }
}

/// Decode an ANSI-C body up to its closing `'`: the text and the number of
/// chars consumed, closing quote included. `None` fails closed.
fn ansi_c_body(body: &[char]) -> Option<(String, usize)> {
    let (mut out, mut i) = (String::new(), 0);
    loop {
        match body.get(i)? {
            '\'' => return Some((out, i + 1)),
            '\\' => {
                let (byte, len) = escape(&body[i + 1..])?;
                // #9001: NUL truncates in bash; a newline would move segment cuts.
                if byte == 0 || byte > 0x7f || byte == b'\n' || byte == b'\r' {
                    return None;
                }
                out.push(char::from(byte));
                i += 1 + len;
            }
            c => {
                out.push(*c);
                i += 1;
            }
        }
    }
}

/// One escape after its backslash: the byte and the chars it used.
fn escape(rest: &[char]) -> Option<(u8, usize)> {
    let simple = match rest.first()? {
        'a' => Some(0x07),
        'b' => Some(0x08),
        'e' | 'E' => Some(0x1b),
        'f' => Some(0x0c),
        'n' => Some(b'\n'),
        'r' => Some(b'\r'),
        't' => Some(b'\t'),
        'v' => Some(0x0b),
        '\\' => Some(b'\\'),
        '\'' => Some(b'\''),
        '"' => Some(b'"'),
        '?' => Some(b'?'),
        _ => None,
    };
    if let Some(byte) = simple {
        return Some((byte, 1));
    }
    let (radix, from, max) = match rest[0] {
        'x' => (16, 1, 2),
        '0'..='7' => (8, 0, 3),
        _ => return None,
    };
    let digits: String = rest[from..]
        .iter()
        .take(max)
        .take_while(|c| c.is_digit(radix))
        .collect();
    let value = u32::from_str_radix(&digits, radix).ok()?;
    Some((u8::try_from(value).ok()?, from + digits.len()))
}

#[cfg(test)]
#[path = "ansi_c_decode_tests.rs"]
mod tests;
