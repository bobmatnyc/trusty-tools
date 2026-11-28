//! The Python semantics `spec_drift_check.py` leans on: dict access on JSON,
//! `str()`/`repr()` of values, text-mode reads, exception text and `glob`.
//!
//! Why: the port must print what the script prints, error cases included
//! (#12028 AC1), and the script's messages embed `KeyError`, `OSError` and
//! `repr()` text.
//! What: [`Raise`] is a Python exception the script either catches per check
//! ([`Raise::Caught`]) or does not ([`Raise::Crash`]); [`key`], [`get`],
//! [`list`] and [`text`] are `m["k"]`, `m.get("k")`, iteration and use as a
//! `str`; [`read`] is `open(path, encoding="utf-8").read()` with universal
//! newlines; [`visible`] and [`md_tree`] are the `glob` patterns the checks use.
//! Test: `tests/replay_spec_drift_check.rs` (every recorded case).

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::pystr::py_repr;

/// One Python exception: its class name and `str(exc)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PyExc {
    pub(crate) class: String,
    pub(crate) msg: String,
}

/// An exception raised inside a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Raise {
    /// `OSError`, `KeyError` or `ValueError`: the check reports `check error`.
    Caught(PyExc),
    /// Anything else (`TypeError`, `re.error`, the store's `StoreError`):
    /// the script would stop with a traceback; the text names the exception.
    Crash(String),
}

impl From<PyExc> for Raise {
    fn from(e: PyExc) -> Self {
        Self::Caught(e)
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn type_error(what: &str, v: &Value) -> Raise {
    Raise::Crash(format!("TypeError: {what} on a {} value", kind(v)))
}

/// `v[k]` on a JSON object.
pub(super) fn key<'a>(v: &'a Value, k: &str) -> Result<&'a Value, Raise> {
    match v {
        Value::Object(m) => m.get(k).ok_or_else(|| {
            Raise::Caught(PyExc {
                class: "KeyError".into(),
                msg: py_repr(k),
            })
        }),
        other => Err(type_error("subscript", other)),
    }
}

/// `v.get(k)`.
pub(super) fn get<'a>(v: &'a Value, k: &str) -> Result<Option<&'a Value>, Raise> {
    match v {
        Value::Object(m) => Ok(m.get(k)),
        other => Err(Raise::Crash(format!(
            "AttributeError: '{}' object has no attribute 'get'",
            kind(other)
        ))),
    }
}

/// `for x in v` over a JSON array.
pub(super) fn list(v: &Value) -> Result<&[Value], Raise> {
    match v {
        Value::Array(a) => Ok(a),
        other => Err(type_error("iteration", other)),
    }
}

/// `v` used where the script needs a `str` (a path, a pattern).
pub(super) fn text(v: &Value) -> Result<&str, Raise> {
    v.as_str().ok_or_else(|| type_error("str use", v))
}

/// Python truthiness of an optional value (`None` when the key is absent).
pub(super) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `v` as a Python number (a `bool` is an `int`); `None` for anything else.
pub(super) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `repr(float)` for the values a manifest holds.
fn float_repr(f: f64) -> String {
    if !f.is_finite() {
        return if f.is_nan() {
            "nan"
        } else if f > 0.0 {
            "inf"
        } else {
            "-inf"
        }
        .into();
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let (sign, digits) = exp.strip_prefix('-').map_or(("+", exp), |d| ("-", d));
        return format!("{mant}e{sign}{digits:0>2}");
    }
    let s = format!("{f}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// `str(v)`.
pub(super) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// `repr(v)` (object keys in `serde_json`'s order).
pub(super) fn py_repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(f64::NAN)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr(s),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(py_repr_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `repr()` of a list of strings.
pub(super) fn list_repr(items: &[&str]) -> String {
    let parts: Vec<String> = items.iter().map(|s| py_repr(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// The `OSError` subclass and `str()` Python raises for `e` on `path`.
pub(crate) fn os_error(path: &Path, e: &std::io::Error) -> PyExc {
    let code = e.raw_os_error().unwrap_or(0);
    let class = match code {
        libc::ENOENT => "FileNotFoundError",
        libc::EISDIR => "IsADirectoryError",
        libc::ENOTDIR => "NotADirectoryError",
        libc::EACCES | libc::EPERM => "PermissionError",
        _ => "OSError",
    };
    let full = std::io::Error::from_raw_os_error(code).to_string();
    let strerror = full
        .strip_suffix(&format!(" (os error {code})"))
        .unwrap_or(&full);
    PyExc {
        class: class.into(),
        msg: format!(
            "[Errno {code}] {strerror}: {}",
            py_repr(&path.to_string_lossy())
        ),
    }
}

/// `UnicodeDecodeError` for bytes that are not UTF-8.
pub(crate) fn decode_error(bytes: &[u8], e: &std::str::Utf8Error) -> PyExc {
    let at = e.valid_up_to();
    let lead = bytes.get(at).copied().unwrap_or(0);
    let (len, reason) = match e.error_len() {
        None => (bytes.len() - at, "unexpected end of data"),
        Some(_) if (0x80..0xc2).contains(&lead) || lead > 0xf4 => (1, "invalid start byte"),
        Some(_) => (1, "invalid continuation byte"),
    };
    let msg = if len > 1 {
        format!(
            "'utf-8' codec can't decode bytes in position {at}-{}: {reason}",
            at + len - 1
        )
    } else {
        format!("'utf-8' codec can't decode byte 0x{lead:02x} in position {at}: {reason}")
    };
    PyExc {
        class: "UnicodeDecodeError".into(),
        msg,
    }
}

/// `open(path, encoding="utf-8").read()`: `\r\n` and `\r` read as `\n`.
pub(crate) fn read(path: &Path) -> Result<String, PyExc> {
    let bytes = fs::read(path).map_err(|e| os_error(path, &e))?;
    let s = std::str::from_utf8(&bytes).map_err(|e| decode_error(&bytes, &e))?;
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `os.path.isfile`.
pub(super) fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// The names `glob` lists in `dir`: listing order, hidden names dropped,
/// `prefix*suffix`; `dirs` keeps directories only. An unreadable dir is empty.
pub(super) fn visible(dir: &Path, prefix: &str, suffix: &str, dirs: bool) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            !n.starts_with('.')
                && n.len() >= prefix.len() + suffix.len()
                && n.starts_with(prefix)
                && n.ends_with(suffix)
        })
        .filter(|n| !dirs || dir.join(n).is_dir())
        .collect()
}

/// `glob(root/base/**/*.md, recursive=True)` as paths relative to `root`.
pub(super) fn md_tree(root: &Path, base: &str, out: &mut Vec<String>) {
    for name in visible(&root.join(base), "", ".md", false) {
        out.push(format!("{base}/{name}"));
    }
    for sub in visible(&root.join(base), "", "", true) {
        md_tree(root, &format!("{base}/{sub}"), out);
    }
}

/// `re.sub(r"\s+", " ", s)`.
pub(super) fn squash_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if crate::pystr::is_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}
