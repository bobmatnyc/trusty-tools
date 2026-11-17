//! An ordered JSON value whose compact form matches Python's `json.dumps`.
//!
//! Why: frame bodies, audit lines and the HMAC transcript are bytes Python
//! defines. `serde_json::Value` sorts keys unless the workspace-wide
//! `preserve_order` feature is on, and its string and float output differs
//! from Python's. Enabling that feature here would change every other crate in
//! a unified build, so this crate keeps its own value type.
//! What: [`Json`] keeps object keys in insertion order (a repeated key keeps
//! its first position and takes the last value, like a Python dict), and
//! [`Json::dumps`] writes `separators=(",", ":")` output with Python's
//! `ensure_ascii` escaping and `float.__repr__` numbers.
//! Test: `every_captured_frame_reencodes_byte_identically` in
//! `tests/parity_frames.rs`, plus the unit tests below.

use std::fmt::{self, Write as _};

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// One JSON value; objects keep key order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer that fits `i64`.
    Int(i64),
    /// An integer above `i64::MAX`.
    UInt(u64),
    /// A float; Python writes it with `repr`.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, in insertion order.
    Object(Vec<(String, Json)>),
}

/// `json.dumps` refused the value (`allow_nan=False`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Out of range float values are not JSON compliant")]
pub struct NonFinite;

impl Json {
    /// Parse JSON text, keeping object key order.
    pub fn parse(text: &[u8]) -> Result<Json, serde_json::Error> {
        serde_json::from_slice(text)
    }

    /// Build an object from `(key, value)` pairs, in order.
    pub fn object<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Json)>) -> Json {
        let mut out = Json::Object(Vec::new());
        for (k, v) in pairs {
            out.insert(k, v);
        }
        out
    }

    /// Set `key` on an object: a new key is appended, an existing key keeps its place.
    /// A non-object is left unchanged.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        if let Json::Object(items) = self {
            let key = key.into();
            match items.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => items.push((key, value)),
            }
        }
    }

    /// The value under `key` when `self` is an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string inside a `Json::Str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Compact `json.dumps(self, separators=(",", ":"), allow_nan=False,
    /// ensure_ascii=ensure_ascii)`.
    pub fn dumps(&self, ensure_ascii: bool) -> Result<String, NonFinite> {
        self.dumps_style(Style::COMPACT, ensure_ascii)
    }

    /// `json.dumps(self, allow_nan=False)` with the default `", "` / `": "`
    /// separators; the MCP server's `agent.sock` request frames use it.
    pub fn dumps_spaced(&self) -> Result<String, NonFinite> {
        self.dumps_style(Style::SPACED, true)
    }

    /// `json.dumps(self, indent=2, ensure_ascii=False)`: the text FastMCP
    /// puts in a tool result's `content`.
    pub fn dumps_indent2(&self) -> Result<String, NonFinite> {
        self.dumps_style(Style::INDENT2, false)
    }

    /// `json.dumps(self, indent=2)` (ASCII-escaped): the published extension manifest.
    pub fn dumps_indent2_ascii(&self) -> Result<String, NonFinite> {
        self.dumps_style(Style::INDENT2, true)
    }

    fn dumps_style(&self, style: Style, ascii: bool) -> Result<String, NonFinite> {
        let mut out = String::new();
        self.write(&mut out, style, ascii, 0)?;
        Ok(out)
    }

    fn write(
        &self,
        out: &mut String,
        st: Style,
        ascii: bool,
        depth: usize,
    ) -> Result<(), NonFinite> {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(i) => push_display(out, i),
            Json::UInt(u) => push_display(out, u),
            Json::Float(f) => out.push_str(&float_repr(*f)?),
            Json::Str(s) => write_str(out, s, ascii),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Object(items) if items.is_empty() => out.push_str("{}"),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    st.item(out, i, depth + 1);
                    item.write(out, st, ascii, depth + 1)?;
                }
                st.close(out, depth);
                out.push(']');
            }
            Json::Object(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    st.item(out, i, depth + 1);
                    write_str(out, k, ascii);
                    out.push_str(st.key);
                    v.write(out, st, ascii, depth + 1)?;
                }
                st.close(out, depth);
                out.push('}');
            }
        }
        Ok(())
    }
}

/// `json.dumps` separators and indent.
#[derive(Clone, Copy)]
struct Style {
    item: &'static str,
    key: &'static str,
    indent: Option<usize>,
}

impl Style {
    const COMPACT: Style = Style {
        item: ",",
        key: ":",
        indent: None,
    };
    const SPACED: Style = Style {
        item: ", ",
        key: ": ",
        indent: None,
    };
    const INDENT2: Style = Style {
        item: ",",
        key: ": ",
        indent: Some(2),
    };

