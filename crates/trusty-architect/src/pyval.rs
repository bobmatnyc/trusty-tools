//! Python semantics over a decoded JSON value, for the task checker ports (#12028 P1d).
//!
//! Why: the task checkers print `str()`, `repr()` and `json.dumps()` of
//! whatever a ledger or dashboard document holds, and test values for
//! truth the Python way; a port must print the same text for every shape.
//! What: [`get`] is `d.get(k)` on a dict (`None` for any other value),
//! [`truthy`] is `bool(v)`, [`repr`] / [`py_str`] are `repr()` / `str()`,
//! [`type_name`] is `type(v).__name__`, [`dumps`] is `json.dumps` with an
//! optional indent and either `ensure_ascii`, and [`cmp_int`] orders two
//! integers of any size. Recursion follows the value's nesting, which
//! [`crate::pyjson_load`] bounds at its `MAX_DEPTH`.
//! Known gap: `repr()` of a string keeps non-printable non-ASCII characters
//! as they are ([`crate::pystr::py_repr`]).
//! Test: the recordings in `tests/replay_tasks.rs`, `pyval_text_matches_python`.

use std::cmp::Ordering;
use std::fmt::Write as _;

use crate::poller::pyfmt::py_float_repr;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::py_repr;

/// `d.get(key)` when `v` is a dict; `None` for a missing key or a non-dict.
pub(crate) fn get<'a>(v: &'a PyValue, key: &str) -> Option<&'a PyValue> {
    match v {
        PyValue::Dict(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// `bool(v)`.
pub(crate) fn truthy(v: &PyValue) -> bool {
    match v {
        PyValue::Null => false,
        PyValue::Bool(b) => *b,
        PyValue::Int(PyInt::Small(n)) => *n != 0,
        PyValue::Int(PyInt::Big(_)) => true,
        PyValue::Float(f) => *f != 0.0,
        PyValue::Str(s) => !s.is_empty(),
        PyValue::List(l) => !l.is_empty(),
        PyValue::Dict(d) => !d.is_empty(),
    }
}

/// `bool(d.get(key))` with a missing key false.
pub(crate) fn has(v: &PyValue, key: &str) -> bool {
    get(v, key).is_some_and(truthy)
}

/// True for a `str` equal to `s`.
pub(crate) fn is_str(v: Option<&PyValue>, s: &str) -> bool {
    matches!(v, Some(PyValue::Str(x)) if x == s)
}

/// `isinstance(v, int)`: an integer or a bool.
pub(crate) fn is_int(v: &PyValue) -> bool {
    matches!(v, PyValue::Int(_) | PyValue::Bool(_))
}

/// `type(v).__name__`.
pub(crate) fn type_name(v: &PyValue) -> &'static str {
    match v {
        PyValue::Null => "NoneType",
        PyValue::Bool(_) => "bool",
        PyValue::Int(_) => "int",
        PyValue::Float(_) => "float",
        PyValue::Str(_) => "str",
        PyValue::List(_) => "list",
        PyValue::Dict(_) => "dict",
    }
}

/// The decimal text of an integer.
pub(crate) fn int_text(i: &PyInt) -> String {
    match i {
        PyInt::Small(n) => n.to_string(),
        PyInt::Big(s) => s.clone(),
    }
}

/// Orders two integers of any size by value.
pub(crate) fn cmp_int(a: &PyInt, b: &PyInt) -> Ordering {
    match (a, b) {
        (PyInt::Small(x), PyInt::Small(y)) => x.cmp(y),
        _ => {
            let (ta, tb) = (int_text(a), int_text(b));
            let neg = |t: &str| t.starts_with('-');
            let mag = |t: &str| {
                let d = t.trim_start_matches(['-', '+']).trim_start_matches('0');
                (d.len(), d.to_owned())
            };
            match (neg(&ta), neg(&tb)) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (both_neg, _) => {
                    let ord = mag(&ta).cmp(&mag(&tb));
                    if both_neg { ord.reverse() } else { ord }
                }
            }
        }
    }
}

/// `repr(v)`.
pub(crate) fn repr(v: &PyValue) -> String {
    let mut out = String::new();
    write_repr(v, &mut out);
    out
}

