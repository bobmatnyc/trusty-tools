//! The secret-shape test of `ilio check`.
//!
//! Why: an IL block records where a secret lives, never its value. `ilio.py`
//! rejects a field matching
//! `(?i)(ghp_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|xox[abp]-[A-Za-z0-9-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY|(?:password|secret|token|api[_-]?key)\s*[:=]\s*\S{8,})`.
//! What: [`looks_secret`] runs the six alternatives by hand, anywhere in the text,
//! caselessly, with Python's `\s` and `\S`.
//! Test: `secret_shapes_match`, `near_misses_do_not`.

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
    let Some(mut j) = after else { return false };
    j += run(t, j, is_space);
    if !matches!(t.get(j), Some(':' | '=')) {
        return false;
    }
    j += 1;
    j += run(t, j, is_space);
    run(t, j, |c| !is_space(c)) >= 8
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
}
