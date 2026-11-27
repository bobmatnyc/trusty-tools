//! The Python operations `verify-claim.py` applies to decoded JSON (#12028 P1g).
//!
//! Why: the script reads gh and crates.io replies and event claims with
//! `d.get`, `==`, `len`, `>` and `str()`; a value of an unexpected type
//! either compares the Python way or ends the run with an exception, and the
//! port has to do the same.
//! What: [`PyExc`] is the last line of the traceback an uncaught exception
//! prints. [`dget`] is `d.get(k)` that raises `AttributeError` on a
//! non-dict, [`or_get`] is `(x or {}).get(k)`, [`py_eq`] is `==`,
//! [`py_len`] is `len(n or [])`, [`gt_int`] is `v > n`, [`text`] is a `str`
//! method call that raises on a non-`str`, and [`lower_py`] is `str.lower`.
//! Test: the `tests/replay_verify_claim.rs` recordings, `py_eq_follows_python`.

use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{int_text, truthy, type_name};

/// An uncaught exception: the last line of its traceback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PyExc(pub(crate) String);

/// `AttributeError` for `v.<attr>`.
pub(crate) fn attr_err(v: &PyValue, attr: &str) -> PyExc {
    PyExc(format!(
        "AttributeError: '{}' object has no attribute '{attr}'",
        type_name(v)
    ))
}

/// `v.get(key)`; `None` for a missing key or a JSON `null`.
pub(crate) fn dget<'a>(v: &'a PyValue, key: &str) -> Result<Option<&'a PyValue>, PyExc> {
    match v {
        PyValue::Dict(pairs) => Ok(pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, PyValue::Null))),
        other => Err(attr_err(other, "get")),
    }
}

/// `key in v` for a dict.
pub(crate) fn has_key(v: &PyValue, key: &str) -> bool {
    matches!(v, PyValue::Dict(pairs) if pairs.iter().any(|(k, _)| k == key))
}

/// `(x or {}).get(key)`.
pub(crate) fn or_get<'a>(x: Option<&'a PyValue>, key: &str) -> Result<Option<&'a PyValue>, PyExc> {
    match x {
        Some(v) if truthy(v) => dget(v, key),
        _ => Ok(None),
    }
}

/// `x or default` for an optional value, as `Option` (`None` when falsy and
/// no default applies).
pub(crate) fn or_value(x: Option<&PyValue>) -> Option<&PyValue> {
    x.filter(|v| truthy(v))
}

/// The `str` a `str` method needs, else `AttributeError` for `attr`.
pub(crate) fn text<'a>(v: &'a PyValue, attr: &str) -> Result<&'a str, PyExc> {
    match v {
        PyValue::Str(s) => Ok(s),
        other => Err(attr_err(other, attr)),
    }
}

/// `str.lower()`.
pub(crate) fn lower_py(s: &str) -> String {
    s.to_lowercase()
}

enum Num {
    Int(i64),
    Big(String),
    Float(f64),
}

fn num(v: &PyValue) -> Option<Num> {
    match v {
        PyValue::Bool(b) => Some(Num::Int(i64::from(*b))),
        PyValue::Int(PyInt::Small(n)) => Some(Num::Int(*n)),
        PyValue::Int(PyInt::Big(s)) => Some(Num::Big(s.clone())),
        PyValue::Float(f) => Some(Num::Float(*f)),
        _ => None,
    }
}

fn num_eq(a: &Num, b: &Num) -> bool {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => x == y,
        (Num::Big(x), Num::Big(y)) => x == y,
        (Num::Float(x), Num::Float(y)) => x == y,
        (Num::Int(i), Num::Float(f)) | (Num::Float(f), Num::Int(i)) => {
            // Exact: the float is integral and converts back to the same i64.
            f.fract() == 0.0 && *f == *i as f64 && (*f as i64) == *i
        }
        (Num::Big(s), Num::Float(f)) | (Num::Float(f), Num::Big(s)) => {
            f.is_finite() && f.fract() == 0.0 && format!("{f:.0}") == *s
        }
        (Num::Int(_), Num::Big(_)) | (Num::Big(_), Num::Int(_)) => false,
    }
}

