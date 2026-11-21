//! Order-preserving JSON for the Intent Ledger files.
//!
//! Why: Python writes `state.json` keys in insertion order and the live
//! Architect ledger holds 24 distinct task key orders. `serde_json::Value` sorts
//! keys (and `preserve_order` would change every crate's `Value`), so a re-write
//! through it would not be byte-identical. This type keeps keys where they were
//! put, the way a Python `dict` does.
//! What: [`Json`] (objects are `Vec<(String, Json)>`), a parser, and the three
//! `json.dumps` shapes the scripts use: compact `(",", ":")`, the default
//! `(", ", ": ")`, and `indent=1`, all with `ensure_ascii=False`.
//! Test: `roundtrip_keeps_key_order_and_bytes`, `dump_escapes_like_python`,
//! `pretty_matches_python_indent_one`.

/// One JSON value; an object keeps its keys in insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer that fits `i64`.
    Int(i64),
    /// Any other number.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object, keys in insertion order.
    Obj(Vec<(String, Json)>),
}

impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json::Str(s.to_owned())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Self {
        Json::Str(s)
    }
}
impl From<i64> for Json {
    fn from(n: i64) -> Self {
        Json::Int(n)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json::Bool(b)
    }
}

/// An object from `(key, value)` pairs in the order given.
pub fn obj<const N: usize>(pairs: [(&str, Json); N]) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

const EMPTY: &[Json] = &[];

impl Json {
    /// The value under `key` of an object, `None` for a missing key or a non-object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Mutable [`Json::get`].
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Obj(m) => m.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `get` that treats `null` as missing (Python `a.get(k) is not None`).
    pub fn opt(&self, key: &str) -> Option<&Json> {
        self.get(key).filter(|v| !matches!(v, Json::Null))
    }

    /// Python `dict[key] = value`: an existing key keeps its place, a new one is appended.
    pub fn set(&mut self, key: &str, value: impl Into<Json>) {
        if let Json::Obj(m) = self {
            let value = value.into();
            match m.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => m.push((key.to_owned(), value)),
            }
        }
    }

    /// Python `dict.pop(key, None)`.
    pub fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Obj(m) => m.iter().position(|(k, _)| k == key).map(|i| m.remove(i).1),
            _ => None,
        }
    }

    /// The `(key, value)` pairs of an object in order, empty for anything else.
    pub fn entries(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(m) => m,
            _ => &[],
        }
    }

    /// The string under `key`, or `""` when missing or not a string.
    pub fn s(&self, key: &str) -> &str {
        self.get(key).and_then(Json::as_str).unwrap_or("")
    }

    /// The integer under `key` (a bool is not an integer here).
    pub fn i(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Json::as_i64)
    }

    /// The array under `key`, empty when missing or not an array.
    pub fn arr(&self, key: &str) -> &[Json] {
        self.get(key).and_then(Json::as_arr).unwrap_or(EMPTY)
    }

    /// The mutable array under `key`.
    pub fn arr_mut(&mut self, key: &str) -> Option<&mut Vec<Json>> {
        match self.get_mut(key) {
            Some(Json::Arr(a)) => Some(a),
            _ => None,
        }
    }

    /// The array under `key` of an object, inserted empty when absent (`dict.setdefault(key, [])`).
    ///
    /// Callers pass a ledger state that `load_state` proved to be an object.
    pub fn arr_or_insert(&mut self, key: &str) -> &mut Vec<Json> {
        if !matches!(self, Json::Obj(_)) {
            *self = Json::Obj(Vec::new()); // never taken for a loaded state; keeps the match below total
        }
        if !matches!(self.get(key), Some(Json::Arr(_))) {
            self.set(key, Json::Arr(Vec::new()));
        }
        match self.get_mut(key) {
            Some(Json::Arr(a)) => a,
            _ => unreachable!("the key was just set to an array on an object"),
        }
    }

    /// This value as a string slice.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// This value as an integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// This value as an array slice.
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// Python truthiness: empty, zero, false and null are falsy.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Int(n) => *n != 0,
            Json::Float(f) => *f != 0.0,
            Json::Str(s) => !s.is_empty(),
            Json::Arr(a) => !a.is_empty(),
            Json::Obj(m) => !m.is_empty(),
        }
    }

    /// Python `str(value)` for the scalars an op carries; `None` reads as `""`.
    pub fn py_str(&self) -> String {
        match self {
            Json::Null => "None".to_owned(),
            Json::Bool(true) => "True".to_owned(),
            Json::Bool(false) => "False".to_owned(),
            Json::Str(s) => s.clone(),
            other => other.dump(),
        }
    }

    /// Compact `json.dumps(v, separators=(",", ":"), ensure_ascii=False)`.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out, ",", ":", None, 0);
        out
    }

    /// Default `json.dumps(v, ensure_ascii=False)` (`", "` and `": "`).
    pub fn dump_spaced(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out, ", ", ": ", None, 0);
        out
    }

    /// `json.dumps(v, indent=1, ensure_ascii=False)`.
    pub fn dump_pretty(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out, ",", ": ", Some(1), 0);
        out
    }

    /// Parses one JSON document; trailing text is an error.
    ///
    /// # Errors
    /// A message naming the byte offset of the first problem.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("extra data at byte {}", p.i));
        }
        Ok(v)
    }
}

