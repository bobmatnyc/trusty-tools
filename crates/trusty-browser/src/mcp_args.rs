//! Tool-argument validation as FastMCP 1.30 and pydantic 2.14 do it.
//!
//! Why: the Python server validates every `tools/call` with FastMCP's
//! `pre_parse_json` and a pydantic model built from the tool's signature. The
//! coerced values reach the `agent.sock` frame (`"3"` becomes `3`, `"yes"`
//! becomes `true`), and a refusal's text is pydantic's error report, so both
//! must match for the port to be a drop-in.
//! What: [`validate`] takes a tool's [`Field`] list and the call's
//! `arguments` object. It pre-parses JSON-in-a-string for every field not
//! typed plain `str`, applies pydantic's lax rules for `str`, `int`, `bool`
//! and `list[str]` (each optionally `| None`), fills defaults, and returns the
//! fields in declaration order. A failure is the exact `str(ValidationError)`.
//! Known gaps: integers beyond `u64` (Python's are unbounded) are refused
//! `int_parsing_size`; `repr` treats only common non-printable classes as
//! Python's `str.isprintable` does.
//! Test: `validation_matches_pydantic_for_every_captured_case` in
//! `tests/parity_mcp.rs`.

use std::fmt::Write as _;

use crate::pyjson::{Json, float_repr};

/// A field's annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `str`.
    Str,
    /// `str | None`.
    OptStr,
    /// `int`.
    Int,
    /// `bool`.
    Bool,
    /// `list[str]`.
    ListStr,
    /// `list[str] | None`.
    OptListStr,
}

/// A field's default; [`Default::Required`] has none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Default {
    /// No default: a missing key is `missing`.
    Required,
    /// `= None`.
    Null,
    /// An integer default.
    Int(i64),
    /// A boolean default.
    Bool(bool),
    /// A string default.
    Str(&'static str),
}

/// One parameter of a tool function.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// The parameter name.
    pub name: &'static str,
    /// Its annotation.
    pub kind: Kind,
    /// Its default.
    pub default: Default,
}

/// Shorthand for a [`Field`].
pub const fn field(name: &'static str, kind: Kind, default: Default) -> Field {
    Field {
        name,
        kind,
        default,
    }
}

struct Issue {
    loc: String,
    kind: &'static str,
    msg: &'static str,
    input: Json,
}

/// Validate `args` (a JSON object) against `fields` of the model `<tool>Arguments`.
///
/// Why/What: see the module doc. Keys not in `fields` are ignored but still
/// appear in a `missing` error's `input_value`, as in pydantic.
/// Test: `validation_matches_pydantic_for_every_captured_case`.
pub fn validate(
    tool: &str,
    fields: &[Field],
    args: &Json,
) -> Result<Vec<(&'static str, Json)>, String> {
    let empty = Vec::new();
    let items = match args {
        Json::Object(items) => items,
        _ => &empty,
    };
    let parsed: Vec<(String, Json)> = items
        .iter()
        .map(|(k, v)| match fields.iter().find(|f| f.name == k) {
            Some(f) if f.kind != Kind::Str => (k.clone(), pre_parse(v)),
            _ => (k.clone(), v.clone()),
        })
        .collect();
    let mut out = Vec::new();
    let mut issues = Vec::new();
    for f in fields {
        match parsed.iter().find(|(k, _)| k == f.name) {
            Some((_, v)) => match coerce(f, v, &mut issues) {
                Some(v) => out.push((f.name, v)),
                None => continue,
            },
            None => match default_value(f.default) {
                Some(v) => out.push((f.name, v)),
                None => issues.push(Issue {
                    loc: f.name.to_owned(),
                    kind: "missing",
                    msg: "Field required",
                    input: Json::Object(parsed.clone()),
                }),
            },
        }
    }
    if issues.is_empty() {
        return Ok(out);
    }
    Err(report(&format!("{tool}Arguments"), &issues))
}

