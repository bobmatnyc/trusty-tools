//! The comment- and literal-aware Rust lexer the source-scan guards share.
//!
//! Why: the #9121 credential scan and the #9127 step-reader scan both read
//! production source, and a `//` inside a URL or a `}` inside a string must
//! not end a comment or a test module early in either. One copy keeps the
//! two from drifting.
//! What: [`lex`] tags each char code or literal with comments removed;
//! [`code_only`] also cuts every `#[cfg(test)]` module. Test-only: each scan
//! includes this file with `#[path]`.
//! Test: `the_scan_reads_past_test_modules_aliases_and_urls`,
//! `the_step_scan_flags_an_unrefused_reader`.

/// One source char and whether it is code (`true`) or inside a literal.
pub(crate) type Lexed = (char, bool);

/// Whether `c` can be part of a Rust identifier.
pub(crate) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` as scanned: comments removed and every `#[cfg(test)]` module cut
/// out — the declaration alone, or an inline body to its closing brace. Code
/// after a test module stays (#9121).
pub(super) fn code_only(text: &str) -> String {
    let lexed = lex(text);
    let mut out = String::with_capacity(lexed.len());
    let mut i = 0;
    while i < lexed.len() {
        if let Some(end) = test_module_end(&lexed, i) {
            i = end;
            continue;
        }
        out.push(lexed[i].0);
        i += 1;
    }
    out
}

/// `text` without comments, each char tagged code or literal, so a `//` or a
/// brace inside a string or char literal is never read as syntax (#9121).
pub(crate) fn lex(text: &str) -> Vec<Lexed> {
    let s: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let next = s.get(i + 1).copied();
        let end = match (s[i], next) {
            ('/', Some('/')) => {
                let end = s[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(s.len(), |n| i + n);
                i = end;
                continue;
            }
            ('/', Some('*')) => {
                i = block_comment_end(&s, i);
                out.push((' ', true));
                continue;
            }
            ('"', _) => Some(string_end(&s, i + 1, 0)),
            ('r', _) => raw_string_start(&s, i).map(|(hashes, body)| string_end(&s, body, hashes)),
            ('\'', _) => char_literal_end(&s, i),
            _ => None,
        };
        match end {
            Some(end) => {
                out.extend(s[i..end].iter().map(|&c| (c, false)));
                i = end;
            }
            None => {
                out.push((s[i], true));
                i += 1;
            }
        }
    }
    out
}

/// The index past the `*/` closing the (nestable) block comment at `at`.
fn block_comment_end(s: &[char], at: usize) -> usize {
    let mut depth = 0usize;
    let mut i = at;
    while i + 1 < s.len() {
        match (s[i], s[i + 1]) {
            ('/', '*') => depth += 1,
            ('*', '/') => {
                depth -= 1;
                if depth == 0 {
                    return i + 2;
                }
            }
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    s.len()
}

/// The index past the quote closing a string whose body starts at `body`.
/// `hashes` is the raw-string `#` count; a raw string has no escapes.
fn string_end(s: &[char], body: usize, hashes: usize) -> usize {
    let mut i = body;
    while i < s.len() {
        if hashes == 0 && s[i] == '\\' {
            i += 2;
            continue;
        }
        if s[i] == '"' && (1..=hashes).all(|n| s.get(i + n) == Some(&'#')) {
            return i + 1 + hashes;
        }
        i += 1;
    }
    s.len()
}

/// For a raw string `r#"…"#` (or `br"…"`) at `at`: its `#` count and the index
/// where its body starts. A raw identifier like `r#type` is not one.
fn raw_string_start(s: &[char], at: usize) -> Option<(usize, usize)> {
    let before = at.checked_sub(1).map(|p| s[p]);
    let prefixed = match before {
        Some('b') => !at.checked_sub(2).is_some_and(|p| is_ident(s[p])),
        Some(c) => !is_ident(c),
        None => true,
    };
    if !prefixed {
        return None;
    }
    let hashes = s[at + 1..].iter().take_while(|&&c| c == '#').count();
    (s.get(at + 1 + hashes) == Some(&'"')).then_some((hashes, at + 2 + hashes))
}

/// The index past a char literal at `at`, or `None` for a lifetime.
fn char_literal_end(s: &[char], at: usize) -> Option<usize> {
    if s.get(at + 1) == Some(&'\\') {
        let close = s.iter().skip(at + 3).take(10).position(|&c| c == '\'')?;
        return Some(at + 3 + close + 1);
    }
    (s.get(at + 1).is_some_and(|&c| c != '\'') && s.get(at + 2) == Some(&'\'')).then_some(at + 3)
}

/// When a `#[cfg(test)]` module starts at `at`, the index just past it: past
/// the `;` of a declaration, or past the brace closing an inline body.
fn test_module_end(src: &[Lexed], at: usize) -> Option<usize> {
    let mut i = eat(src, at, "#[cfg(test)]")?;
    loop {
        i = skip_ws(src, i);
        if eat(src, i, "#[").is_none() {
            break;
        }
        i = matching(src, i + 1, '[', ']')?;
    }
    if let Some(after) = eat(src, i, "pub") {
        i = skip_ws(src, after);
        if eat(src, i, "(").is_some() {
            i = skip_ws(src, matching(src, i, '(', ')')?);
        }
    }
    let after_mod = eat(src, i, "mod")?;
    let name = skip_ws(src, after_mod);
    if name == after_mod {
        return None;
    }
    i = name;
    while src.get(i).is_some_and(|&(c, code)| code && is_ident(c)) {
        i += 1;
    }
    if i == name {
        return None;
    }
    i = skip_ws(src, i);
    match src.get(i) {
        Some((';', true)) => Some(i + 1),
        Some(('{', true)) => matching(src, i, '{', '}'),
        _ => None,
    }
}

/// `at + needle.len()` when the code at `at` spells `needle`.
pub(crate) fn eat(src: &[Lexed], at: usize, needle: &str) -> Option<usize> {
    let mut i = at;
    for want in needle.chars() {
        match src.get(i) {
            Some(&(c, true)) if c == want => i += 1,
            _ => return None,
        }
    }
    Some(i)
}

pub(crate) fn skip_ws(src: &[Lexed], mut i: usize) -> usize {
    while src
        .get(i)
        .is_some_and(|&(c, code)| code && c.is_whitespace())
    {
        i += 1;
    }
    i
}

/// The index past the `close` balancing the `open` at `at`. Literal chars
/// never count, so a `"}"` inside a test module cannot end it early.
pub(crate) fn matching(src: &[Lexed], at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &(c, code)) in src.iter().enumerate().skip(at) {
        if code && c == open {
            depth += 1;
        } else if code && c == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i + 1);
            }
        }
    }
    None
}