fn write_repr(v: &PyValue, out: &mut String) {
    match v {
        PyValue::Null => out.push_str("None"),
        PyValue::Bool(true) => out.push_str("True"),
        PyValue::Bool(false) => out.push_str("False"),
        PyValue::Int(i) => out.push_str(&int_text(i)),
        PyValue::Float(f) => out.push_str(&py_float_repr(*f)),
        PyValue::Str(s) => out.push_str(&py_repr(s)),
        PyValue::List(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                write_repr(item, out);
            }
            out.push(']');
        }
        PyValue::Dict(pairs) => {
            out.push('{');
            for (n, (k, item)) in pairs.iter().enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                out.push_str(&py_repr(k));
                out.push_str(": ");
                write_repr(item, out);
            }
            out.push('}');
        }
    }
}

/// `str(v)`.
pub(crate) fn py_str(v: &PyValue) -> String {
    match v {
        PyValue::Str(s) => s.clone(),
        other => repr(other),
    }
}

/// `json.dumps(v, indent=indent, ensure_ascii=ensure_ascii)` with the default
/// separators (`", "` and `": "`, or `","` at line ends with an indent).
pub(crate) fn dumps(v: &PyValue, indent: Option<usize>, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_json(v, indent, ensure_ascii, 0, &mut out);
    out
}

fn newline(indent: Option<usize>, level: usize, out: &mut String) {
    if let Some(n) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * level));
    }
}

fn write_json(v: &PyValue, indent: Option<usize>, ascii: bool, level: usize, out: &mut String) {
    let item_sep = if indent.is_some() { "," } else { ", " };
    match v {
        PyValue::Null => out.push_str("null"),
        PyValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PyValue::Int(i) => out.push_str(&int_text(i)),
        PyValue::Float(f) if f.is_nan() => out.push_str("NaN"),
        PyValue::Float(f) if f.is_infinite() => {
            out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" });
        }
        PyValue::Float(f) => out.push_str(&py_float_repr(*f)),
        PyValue::Str(s) => json_str(s, ascii, out),
        PyValue::List(items) if items.is_empty() => out.push_str("[]"),
        PyValue::Dict(pairs) if pairs.is_empty() => out.push_str("{}"),
        PyValue::List(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push_str(item_sep);
                }
                newline(indent, level + 1, out);
                write_json(item, indent, ascii, level + 1, out);
            }
            newline(indent, level, out);
            out.push(']');
        }
        PyValue::Dict(pairs) => {
            out.push('{');
            for (n, (k, item)) in pairs.iter().enumerate() {
                if n > 0 {
                    out.push_str(item_sep);
                }
                newline(indent, level + 1, out);
                json_str(k, ascii, out);
                out.push_str(": ");
                write_json(item, indent, ascii, level + 1, out);
            }
            newline(indent, level, out);
            out.push('}');
        }
    }
}

/// A JSON string literal as `json.dumps` writes it.
pub(crate) fn json_str(s: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if ascii && c > '~' => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson_load::loads;

    // Expected values printed by CPython 3.14.8.
    #[test]
    fn pyval_text_matches_python() {
        let v = loads(r#"{"a": [1, 2.5, null, true, "it's"], "b": {"c": "é\u007f😀"}, "n": NaN}"#)
            .expect("json");
        assert_eq!(
            repr(&v),
            "{'a': [1, 2.5, None, True, \"it's\"], 'b': {'c': '\u{e9}\u{7f}\u{1f600}'}, 'n': nan}"
                .replace("\u{7f}", "\\x7f")
        );
        assert_eq!(
            dumps(&v, None, false),
            "{\"a\": [1, 2.5, null, true, \"it's\"], \"b\": {\"c\": \"\u{e9}\u{7f}\u{1f600}\"}, \"n\": NaN}"
        );
        assert_eq!(
            dumps(&v, Some(1), true),
            "{\n \"a\": [\n  1,\n  2.5,\n  null,\n  true,\n  \"it's\"\n ],\n \"b\": {\n  \"c\": \"\\u00e9\\u007f\\ud83d\\ude00\"\n },\n \"n\": NaN\n}"
        );
        let big = loads("[-123456789012345678901, 99999999999999999999]").expect("json");
        let PyValue::List(ints) = big else {
            panic!("a list")
        };
        let (PyValue::Int(a), PyValue::Int(b)) = (&ints[0], &ints[1]) else {
            panic!("ints")
        };
        assert_eq!(cmp_int(a, b), Ordering::Less);
        assert_eq!(cmp_int(b, &PyInt::Small(5)), Ordering::Greater);
        assert_eq!(cmp_int(a, &PyInt::Small(-5)), Ordering::Less);
        assert!(!truthy(&loads("0.0").expect("json")));
        assert!(truthy(&loads("NaN").expect("json")));
    }
}