/// FastMCP `pre_parse_json`: a string holding a JSON list, object or `null`
/// is replaced by that value; a JSON string, number or bool (a Python int)
/// leaves the original string in place.
fn pre_parse(v: &Json) -> Json {
    let Json::Str(s) = v else {
        return v.clone();
    };
    match Json::parse(s.as_bytes()) {
        Ok(Json::Str(_) | Json::Int(_) | Json::UInt(_) | Json::Float(_) | Json::Bool(_))
        | Err(_) => v.clone(),
        Ok(other) => other,
    }
}

fn default_value(d: Default) -> Option<Json> {
    match d {
        Default::Required => None,
        Default::Null => Some(Json::Null),
        Default::Int(i) => Some(Json::Int(i)),
        Default::Bool(b) => Some(Json::Bool(b)),
        Default::Str(s) => Some(Json::from(s)),
    }
}

fn coerce(f: &Field, v: &Json, issues: &mut Vec<Issue>) -> Option<Json> {
    let mut fail = |kind, msg| {
        issues.push(Issue {
            loc: f.name.to_owned(),
            kind,
            msg,
            input: v.clone(),
        });
        None
    };
    let nullable = matches!(f.kind, Kind::OptStr | Kind::OptListStr);
    if nullable && *v == Json::Null {
        return Some(Json::Null);
    }
    match f.kind {
        Kind::Str | Kind::OptStr => match v {
            Json::Str(_) => Some(v.clone()),
            _ => fail("string_type", "Input should be a valid string"),
        },
        Kind::Int => match int(v) {
            Ok(i) => Some(i),
            Err((kind, msg)) => fail(kind, msg),
        },
        Kind::Bool => match boolean(v) {
            Ok(b) => Some(Json::Bool(b)),
            Err((kind, msg)) => fail(kind, msg),
        },
        Kind::ListStr | Kind::OptListStr => {
            let Json::Array(items) = v else {
                return fail("list_type", "Input should be a valid list");
            };
            let mut ok = true;
            for (i, item) in items.iter().enumerate() {
                if !matches!(item, Json::Str(_)) {
                    ok = false;
                    issues.push(Issue {
                        loc: format!("{}.{i}", f.name),
                        kind: "string_type",
                        msg: "Input should be a valid string",
                        input: item.clone(),
                    });
                }
            }
            ok.then(|| v.clone())
        }
    }
}

type Fail = (&'static str, &'static str);

const INT_PARSING: Fail = (
    "int_parsing",
    "Input should be a valid integer, unable to parse string as an integer",
);
const INT_SIZE: Fail = (
    "int_parsing_size",
    "Unable to parse input string as an integer, exceeded maximum size",
);

/// pydantic lax `int`.
fn int(v: &Json) -> Result<Json, Fail> {
    match v {
        Json::Int(_) | Json::UInt(_) => Ok(v.clone()),
        Json::Bool(b) => Ok(Json::Int(i64::from(*b))),
        Json::Float(f) if !f.is_finite() => {
            Err(("finite_number", "Input should be a finite number"))
        }
        Json::Float(f) if f.fract() != 0.0 => Err((
            "int_from_float",
            "Input should be a valid integer, got a number with a fractional part",
        )),
        // 2^63 is the first float outside i64.
        Json::Float(f) if f.abs() >= 9_223_372_036_854_775_808.0 => Err(INT_SIZE),
        Json::Float(f) => Ok(Json::Int(*f as i64)),
        Json::Str(s) => str_int(s),
        _ => Err(("int_type", "Input should be a valid integer")),
    }
}

/// pydantic's string-to-int: Python whitespace trimmed, an optional sign,
/// ASCII digits with single `_` between digits, an optional `.0…` tail.
fn str_int(s: &str) -> Result<Json, Fail> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    let t = match t.split_once('.') {
        Some((whole, frac)) if !frac.is_empty() && frac.bytes().all(|b| b == b'0') => whole,
        Some(_) => return Err(INT_PARSING),
        None => t,
    };
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let bytes = digits.as_bytes();
    let well_formed = !bytes.is_empty()
        && bytes.iter().all(|b| b.is_ascii_digit() || *b == b'_')
        && bytes.first().is_some_and(u8::is_ascii_digit)
        && bytes.last().is_some_and(u8::is_ascii_digit)
        && !digits.contains("__");
    if !well_formed {
        return Err(INT_PARSING);
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let signed = if neg {
        format!("-{clean}")
    } else {
        clean.clone()
    };
    if let Ok(i) = signed.parse::<i64>() {
        return Ok(Json::Int(i));
    }
    match clean.parse::<u64>() {
        Ok(u) if !neg => Ok(Json::UInt(u)),
        _ => Err(INT_SIZE),
    }
}