    /// Before item `i` of a container whose items sit at `depth`.
    fn item(self, out: &mut String, i: usize, depth: usize) {
        if i > 0 {
            out.push_str(self.item);
        }
        if let Some(n) = self.indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * depth));
        }
    }

    /// Before the closing bracket of a non-empty container at `depth`.
    fn close(self, out: &mut String, depth: usize) {
        if let Some(n) = self.indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * depth));
        }
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_owned())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}

impl From<i64> for Json {
    fn from(i: i64) -> Json {
        Json::Int(i)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

fn push_display(out: &mut String, value: impl fmt::Display) {
    // Writing to a String cannot fail.
    let _ = write!(out, "{value}");
}

/// Python's JSON string escaping: `ensure_ascii` escapes everything outside
/// printable ASCII as `\uXXXX` (surrogate pairs above the BMP); otherwise only
/// quotes, backslashes and C0 controls are escaped.
fn write_str(out: &mut String, s: &str, ascii: bool) {
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
            c if (c as u32) < 0x20 => push_u(out, c as u32),
            c if ascii && !(' '..='~').contains(&c) => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    push_u(out, u32::from(*unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_u(out: &mut String, unit: u32) {
    let _ = write!(out, "\\u{unit:04x}");
}

/// Python's `float.__repr__`: shortest round-trip digits, fixed notation for
/// decimal exponents in `-4..16`, else `d.ddde+XX` with at least two exponent digits.
pub fn float_repr(f: f64) -> Result<String, NonFinite> {
    if !f.is_finite() {
        return Err(NonFinite);
    }
    if f == 0.0 {
        return Ok(if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned());
    }
    // `{:e}` gives the shortest round-trip digits, e.g. "1.79000000025e9".
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').ok_or(NonFinite)?;
    let exp: i32 = exp.parse().map_err(|_| NonFinite)?;
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::from(sign);
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        if point <= 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', point.unsigned_abs() as usize));
            out.push_str(&digits);
        } else {
            let point = point as usize;
            if digits.len() <= point {
                out.push_str(&digits);
                out.extend(std::iter::repeat_n('0', point - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..point]);
                out.push('.');
                out.push_str(&digits[point..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    Ok(out)
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Int(v))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Json, E> {
        Ok(i64::try_from(v).map_or(Json::UInt(v), Json::Int))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        Ok(Json::Float(v))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(v.to_owned()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Json, E> {
        Ok(Json::Str(v))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut out = Json::Object(Vec::new());
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            out.insert(k, v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (1790000000.0, "1790000000.0"),
            (1790000000.25, "1790000000.25"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1.5e-5, "1.5e-05"),
            (0.0001, "0.0001"),
            (123456789012345.6, "123456789012345.6"),
            (-2.5, "-2.5"),
            (-0.0, "-0.0"),
        ];
        for (f, want) in cases {
            assert_eq!(float_repr(f).ok().as_deref(), Some(want), "{f}");
        }
        assert_eq!(float_repr(f64::NAN), Err(NonFinite));
    }

    #[test]
    fn strings_escape_like_python() {
        let v = Json::from("é\u{1F600}\"\\\n\u{1}\u{7f}");
        assert_eq!(
            v.dumps(true).ok().as_deref(),
            Some(r#""\u00e9\ud83d\ude00\"\\\n\u0001\u007f""#)
        );
        assert_eq!(
            v.dumps(false).ok().as_deref(),
            Some("\"é\u{1F600}\\\"\\\\\\n\\u0001\u{7f}\"")
        );
    }

    #[test]
    fn repeated_key_keeps_first_position_and_last_value() {
        let v = Json::parse(br#"{"a":1,"b":2,"a":3}"#).ok();
        assert_eq!(
            v.and_then(|v| v.dumps(true).ok()).as_deref(),
            Some(r#"{"a":3,"b":2}"#)
        );
    }

    #[test]
    fn spaced_and_indented_styles_match_python() {
        let v = Json::parse(br#"{"a":[1,{}],"b":[],"c":"\u00e9"}"#).ok();
        let v = v.unwrap_or(Json::Null);
        assert_eq!(
            v.dumps_spaced().ok().as_deref(),
            Some(r#"{"a": [1, {}], "b": [], "c": "\u00e9"}"#)
        );
        assert_eq!(
            v.dumps_indent2().ok().as_deref(),
            Some("{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": [],\n  \"c\": \"é\"\n}")
        );
    }
}