/// `a == b` with Python's rules: numbers compare across `bool`, `int` and
/// `float`; a dict compares by its key set.
pub(crate) fn py_eq(a: &PyValue, b: &PyValue) -> bool {
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return num_eq(&x, &y);
    }
    match (a, b) {
        (PyValue::Null, PyValue::Null) => true,
        (PyValue::Str(x), PyValue::Str(y)) => x == y,
        (PyValue::List(x), PyValue::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (PyValue::Dict(x), PyValue::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && py_eq(v, v2)))
        }
        _ => false,
    }
}

/// `n if isinstance(n, int) else len(n or [])`, as a value.
pub(crate) fn count_or_len(n: Option<&PyValue>) -> Result<PyValue, PyExc> {
    match n {
        Some(v @ (PyValue::Int(_) | PyValue::Bool(_))) => Ok(v.clone()),
        Some(v) if truthy(v) => py_len(v).map(|l| PyValue::Int(PyInt::Small(l))),
        _ => Ok(PyValue::Int(PyInt::Small(0))),
    }
}

/// `len(v)`.
fn py_len(v: &PyValue) -> Result<i64, PyExc> {
    let n = match v {
        PyValue::Str(s) => s.chars().count(),
        PyValue::List(l) => l.len(),
        PyValue::Dict(d) => d.len(),
        other => {
            return Err(PyExc(format!(
                "TypeError: object of type '{}' has no len()",
                type_name(other)
            )));
        }
    };
    Ok(i64::try_from(n).unwrap_or(i64::MAX))
}

/// `v > n` for an `int` `n`.
pub(crate) fn gt_int(v: &PyValue, n: usize) -> Result<bool, PyExc> {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    match num(v) {
        Some(Num::Int(x)) => Ok(x > n),
        Some(Num::Float(f)) => Ok(f > n as f64),
        Some(Num::Big(s)) => Ok(!s.starts_with('-')),
        None => Err(PyExc(format!(
            "TypeError: '>' not supported between instances of '{}' and 'int'",
            type_name(v)
        ))),
    }
}

/// `int(text)` for a run of decimal digits (`str.isdigit()` already true).
pub(crate) fn int_from_digits(digits: &str) -> PyInt {
    let ascii: String = digits
        .chars()
        .filter_map(crate::pystr::digit_value)
        .filter_map(|d| char::from_digit(d, 10))
        .collect();
    let trimmed = ascii.trim_start_matches('0');
    let text = if trimmed.is_empty() { "0" } else { trimmed };
    text.parse::<i64>()
        .map_or_else(|_| PyInt::Big(text.to_owned()), PyInt::Small)
}

/// `str()` of an integer.
pub(crate) fn int_str(i: &PyInt) -> String {
    int_text(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(n: i64) -> PyValue {
        PyValue::Int(PyInt::Small(n))
    }

    #[test]
    fn py_eq_follows_python() {
        // `3 == 3.0`, `1 == True`, `{"a": 1} == {"a": 1.0}`; `"3" != 3`.
        assert!(py_eq(&i(3), &PyValue::Float(3.0)));
        assert!(py_eq(&i(1), &PyValue::Bool(true)));
        assert!(!py_eq(&PyValue::Str("3".into()), &i(3)));
        let a = PyValue::Dict(vec![("a".into(), i(1)), ("b".into(), i(2))]);
        let b = PyValue::Dict(vec![("b".into(), PyValue::Float(2.0)), ("a".into(), i(1))]);
        assert!(py_eq(&a, &b));
        assert!(!py_eq(&PyValue::Null, &i(0)));
        assert_eq!(int_from_digits("0042"), PyInt::Small(42));
        assert_eq!(
            count_or_len(Some(&PyValue::Float(1.5))),
            Err(PyExc(
                "TypeError: object of type 'float' has no len()".into()
            ))
        );
        assert_eq!(
            gt_int(&PyValue::Null, 1).map_err(|e| e.0),
            Err("TypeError: '>' not supported between instances of 'NoneType' and 'int'".into())
        );
    }
}