fn write_str(s: &str, out: &mut String) {
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * depth));
    }
}

fn write_value(v: &Json, out: &mut String, item: &str, kv: &str, indent: Option<usize>, d: usize) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Float(f) => out.push_str(&format!("{f:?}")),
        Json::Str(s) => write_str(s, out),
        Json::Arr(a) if a.is_empty() => out.push_str("[]"),
        Json::Obj(m) if m.is_empty() => out.push_str("{}"),
        Json::Arr(a) => {
            out.push('[');
            for (n, x) in a.iter().enumerate() {
                if n > 0 {
                    out.push_str(item);
                }
                newline(out, indent, d + 1);
                write_value(x, out, item, kv, indent, d + 1);
            }
            newline(out, indent, d);
            out.push(']');
        }
        Json::Obj(m) => {
            out.push('{');
            for (n, (k, x)) in m.iter().enumerate() {
                if n > 0 {
                    out.push_str(item);
                }
                newline(out, indent, d + 1);
                write_str(k, out);
                out.push_str(kv);
                write_value(x, out, item, kv, indent, d + 1);
            }
            newline(out, indent, d);
            out.push('}');
        }
    }
}

const MAX_DEPTH: usize = 256;

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("bad literal")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return self.err("nesting too deep");
        }
        match self.b.get(self.i) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("expecting a value"),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1;
        let mut out = Json::Obj(Vec::new());
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(out);
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return self.err("expecting a property name");
            }
            let k = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return self.err("expecting ':'");
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            out.set(&k, v); // a repeated key keeps its first place, last value: Python's dict
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return self.err("expecting ',' or '}'"),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth + 1)?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                _ => return self.err("expecting ',' or ']'"),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .b
            .get(self.i..self.i + 4)
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| u32::from_str_radix(s, 16).ok());
        match h {
            Some(n) => {
                self.i += 4;
                Ok(n)
            }
            None => self.err("bad \\u escape"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // The input came from a &str and the run stops at ASCII bytes, so it is whole UTF-8.
            out.push_str(&String::from_utf8_lossy(&self.b[start..self.i]));
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = self.b.get(self.i).copied();
                    self.i += 1;
                    match e {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => out.push(self.unicode()?),
                        _ => return self.err("bad escape"),
                    }
                }
                _ => return self.err("unterminated string or control character"),
            }
        }
    }

    fn unicode(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) && self.b[self.i..].starts_with(b"\\u") {
            let save = self.i;
            self.i += 2;
            let lo = self.hex4()?;
            if (0xDC00..0xE000).contains(&lo) {
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                return Ok(char::from_u32(cp).unwrap_or('\u{fffd}'));
            }
            self.i = save;
        }
        // A lone surrogate cannot live in a Rust string; U+FFFD stands in.
        Ok(char::from_u32(hi).unwrap_or('\u{fffd}'))
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while matches!(p.b.get(p.i), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i > s
        };
        if !digits(self) {
            return self.err("bad number");
        }
        let mut float = false;
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            float = true;
            if !digits(self) {
                return self.err("bad number");
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            float = true;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return self.err("bad number");
            }
        }
        let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
        if !float && let Ok(n) = text.parse::<i64>() {
            return Ok(Json::Int(n));
        }
        text.parse::<f64>()
            .map(Json::Float)
            .or_else(|_| self.err("bad number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_key_order_and_bytes() {
        let text = r#"{"z":1,"a":[true,null,{"y":"é","b":2.5}],"m":"x"}"#;
        let v = Json::parse(text).unwrap();
        assert_eq!(v.dump(), text);
    }

    #[test]
    fn dump_escapes_like_python() {
        let v = Json::Str("a\"b\\c\n\t\u{1}\u{7f}é".to_owned());
        assert_eq!(v.dump(), "\"a\\\"b\\\\c\\n\\t\\u0001\u{7f}é\"");
        assert_eq!(Json::parse(&v.dump()).unwrap(), v);
    }

    #[test]
    fn pretty_matches_python_indent_one() {
        let v = Json::parse(r#"{"a":[1,2],"b":{},"c":[]}"#).unwrap();
        assert_eq!(
            v.dump_pretty(),
            "{\n \"a\": [\n  1,\n  2\n ],\n \"b\": {},\n \"c\": []\n}"
        );
        assert_eq!(v.dump_spaced(), r#"{"a": [1, 2], "b": {}, "c": []}"#);
    }

    #[test]
    fn repeated_key_keeps_first_place_last_value() {
        let v = Json::parse(r#"{"a":1,"b":2,"a":3}"#).unwrap();
        assert_eq!(v.dump(), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn parse_rejects_garbage() {
        for bad in [
            "",
            "{",
            "[1,]",
            r#"{"a":}"#,
            "1 2",
            "\"\u{1}\"",
            "{\"a\" 1}",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?}");
        }
    }
}
