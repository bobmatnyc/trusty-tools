//! Python `str` and `re` character semantics the ported scripts rely on.
//!
//! Why: the A2 ports must give byte-identical output to the prototype Python
//! (#9171). Rust's `lines`, `trim` and `split_whitespace` disagree with
//! Python's `splitlines`, `strip` and `split` on several characters, and the
//! crate takes no `regex` dependency, so the scripts' patterns are matched by
//! hand on top of these classes.
//! What: Python's whitespace (`str.isspace`, `re` `\s`), word (`\w`) and
//! decimal (`\d`, every Unicode `Nd` digit) classes, `splitlines`, the strip
//! family, `split()` with no separator, and `re.IGNORECASE` comparison against
//! ASCII patterns.
//! Known gap: `\w` follows Rust's `Alphabetic`, which also admits combining
//! marks Python's `isalpha` rejects.
//! Test: `splitlines_matches_python_boundaries`, `strip_uses_python_space`,
//! `is_digit_matches_python_re_digit_for_every_code_point`.

use crate::decimal_table::DIGIT_ZEROS;

/// Python `str.isspace` for one character (also `re`'s `\s` on `str`).
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `re`'s `\w` on `str`: a letter, a number, or `_`.
pub(crate) fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `re`'s `\d` on `str`: any Unicode decimal digit (`Nd`).
///
/// Why: the parity claim covers non-ASCII digits too (#9171, Architect ruling
/// Q6): Python reads `ctx` followed by Arabic-Indic four-seven `%` as 47.
/// What: a binary search over `DIGIT_ZEROS`, the generated table of the first
/// code point of every ten-digit run.
/// Test: `is_digit_matches_python_re_digit_for_every_code_point`.
pub(crate) fn is_digit(c: char) -> bool {
    digit_value(c).is_some()
}

/// The value (0-9) of a `\d` character, as Python's `int()` reads it.
pub(crate) fn digit_value(c: char) -> Option<u32> {
    let cp = u32::from(c);
    let idx = DIGIT_ZEROS.partition_point(|z| *z <= cp).checked_sub(1)?;
    let offset = cp - DIGIT_ZEROS[idx];
    (offset < 10).then_some(offset)
}

/// Python `int(s)` for a run of `\d` characters; `None` past `u64` or on a
/// character that is not a digit.
pub(crate) fn parse_digits(s: &[char]) -> Option<u64> {
    s.iter().try_fold(0u64, |acc, c| {
        acc.checked_mul(10)?
            .checked_add(u64::from(digit_value(*c)?))
    })
}

/// `s` with every `\d` character replaced by its ASCII digit, so Rust's
/// number parsers read it the way Python's `int()` and `float()` do.
pub(crate) fn ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| match digit_value(c) {
            Some(d) => char::from(b'0' + d as u8),
            None => c,
        })
        .collect()
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

    /// `digits.jsonl` lists, as `[first, last]` ranges, every code point
    /// Python's `re` `\d` matches, scanned by `regen_a3.py` independently of
    /// the table `is_digit` reads.
    #[test]
    fn is_digit_matches_python_re_digit_for_every_code_point()
    -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a3/digits.jsonl");
        let mut ranges = Vec::new();
        for line in std::fs::read_to_string(path)?.lines() {
            let r: [u32; 2] = serde_json::from_str(line)?;
            ranges.push(r);
        }
        let python = |cp: u32| ranges.iter().any(|[lo, hi]| (*lo..=*hi).contains(&cp));
        let mut count = 0;
        for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
            assert_eq!(is_digit(c), python(u32::from(c)), "U+{:04X}", u32::from(c));
            count += usize::from(is_digit(c));
        }
        assert_eq!(count, DIGIT_ZEROS.len() * 10);
        assert_eq!(parse_digits(&['\u{664}', '\u{667}']), Some(47));
        assert_eq!(digit_value('\u{1D7FF}'), Some(9));
        Ok(())
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
