//! Python number parsing and formatting the poller's alert text relies on.
//!
//! Why: alert details are dedup keys and owner-facing text, so `f"{x}"`,
//! `int(s)` and `float(s)` must read and print like the prototype (#9171 A3a).
//! What: [`py_float_repr`] is `repr(float)`; [`py_float`] and [`py_int`] parse
//! an environment value the way `float()` and `int()` do for the forms the
//! poller's variables take (surrounding whitespace, a sign, Unicode digits,
//! `_` between digits).
//! Test: `py_float_repr_matches_python`, `py_number_parsers_accept_python_forms`.

use crate::pystr::{ascii_digits, strip};

/// Python's `repr()` of a float: shortest round-trip digits, `.0` on an
/// integral value, and the exponent form outside `1e-4 <= |x| < 1e16`.
pub(crate) fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let a = x.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{x:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let (sign, digits) = match exp.strip_prefix('-') {
            Some(d) => ('-', d),
            None => ('+', exp),
        };
        return format!("{mant}e{sign}{digits:0>2}");
    }
    let s = format!("{x}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// Removes `_` between two digits, as Python's number parsers allow; any
/// other `_` is kept so the parse fails.
fn drop_digit_underscores(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    (0..c.len())
        .filter(|&i| {
            !(c[i] == '_'
                && i > 0
                && c[i - 1].is_ascii_digit()
                && c.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        .map(|i| c[i])
        .collect()
}

/// Python `float(s)` for an environment value; `None` where Python raises.
pub(crate) fn py_float(s: &str) -> Option<f64> {
    drop_digit_underscores(&ascii_digits(strip(s)))
        .parse()
        .ok()
}

/// Python `int(s)` for an environment value; `None` where Python raises (or
/// past `i64`).
pub(crate) fn py_int(s: &str) -> Option<i64> {
    let t = drop_digit_underscores(&ascii_digits(strip(s)));
    let digits = t.strip_prefix(['+', '-']).unwrap_or(&t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_float_repr_matches_python() {
        let cases = [
            (1.5, "1.5"),
            (2.0, "2.0"),
            (-0.0, "-0.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e-5, "1.5e-05"),
            (0.0001, "0.0001"),
            (123.456, "123.456"),
            (f64::INFINITY, "inf"),
        ];
        for (x, want) in cases {
            assert_eq!(py_float_repr(x), want, "{x}");
        }
    }

    #[test]
    fn py_number_parsers_accept_python_forms() {
        assert_eq!(py_float(" 2.5\n"), Some(2.5));
        assert_eq!(py_float("1_000.5"), Some(1000.5));
        assert_eq!(py_float("\u{664}.5"), Some(4.5));
        assert_eq!(py_float("lots"), None);
        assert_eq!(py_float(""), None);
        assert_eq!(py_int(" 1_000_000 "), Some(1_000_000));
        assert_eq!(py_int("-5"), Some(-5));
        assert_eq!(py_int("\u{664}\u{667}"), Some(47));
        for bad in ["2.0", "1__0", "_1", "", "+", "lots"] {
            assert_eq!(py_int(bad), None, "{bad:?}");
        }
    }
}
