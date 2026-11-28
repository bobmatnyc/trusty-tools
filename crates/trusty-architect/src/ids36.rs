//! Base-34 id digits: the port of `ids36.py` (#12029 P2a).
//!
//! Why: dashboard and Intent Ledger ids end in two base-34 digits (`0-9` and
//! `a-z` without `l` and `o`, which read as `1` and `0`), so sort and "next"
//! arithmetic must go through one definition, never hex. 1,156 values per
//! prefix, 1,155 issuable: `00` is never issued.
//! What: [`DIGITS`], [`BASE`], [`SPACE`], [`DIGIT_RE`], [`enc`] (a number to
//! two digits), [`dec`] (two digits to a number), [`next_digits`] (the digits
//! after, skipping `00` and wrapping `zz` to `01`) and [`run`], the script's
//! `next <digits>` command line. Errors carry the script's `ValueError` text.
//! Pure: no clock, no filesystem.
//! Test: `ids36_recordings_replay_clean_against_the_port`,
//! `enc_dec_bounds_match_the_script`, `shipped_ids36_next_and_usage`.
// See #12029

use crate::pystr::py_repr;
use crate::spec_drift::Outcome;

/// The digits in order: `0-9` and `a-z` without `l` and `o`.
pub const DIGITS: &str = "0123456789abcdefghijkmnpqrstuvwxyz";
/// The radix (34).
pub const BASE: usize = 34;
/// Values per prefix (`34 * 34`).
pub const SPACE: usize = BASE * BASE;
/// The regex fragment for the two digits.
pub const DIGIT_RE: &str = "[0-9a-km-np-z]{2}";

/// The script's usage line (its `sys.exit` text).
const USAGE: &str =
    "usage: ids36.py next <two digits>   (0-9 and a-z without o and l; e.g. 0f -> 0g, 0z -> 10)";

/// A `ValueError` of `ids36.py`; the text is the script's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ids36Error {
    /// `enc` of a number outside `0..SPACE`.
    #[error("id number out of range: {0}")]
    OutOfRange(i64),
    /// `dec` of anything but two lowercase base-34 digits (the field is the
    /// value's Python `repr`).
    #[error("not two base-34 digits: {0}")]
    NotDigits(String),
}

/// The value of one digit.
fn digit(c: char) -> Option<usize> {
    DIGITS.chars().position(|d| d == c)
}

/// Two base-34 digits for `0 <= n < SPACE`.
///
/// # Errors
/// [`Ids36Error::OutOfRange`] for any other `n`.
pub fn enc(n: i64) -> Result<String, Ids36Error> {
    let i = usize::try_from(n)
        .ok()
        .filter(|i| *i < SPACE)
        .ok_or(Ids36Error::OutOfRange(n))?;
    let d = DIGITS.as_bytes();
    Ok(format!(
        "{}{}",
        char::from(d[i / BASE]),
        char::from(d[i % BASE])
    ))
}

/// The value of two lowercase base-34 digits.
///
/// The script matches `^[0-9a-km-np-z]{2}$`, and Python's `$` also matches
/// before one final `\n`, so `"0f\n"` is 15 as in the script.
///
/// # Errors
/// [`Ids36Error::NotDigits`] for anything else (uppercase, `o`, `l`, the
/// wrong length).
pub fn dec(s: &str) -> Result<usize, Ids36Error> {
    let body = s.strip_suffix('\n').unwrap_or(s);
    let mut chars = body.chars();
    match (
        chars.next().and_then(digit),
        chars.next().and_then(digit),
        chars.next(),
    ) {
        (Some(hi), Some(lo), None) => Ok(hi * BASE + lo),
        _ => Err(Ids36Error::NotDigits(py_repr(s))),
    }
}

/// The digits after `last` in base-34 order, skipping `00` and wrapping
/// `zz` to `01`.
///
/// # Errors
/// [`dec`]'s error for `last`.
pub fn next_digits(last: &str) -> Result<String, Ids36Error> {
    let n = dec(last)? % (SPACE - 1) + 1;
    // `n` is in `1..SPACE`, so `enc` cannot refuse it.
    enc(n as i64)
}

/// The script's command line: `next <two digits>`; anything else prints the
/// usage line and exits 1, as `sys.exit(text)` does.
pub fn run(args: &[String]) -> Outcome {
    let (stdout, stderr, code) = match args {
        [cmd, last] if cmd == "next" => match next_digits(last) {
            Ok(d) => (format!("{d}\n"), String::new(), 0),
            Err(e) => (String::new(), format!("ids36: {e}\n"), 1),
        },
        _ => (String::new(), format!("{USAGE}\n"), 1),
    };
    Outcome {
        stdout,
        stderr,
        code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enc_dec_bounds_match_the_script() {
        assert_eq!(BASE, 34);
        assert_eq!(SPACE, 1156);
        assert_eq!(enc(0).as_deref(), Ok("00"));
        assert_eq!(enc(1155).as_deref(), Ok("zz"));
        assert_eq!(enc(1156), Err(Ids36Error::OutOfRange(1156)));
        assert_eq!(enc(-1), Err(Ids36Error::OutOfRange(-1)));
        assert_eq!(dec("zz"), Ok(1155));
        assert_eq!(dec("0f\n"), Ok(15));
        for bad in ["0o", "0l", "0F", "0", "000", "0f\n\n", "\n0f", ""] {
            assert!(dec(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            dec("0O").map_err(|e| e.to_string()),
            Err("not two base-34 digits: '0O'".to_owned())
        );
        assert_eq!(next_digits("zz").as_deref(), Ok("01"));
        assert_eq!(next_digits("00").as_deref(), Ok("01"));
        assert_eq!(next_digits("0z").as_deref(), Ok("10"));
        assert_eq!(next_digits("zy").as_deref(), Ok("zz"));
        assert_eq!(next_digits("0j").as_deref(), Ok("0k"));
    }
}
