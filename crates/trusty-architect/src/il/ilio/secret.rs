//! The secret-shape test of `ilio check` and of the IL packet path.
//!
//! Why: an IL block records where a secret lives, never its value. `ilio.py`
//! rejects a field matching
//! `(?i)(ghp_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|xox[abp]-[A-Za-z0-9-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY|(?:password|secret|token|api[_-]?key)\s*[:=]\s*\S{8,})`.
//! The crate once had two ports of it (this one and `ledger/il/secret.rs`);
//! #10571 keeps one, the union of what both caught.
//! What: [`looks_secret`] runs the six alternatives by hand, anywhere in the text,
//! caselessly (`re.IGNORECASE` folding), and tries the assignment alternative
//! under both whitespace classes the two ports used: Python's `\s`/`\S` and
//! Rust's `char::is_whitespace`.
//! Test: `secret_shapes_match`, `near_misses_do_not`,
//! `merged_detector_catches_every_input_either_old_detector_caught`.

use crate::pystr::{contains_ci, is_space, starts_with_ci};

fn run(t: &[char], from: usize, class: impl Fn(char) -> bool) -> usize {
    t[from.min(t.len())..]
        .iter()
        .take_while(|c| class(**c))
        .count()
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// `\s*[:=]\s*\S{8,}` from `j`, with `space` as the `\s` class.
fn assigned(t: &[char], mut j: usize, space: fn(char) -> bool) -> bool {
    j += run(t, j, space);
    if !matches!(t.get(j), Some(':' | '=')) {
        return false;
    }
    j += 1;
    j += run(t, j, space);
    run(t, j, |c| !space(c)) >= 8
}

fn assignment(t: &[char], i: usize) -> bool {
    let mut after = None;
    for kw in [
        "password", "secret", "token", "api_key", "api-key", "apikey",
    ] {
        if starts_with_ci(&t[i..], kw) {
            after = Some(i + kw.chars().count());
            break;
        }
    }
    let Some(j) = after else { return false };
    // #10571: Python `\s` (the ilio check port) or Rust whitespace (the IL
    // packet port); each caught a `\x1c`-`\x1f` case the other missed.
    assigned(t, j, is_space) || assigned(t, j, char::is_whitespace)
}

/// True when `text` carries a secret-shaped value.
pub fn looks_secret(text: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    (0..t.len()).any(|i| {
        let rest = &t[i..];
        (starts_with_ci(rest, "ghp_") && run(&t, i + 4, alnum) >= 20)
            || (starts_with_ci(rest, "sk-")
                && run(&t, i + 3, |c| alnum(c) || c == '_' || c == '-') >= 20)
            || (starts_with_ci(rest, "akia") && run(&t, i + 4, alnum) >= 16)
            || (starts_with_ci(rest, "xox")
                && t.get(i + 3)
                    .is_some_and(|c| matches!(c.to_ascii_lowercase(), 'a' | 'b' | 'p'))
                && t.get(i + 4) == Some(&'-')
                && run(&t, i + 5, |c| alnum(c) || c == '-') >= 10)
            || (starts_with_ci(rest, "-----begin ")
                && contains_ci(
                    &t[i + 11..i + 11 + run(&t, i + 11, |c| c.is_ascii_alphabetic() || c == ' ')],
                    "private key",
                ))
            || assignment(&t, i)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_shapes_match() {
        for yes in [
            "ghp_abcdefghijklmnopqrstuv",
            "GHP_abcdefghijklmnopqrstuv",
            "sk-abcdefghij_klmnopqrst-uv",
            "key AKIAABCDEFGHIJKLMNOP end",
            "xoxb-1234567890-abc",
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY",
            "password: hunter2hunter2",
            "API_KEY=abcdefghi",
            "api-key =  abcdefgh",
            "token:\tabcdefgh1",
        ] {
            assert!(looks_secret(yes), "{yes}");
        }
    }

    #[test]
    fn near_misses_do_not() {
        for no in [
            "ghp_short",
            "sk-short",
            "AKIAshort",
            "xoxz-1234567890",
            "xoxb-123",
            "-----BEGIN CERTIFICATE-----",
            "password: short",
            "the token is stored in vault",
            "secret",
            "",
        ] {
            assert!(!looks_secret(no), "{no}");
        }
    }

    /// #10571: every alternative of both old ports, plus the inputs only one
    /// of them caught, must still be caught by the one detector left.
    #[test]
    fn merged_detector_catches_every_input_either_old_detector_caught() {
        for (why, yes) in [
            // Both ports: one input per alternative and keyword spelling.
            ("ghp_", "x ghp_ABCDEFGHIJ0123456789 y"),
            ("sk-", "used sk-abcdefghij0123456789"),
            ("sk- with _ and -", "sk-abc_def-ghi_jkl-mno_pq"),
            ("AKIA", "AKIA0123456789ABCDEF"),
            ("xoxa", "xoxa-0123456789"),
            ("xoxb", "XOXB-abc-def-ghij"),
            ("xoxp", "xoxp-0123456789"),
            ("private key", "-----begin rsa private key"),
            ("private key, ec", "-----BEGIN EC PRIVATE KEY-----"),
            ("password", "act: rotated, password: hunter2hunter2"),
            ("secret", "secret=abcdefgh"),
            ("token", "TOKEN : abcdefgh"),
            ("api_key", "api_key=abcdefgh"),
            ("api-key", "act: set API-KEY = abcdefgh1234"),
            ("apikey", "apikey:abcdefgh"),
            // Only the old ilio-check port (Python `\s`, `re` folding).
            ("\\x1c as \\s", "password\u{1c}: abcdefgh"),
            ("\\x1f before the =", "token\u{1f}=abcdefgh"),
            ("Kelvin sign folds to k", "to\u{212a}en: abcdefgh"),
            ("long s folds to s", "\u{17f}ecret: abcdefgh"),
            ("dotless i folds to i", "AP\u{131}KEY=abcdefgh"),
            // Only the old IL packet port (Rust whitespace: `\x1c` is `\S`).
            ("\\x1c inside the value", "password: abc\u{1c}defgh"),
            ("\\x1d inside the value", "secret=ab\u{1d}cdefgh"),
        ] {
            assert!(looks_secret(yes), "{why}: {yes:?}");
        }
    }
}