/// pydantic lax `bool`.
fn boolean(v: &Json) -> Result<bool, Fail> {
    const PARSING: Fail = (
        "bool_parsing",
        "Input should be a valid boolean, unable to interpret input",
    );
    const TYPE: Fail = ("bool_type", "Input should be a valid boolean");
    match v {
        Json::Bool(b) => Ok(*b),
        Json::Int(0) => Ok(false),
        Json::Int(1) => Ok(true),
        Json::Int(_) | Json::UInt(_) => Err(PARSING),
        Json::Float(f) if *f == 0.0 => Ok(false),
        Json::Float(f) if *f == 1.0 => Ok(true),
        Json::Float(f) if f.is_finite() && f.fract() == 0.0 => Err(PARSING),
        Json::Str(s) => match s.to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
            _ => Err(PARSING),
        },
        _ => Err(TYPE),
    }
}

fn report(model: &str, issues: &[Issue]) -> String {
    let n = issues.len();
    let mut out = format!(
        "{n} validation error{} for {model}",
        if n == 1 { "" } else { "s" }
    );
    for i in issues {
        let _ = write!(
            out,
            "\n{}\n  {} [type={}, input_value={}, input_type={}]\n    For further information visit https://errors.pydantic.dev/2.14/v/{}",
            i.loc,
            i.msg,
            i.kind,
            truncate(&repr(&i.input)),
            type_name(&i.input),
            i.kind
        );
    }
    out
}

/// pydantic shortens an `input_value` over 50 UTF-8 bytes to its first 25
/// bytes + `...` + its last 24 (cuts moved off a character boundary outward).
fn truncate(s: &str) -> String {
    if s.len() <= 50 {
        return s.to_owned();
    }
    let mut head = 25;
    while !s.is_char_boundary(head) {
        head += 1;
    }
    let mut tail = s.len() - 24;
    while !s.is_char_boundary(tail) {
        tail -= 1;
    }
    format!("{}...{}", &s[..head], &s[tail..])
}

fn type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Int(_) | Json::UInt(_) => "int",
        Json::Float(_) => "float",
        Json::Str(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// Python `repr` of a JSON-decoded value (`str()` of a non-string is the same).
pub fn repr(v: &Json) -> String {
    match v {
        Json::Null => "None".to_owned(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        Json::Int(i) => i.to_string(),
        Json::UInt(u) => u.to_string(),
        Json::Float(f) => float_repr(*f).unwrap_or_else(|_| "nan".to_owned()),
        Json::Str(s) => str_repr(s),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Json::Object(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python `str.__repr__`.
fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x100 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) < 0x10000 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable` for one character: not a control, separator (other than
/// space), format or private-use character.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let u = c as u32;
    let format = matches!(u, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0xE0001 | 0xE0020..=0xE007F);
    let private = matches!(u, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF);
    !(c.is_control() || c.is_whitespace() || format || private)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_picks_quotes_and_escapes_like_python() {
        assert_eq!(str_repr("a'b"), "\"a'b\"");
        assert_eq!(str_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(
            str_repr("\u{0}\u{7f}\u{a0}\u{200b}\u{1F600}"),
            "'\\x00\\x7f\\xa0\\u200b\u{1F600}'"
        );
    }

    #[test]
    fn long_input_values_are_shortened() {
        let s = "x".repeat(60);
        assert_eq!(truncate(&s).chars().count(), 52);
        // Bytes, not characters: a two-byte 'é' in the tail leaves 23 characters.
        let s = format!("{}é{}", "x".repeat(40), "y".repeat(22));
        assert_eq!(
            truncate(&s),
            format!("{}...é{}", "x".repeat(25), "y".repeat(22))
        );
    }
}
