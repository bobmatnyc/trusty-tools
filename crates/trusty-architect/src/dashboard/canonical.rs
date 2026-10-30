//! The dashboard store's two byte encodings, equal to the prototype's.
//!
//! Why: the Rust writer must leave the bytes `dashboard_store.py` leaves
//! (DOC-82 LDG-3.2, #10063 AC 2), so a file hash is a state fingerprint and
//! either side can take over a store. `serde_json` differs from Python's
//! `json.dumps` in float spelling (`1e16` against `1e+16`) and in key order
//! when `preserve_order` is unified in, so the text is written here.
//! What: [`compact`] is `json.dumps(v, sort_keys=True, separators=(",", ":"),
//! ensure_ascii=False)` (one `jsonl` line); [`pretty`] is `json.dumps(v,
//! indent=2, sort_keys=True, ensure_ascii=False)` (one `dir` file);
//! [`python_dumps`] is Python's default `json.dumps(v)` (ASCII escapes, `", "`
//! and `": "`), which the list sort key and the schema messages quote;
//! [`py_float`] is Python's `repr(float)`. Keys sort by code point, which is
//! UTF-8 byte order, at every depth.
//! Test: `canonical_floats_follow_python_repr`, `canonical_escapes_follow_python`,
//! `dashboard_writer_matches_prototype_bytes`.

use serde_json::{Map, Value};

/// How one value is spelled.
#[derive(Clone, Copy)]
struct Style {
    item_sep: &'static str,
    key_sep: &'static str,
    indent: Option<usize>,
    ensure_ascii: bool,
    sort_keys: bool,
}

/// One `jsonl` line body: sorted keys, `,` and `:`, UTF-8 kept.
pub fn compact(value: &Value) -> String {
    render(value, COMPACT)
}

/// One `dir` document body: sorted keys, two-space indent, UTF-8 kept. No
/// trailing newline.
pub fn pretty(value: &Value) -> String {
    render(value, PRETTY)
}

/// Python's `json.dumps(value)` (`sort_keys` as given): `", "` and `": "`,
/// every character outside `' '..='~'` escaped.
pub fn python_dumps(value: &Value, sort_keys: bool) -> String {
    render(
        value,
        Style {
            sort_keys,
            ..PYTHON
        },
    )
}

/// Python's `json.dumps(value, ensure_ascii=False)`: default separators,
/// keys in map order, UTF-8 kept.
pub fn python_dumps_utf8(value: &Value) -> String {
    render(
        value,
        Style {
            sort_keys: false,
            ensure_ascii: false,
            ..PYTHON
        },
    )
}

const COMPACT: Style = Style {
    item_sep: ",",
    key_sep: ":",
    indent: None,
    ensure_ascii: false,
    sort_keys: true,
};
const PRETTY: Style = Style {
    item_sep: ",",
    key_sep: ": ",
    indent: Some(2),
    ensure_ascii: false,
    sort_keys: true,
};
const PYTHON: Style = Style {
    item_sep: ", ",
    key_sep: ": ",
    indent: None,
    ensure_ascii: true,
    sort_keys: true,
};

/// A reply as `dashboard-db.py`'s `emit` prints it: an [`Emit::Object`]
/// keeps the key order given (the prototype's dict order), an
/// [`Emit::Value`] sorts its keys as the stored files do (#10063 PR 3).
#[derive(Debug, Clone, PartialEq)]
pub enum Emit {
    /// Keys in this order.
    Object(Vec<(String, Emit)>),
    /// Items in this order.
    List(Vec<Emit>),
    /// A stored value: keys sorted at every depth.
    Value(Value),
}

/// `json.dump(obj, indent=2, ensure_ascii=False)` and a newline.
pub fn emit(e: &Emit) -> String {
    let mut out = String::new();
    write_emit(&mut out, e, 0);
    out.push('\n');
    out
}

fn write_emit(out: &mut String, e: &Emit, depth: usize) {
    match e {
        Emit::Value(v) => write_value(out, v, PRETTY, depth),
        Emit::List(items) => write_seq(out, '[', ']', items.len(), PRETTY, depth, |out, i| {
            write_emit(out, &items[i], depth + 1);
        }),
        Emit::Object(fields) => write_seq(out, '{', '}', fields.len(), PRETTY, depth, |out, i| {
            write_str(out, &fields[i].0, false);
            out.push_str(PRETTY.key_sep);
            write_emit(out, &fields[i].1, depth + 1);
        }),
    }
}

fn render(value: &Value, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    out
}

