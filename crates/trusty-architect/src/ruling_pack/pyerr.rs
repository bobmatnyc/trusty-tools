//! Python exception texts the `ruling_pack.py` port reproduces (#12028 P1i).
//!
//! Why: a backend reply of an unexpected shape makes the script print the
//! exception's `str()` in the pack (`recall failed: 'result'`), or end in a
//! traceback whose last line the port prints; both must match byte for byte.
//! What: [`subscript`] is `v[key]` and [`dict_get`] is `v.get(key)`, each
//! failing with the text Python raises; [`PyErr`] keeps the class for a
//! traceback line. [`universal`] is text-mode newline translation.
//! Test: `subscript_errors_match_python`, the `recall-*` recordings.

use crate::pyjson_load::PyValue;
use crate::pystr::py_repr;
use crate::pyval::{get, type_name};

/// A raised Python exception: its class and its `str()`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{class}: {msg}")]
pub(crate) struct PyErr {
    /// The class name a traceback's last line starts with.
    pub(crate) class: String,
    /// `str(e)`.
    pub(crate) msg: String,
}

impl PyErr {
    pub(crate) fn new(class: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            class: class.into(),
            msg: msg.into(),
        }
    }

    /// From a traceback line `Class: message`.
    pub(crate) fn from_line(line: &str) -> Self {
        match line.split_once(": ") {
            Some((class, msg)) => Self::new(class, msg),
            None => Self::new("OSError", line),
        }
    }
}

/// A subscript: `v["k"]` or `v[0]`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Key<'a> {
    Str(&'a str),
    Idx(usize),
}

/// `v[key]`, with Python's `KeyError`, `IndexError` and `TypeError` texts.
pub(crate) fn subscript(v: &PyValue, key: Key<'_>) -> Result<PyValue, PyErr> {
    let not_sub = |v: &PyValue| {
        PyErr::new(
            "TypeError",
            format!("'{}' object is not subscriptable", type_name(v)),
        )
    };
    match (v, key) {
        (PyValue::Dict(_), Key::Str(k)) => get(v, k)
            .cloned()
            .ok_or_else(|| PyErr::new("KeyError", py_repr(k))),
        (PyValue::Dict(_), Key::Idx(i)) => Err(PyErr::new("KeyError", i.to_string())),
        (PyValue::List(items), Key::Idx(i)) => items
            .get(i)
            .cloned()
            .ok_or_else(|| PyErr::new("IndexError", "list index out of range")),
        (PyValue::List(_), Key::Str(_)) => Err(PyErr::new(
            "TypeError",
            "list indices must be integers or slices, not str",
        )),
        (PyValue::Str(s), Key::Idx(i)) => s
            .chars()
            .nth(i)
            .map(|c| PyValue::Str(c.to_string()))
            .ok_or_else(|| PyErr::new("IndexError", "string index out of range")),
        (PyValue::Str(_), Key::Str(_)) => Err(PyErr::new(
            "TypeError",
            "string indices must be integers, not 'str'",
        )),
        (other, _) => Err(not_sub(other)),
    }
}

/// `v.get(key)`, or the `AttributeError` a non-dict raises.
pub(crate) fn dict_get<'a>(v: &'a PyValue, key: &str) -> Result<Option<&'a PyValue>, PyErr> {
    match v {
        PyValue::Dict(_) => Ok(get(v, key)),
        other => Err(no_attr(other, "get")),
    }
}

/// `'<type>' object has no attribute '<attr>'`.
pub(crate) fn no_attr(v: &PyValue, attr: &str) -> PyErr {
    PyErr::new(
        "AttributeError",
        format!("'{}' object has no attribute '{attr}'", type_name(v)),
    )
}

/// Text-mode newline translation: `\r\n` and a lone `\r` read as `\n`.
pub(crate) fn universal(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// `str(OSError)` with no file name: `[Errno N] <message>`.
pub(crate) fn errno_only(e: &std::io::Error) -> String {
    let msg = e.to_string();
    let msg = msg.split(" (os error").next().unwrap_or(&msg).to_owned();
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {msg}"),
        None => msg,
    }
}

/// `str(TimeoutExpired)` for `argv` and `secs`.
pub(crate) fn timed_out(argv: &[String], secs: u64) -> String {
    let items: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
    format!(
        "Command '[{}]' timed out after {secs} seconds",
        items.join(", ")
    )
}
