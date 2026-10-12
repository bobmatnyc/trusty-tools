//! Python `str` and `re` character semantics the ported scripts rely on.
//!
//! Why: the A2 ports must give byte-identical output to the prototype Python
//! (#9171). Rust's `lines`, `trim` and `split_whitespace` disagree with
//! Python's `splitlines`, `strip` and `split` on several characters, and the
//! crate takes no `regex` dependency, so the scripts' patterns are matched by
//! hand on top of these classes.
//! What: Python's whitespace (`str.isspace`, `re` `\s`), word (`\w`) and
//! decimal (`\d`, ASCII only) classes, `splitlines`, the strip family, `split()`
//! with no separator, and `re.IGNORECASE` comparison against ASCII patterns.
//! Known gaps: `\d` here is ASCII-only, and `\w` follows Rust's `Alphabetic`,
//! which also admits combining marks Python's `isalpha` rejects.
//! Test: `splitlines_matches_python_boundaries`, `strip_uses_python_space`.

/// Python `str.isspace` for one character (also `re`'s `\s` on `str`).
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `re`'s `\w` on `str`: a letter, a number, or `_`.
pub(crate) fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `re`'s `\d`, restricted to ASCII digits (see the module note).
pub(crate) fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// Python `str.splitlines()` with no `keepends`.
pub(crate) fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        out.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' && chars.peek().map(|&(_, n)| n) == Some('\n') {
            chars.next();
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Python `str.strip()`.
pub(crate) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Python `str.lstrip()`.
pub(crate) fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// Python `str.rstrip()`.
pub(crate) fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// Python `" ".join(s.split())`: whitespace runs collapsed to one space.
pub(crate) fn collapse_space(s: &str) -> String {
    s.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One text character equals one lowercase ASCII pattern character under
/// `re.IGNORECASE` (simple case folding, plus `re`'s own fixes for `ı`, `İ`,
/// `ſ` and the Kelvin sign).
fn fold_eq(c: char, p: char) -> bool {
    if c == p {
        return true;
    }
    let special = match c {
        '\u{130}' | '\u{131}' => Some('i'),
        '\u{17f}' => Some('s'),
        '\u{212a}' => Some('k'),
        _ => None,
    };
    if let Some(s) = special {
        return s == p;
    }
    let mut lower = c.to_lowercase();
    lower.next() == Some(p) && lower.next().is_none()
}

/// True when `pattern` (lowercase ASCII) matches `text` at the start, caselessly.
pub(crate) fn starts_with_ci(text: &[char], pattern: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    text.len() >= pat.len() && text.iter().zip(&pat).all(|(&c, &p)| fold_eq(c, p))
}

/// True when `pattern` (lowercase ASCII) occurs anywhere in `text`, caselessly.
pub(crate) fn contains_ci(text: &[char], pattern: &str) -> bool {
    (0..text.len()).any(|i| starts_with_ci(&text[i..], pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python_boundaries() {
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("a\n"), ["a"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(
            splitlines("a\r\nb\rc\u{2028}d\u{1e}e"),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(splitlines("a\u{85}\u{0c}"), ["a", ""]);
    }

    #[test]
    fn strip_uses_python_space() {
        assert_eq!(strip("\u{1f}\u{a0} x \u{3000}"), "x");
        assert_eq!(collapse_space(" a \u{1c} b\n\tc "), "a b c");
        assert!(contains_ci(
            &"USAGE LIMIT".chars().collect::<Vec<_>>(),
            "limit"
        ));
        assert!(contains_ci(
            &"\u{17f}ecret".chars().collect::<Vec<_>>(),
            "secret"
        ));
        assert!(!contains_ci(&"lim".chars().collect::<Vec<_>>(), "limit"));
    }
}