fn write_value(out: &mut String, value: &Value, style: Style, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_u64(), n.as_i64(), n.as_f64()) {
            (Some(u), _, _) => out.push_str(&u.to_string()),
            (None, Some(i), _) => out.push_str(&i.to_string()),
            (None, None, Some(f)) => out.push_str(&py_float(f)),
            (None, None, None) => out.push_str(&n.to_string()),
        },
        Value::String(s) => write_str(out, s, style.ensure_ascii),
        Value::Array(items) => {
            let parts: Vec<&Value> = items.iter().collect();
            write_seq(out, '[', ']', parts.len(), style, depth, |out, i| {
                write_value(out, parts[i], style, depth + 1);
            });
        }
        Value::Object(map) => {
            let keys = keys(map, style.sort_keys);
            write_seq(out, '{', '}', keys.len(), style, depth, |out, i| {
                write_str(out, keys[i], style.ensure_ascii);
                out.push_str(style.key_sep);
                if let Some(v) = map.get(keys[i]) {
                    write_value(out, v, style, depth + 1);
                }
            });
        }
    }
}

fn keys(map: &Map<String, Value>, sort: bool) -> Vec<&str> {
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    if sort {
        keys.sort_unstable();
    }
    keys
}

/// Writes `len` items between `open` and `close`; Python writes an empty
/// container as `[]` / `{}` whatever the indent.
fn write_seq(
    out: &mut String,
    open: char,
    close: char,
    len: usize,
    style: Style,
    depth: usize,
    mut item: impl FnMut(&mut String, usize),
) {
    out.push(open);
    if len == 0 {
        out.push(close);
        return;
    }
    let newline = |out: &mut String, level: usize| {
        if let Some(width) = style.indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
    };
    for i in 0..len {
        if i > 0 {
            out.push_str(style.item_sep);
        }
        newline(out, depth + 1);
        item(out, i);
    }
    newline(out, depth);
    out.push(close);
}

/// A JSON string as Python's encoder writes it: `\"`, `\\`, the five short
/// escapes, `\u00XX` (lowercase) for other C0 controls and, under
/// `ensure_ascii`, `\uXXXX` (surrogate pairs above U+FFFF) for everything
/// outside `' '..='~'`.
fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c < ' ' || (ensure_ascii && c > '~') => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: the shortest round-trip digits, fixed notation
/// when the decimal exponent is in `-4..16`, else `d.ddde+XX` with a signed
/// two-digit-minimum exponent; a whole number keeps `.0`.
pub fn py_float(f: f64) -> String {
    // Rust's `{:e}` prints the same shortest round-trip digits as Python.
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exp + 1; // digits are 0.d1d2... x 10^point
    let n = digits.len() as i32;
    if point <= -4 || point > 16 {
        let (head, tail) = digits.split_at(1);
        let frac = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{head}{frac}e{esign}{:02}", exp.abs());
    }
    if point <= 0 {
        let zeros = "0".repeat(point.unsigned_abs() as usize);
        return format!("{sign}0.{zeros}{digits}");
    }
    if point >= n {
        let zeros = "0".repeat((point - n) as usize);
        return format!("{sign}{digits}{zeros}.0");
    }
    let (int, frac) = digits.split_at(point as usize);
    format!("{sign}{int}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_floats_follow_python_repr() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1e22, "1e+22"),
            (123_456_789_012_345.6, "123456789012345.6"),
            (0.0001, "0.0001"),
            (1.5e-5, "1.5e-05"),
            (2.5e-300, "2.5e-300"),
            (5e-324, "5e-324"),
            (1.797_693_134_862_315_7e308, "1.7976931348623157e+308"),
            (-2.75, "-2.75"),
        ];
        for (f, want) in cases {
            assert_eq!(py_float(f), want, "repr({f:?})");
        }
    }

    #[test]
    fn canonical_escapes_follow_python() {
        let v = json!({"b": "\u{0}\u{1f}\u{7f}\t\u{8}\u{c}\"\\/", "a": ["é", "\u{1f600}", {}, []]});
        assert_eq!(
            compact(&v),
            "{\"a\":[\"é\",\"\u{1f600}\",{},[]],\"b\":\"\\u0000\\u001f\u{7f}\\t\\b\\f\\\"\\\\/\"}"
        );
        assert_eq!(
            python_dumps(&v, true),
            "{\"a\": [\"\\u00e9\", \"\\ud83d\\ude00\", {}, []], \"b\": \"\\u0000\\u001f\\u007f\\t\\b\\f\\\"\\\\/\"}"
        );
        assert_eq!(
            pretty(&json!({"k": [1, {"x": null}], "e": {}})),
            "{\n  \"e\": {},\n  \"k\": [\n    1,\n    {\n      \"x\": null\n    }\n  ]\n}"
        );
    }
}
