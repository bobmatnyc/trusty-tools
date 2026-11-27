//! Python's `json.loads` for a `str`, error messages and value kinds included.
//!
//! Why: a ported config reader must say what the script says about a bad
//! file (#12028 AC1): `json.JSONDecodeError`'s message, line and column, and
//! the values Python accepts that `serde_json` refuses (`NaN`, `Infinity`,
//! integers past 64 bits, a repeated key).
//! What: [`loads`] follows CPython 3.14's C scanner (`Modules/_json.c`) over
//! code points: the same checks in the same order, so the first error and
//! its position match. [`PyValue::Dict`] keeps first-insertion order with the
//! last value of a repeated key, as `dict(pairs)` does.
//! Known gap: a lone surrogate escape becomes U+FFFD (Rust strings cannot
//! hold one); nesting past Python's recursion limit is not refused.
//! Test: `architect_config_recordings_replay_clean_against_the_port`
//! (`bad-json-*` recordings), `loads_matches_python_errors`.

/// A Python integer from JSON text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PyInt {
    /// Fits `i64`.
    Small(i64),
    /// Past `i64`: the decimal text (`-` included), compared by sign only.
    Big(String),
}

/// A decoded JSON value with Python's types.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PyValue {
    Null,
    Bool(bool),
    Int(PyInt),
    Float(f64),
    Str(String),
    List(Vec<PyValue>),
    Dict(Vec<(String, PyValue)>),
}

/// `JSONDecodeError`: the bare message and its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodeError {
    /// `err.msg`.
    pub(crate) msg: String,
    /// `err.lineno`.
    pub(crate) lineno: usize,
    /// `err.colno`.
    pub(crate) colno: usize,
}

enum Fail {
    /// `StopIteration(idx)`: "Expecting value" at the top.
    Stop(usize),
    /// `raise_errmsg(msg, pos)`.
    Msg(&'static str, usize),
}

fn ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn error(s: &[char], msg: &str, pos: usize) -> DecodeError {
    let before = &s[..pos.min(s.len())];
    let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
    let colno = match before.iter().rposition(|c| *c == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    DecodeError {
        msg: msg.to_owned(),
        lineno,
        colno,
    }
}

/// `json.loads(text)`.
pub(crate) fn loads(text: &str) -> Result<PyValue, DecodeError> {
    let s: Vec<char> = text.chars().collect();
    if s.first() == Some(&'\u{feff}') {
        return Err(error(
            &s,
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            0,
        ));
    }
    let skip = |mut i: usize| {
        while i < s.len() && ws(s[i]) {
            i += 1;
        }
        i
    };
    let (value, end) = match scan(&s, skip(0)) {
        Ok(v) => v,
        Err(Fail::Stop(i)) => return Err(error(&s, "Expecting value", i)),
        Err(Fail::Msg(m, i)) => return Err(error(&s, m, i)),
    };
    let end = skip(end);
    if end != s.len() {
        return Err(error(&s, "Extra data", end));
    }
    Ok(value)
}

fn word(s: &[char], i: usize, w: &str) -> bool {
    let w: Vec<char> = w.chars().collect();
    s.len() >= i + w.len() && s[i..i + w.len()] == w[..]
}

/// `scan_once_unicode`: the value at `i` and the index after it.
fn scan(s: &[char], i: usize) -> Result<(PyValue, usize), Fail> {
    let Some(&c) = s.get(i) else {
        return Err(Fail::Stop(i));
    };
    let constant: &[(&str, char)] = &[
        ("null", 'n'),
        ("true", 't'),
        ("false", 'f'),
        ("NaN", 'N'),
        ("Infinity", 'I'),
        ("-Infinity", '-'),
    ];
    match c {
        '"' => return string(s, i + 1).map(|(v, e)| (PyValue::Str(v), e)),
        '{' => return object(s, i + 1),
        '[' => return array(s, i + 1),
        _ => {}
    }
    for (w, first) in constant {
        if c == *first && word(s, i, w) {
            let v = match *w {
                "null" => PyValue::Null,
                "true" => PyValue::Bool(true),
                "false" => PyValue::Bool(false),
                "NaN" => PyValue::Float(f64::NAN),
                "Infinity" => PyValue::Float(f64::INFINITY),
                _ => PyValue::Float(f64::NEG_INFINITY),
            };
            return Ok((v, i + w.chars().count()));
        }
    }
    number(s, i)
}

/// `_match_number_unicode`.
fn number(s: &[char], start: usize) -> Result<(PyValue, usize), Fail> {
    let digit = |i: usize| s.get(i).is_some_and(char::is_ascii_digit);
    let mut i = start;
    if s[i] == '-' {
        i += 1;
        if i >= s.len() {
            return Err(Fail::Stop(start));
        }
    }
    match s[i] {
        '1'..='9' => {
            i += 1;
            while digit(i) {
                i += 1;
            }
        }
        '0' => i += 1,
        _ => return Err(Fail::Stop(start)),
    }
    let mut float = false;
    if s.get(i) == Some(&'.') && digit(i + 1) {
        float = true;
        i += 2;
        while digit(i) {
            i += 1;
        }
    }
    if i + 1 < s.len() && matches!(s[i], 'e' | 'E') {
        let e_start = i;
        i += 1;
        if i + 1 < s.len() && matches!(s[i], '-' | '+') {
            i += 1;
        }
        while digit(i) {
            i += 1;
        }
        if digit(i - 1) {
            float = true;
        } else {
            i = e_start;
        }
    }
    let text: String = s[start..i].iter().collect();
    let v = if float {
        PyValue::Float(text.parse().unwrap_or(f64::NAN))
    } else {
        PyValue::Int(match text.parse::<i64>() {
            Ok(n) => PyInt::Small(n),
            Err(_) => PyInt::Big(text),
        })
    };
    Ok((v, i))
}

fn hex4(s: &[char], at: usize) -> Option<u32> {
    s.get(at..at + 4)?
        .iter()
        .try_fold(0u32, |acc, c| Some(acc * 16 + c.to_digit(16)?))
}

/// `scanstring_unicode` (strict): the text and the index after the quote.
fn string(s: &[char], end: usize) -> Result<(String, usize), Fail> {
    let begin = end - 1;
    let unterminated = Fail::Msg("Unterminated string starting at", begin);
    let mut out = String::new();
    let mut end = end;
    loop {
        let mut next = end;
        while next < s.len() && s[next] != '"' && s[next] != '\\' {
            if u32::from(s[next]) <= 0x1f {
                return Err(Fail::Msg("Invalid control character at", next));
            }
            out.push(s[next]);
            next += 1;
        }
        if next == s.len() {
            return Err(unterminated);
        }
        if s[next] == '"' {
            return Ok((out, next + 1));
        }
        next += 1;
        let Some(&c) = s.get(next) else {
            return Err(unterminated);
        };
        if c != 'u' {
            end = next + 1;
            let ch = match c {
                '"' | '\\' | '/' => c,
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return Err(Fail::Msg("Invalid \\escape", end - 2)),
            };
            out.push(ch);
            continue;
        }
        next += 1;
        end = next + 4;
        if end >= s.len() {
            return Err(Fail::Msg("Invalid \\uXXXX escape", next - 1));
        }
        let bad = Fail::Msg("Invalid \\uXXXX escape", end - 5);
        let mut cp = hex4(s, next).ok_or(bad)?;
        if (0xd800..=0xdbff).contains(&cp)
            && end + 6 < s.len()
            && s[end] == '\\'
            && s[end + 1] == 'u'
        {
            let c2 = hex4(s, end + 2).ok_or(Fail::Msg("Invalid \\uXXXX escape", end + 1))?;
            if (0xdc00..=0xdfff).contains(&c2) {
                cp = 0x10000 + ((cp - 0xd800) << 10 | (c2 - 0xdc00));
                end += 6;
            }
        }
        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
    }
}

fn skip_ws(s: &[char], mut i: usize) -> usize {
    while i < s.len() && ws(s[i]) {
        i += 1;
    }
    i
}

/// `_parse_object_unicode` from just after `{`.
fn object(s: &[char], i: usize) -> Result<(PyValue, usize), Fail> {
    let mut pairs: Vec<(String, PyValue)> = Vec::new();
    let mut i = skip_ws(s, i);
    if s.get(i) != Some(&'}') {
        loop {
            if s.get(i) != Some(&'"') {
                return Err(Fail::Msg(
                    "Expecting property name enclosed in double quotes",
                    i,
                ));
            }
            let (key, next) = string(s, i + 1)?;
            i = skip_ws(s, next);
            if s.get(i) != Some(&':') {
                return Err(Fail::Msg("Expecting ':' delimiter", i));
            }
            i = skip_ws(s, i + 1);
            let (val, next) = scan(s, i)?;
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = val,
                None => pairs.push((key, val)),
            }
            i = skip_ws(s, next);
            if s.get(i) == Some(&'}') {
                break;
            }
            if s.get(i) != Some(&',') {
                return Err(Fail::Msg("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = skip_ws(s, i + 1);
            if s.get(i) == Some(&'}') {
                return Err(Fail::Msg(
                    "Illegal trailing comma before end of object",
                    comma,
                ));
            }
        }
    }
    Ok((PyValue::Dict(pairs), i + 1))
}

/// `_parse_array_unicode` from just after `[`.
fn array(s: &[char], i: usize) -> Result<(PyValue, usize), Fail> {
    let mut items = Vec::new();
    let mut i = skip_ws(s, i);
    if s.get(i) != Some(&']') {
        loop {
            let (val, next) = scan(s, i)?;
            items.push(val);
            i = skip_ws(s, next);
            if s.get(i) == Some(&']') {
                break;
            }
            if s.get(i) != Some(&',') {
                return Err(Fail::Msg("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = skip_ws(s, i + 1);
            if s.get(i) == Some(&']') {
                return Err(Fail::Msg(
                    "Illegal trailing comma before end of array",
                    comma,
                ));
            }
        }
    }
    Ok((PyValue::List(items), i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> (String, usize, usize) {
        let e = loads(text).expect_err("an error");
        (e.msg, e.lineno, e.colno)
    }

    // Expected values from CPython 3.14.8 `json.loads`.
    #[test]
    fn loads_matches_python_errors() {
        assert_eq!(err(""), ("Expecting value".into(), 1, 1));
        assert_eq!(
            err("{\"a\": 1,}"),
            ("Illegal trailing comma before end of object".into(), 1, 8)
        );
        assert_eq!(err("{\"a\" 1}"), ("Expecting ':' delimiter".into(), 1, 6));
        assert_eq!(
            err("{\n  'a': 1}"),
            (
                "Expecting property name enclosed in double quotes".into(),
                2,
                3
            )
        );
        assert_eq!(err("[1 2]"), ("Expecting ',' delimiter".into(), 1, 4));
        assert_eq!(err("{} x"), ("Extra data".into(), 1, 4));
        assert_eq!(
            err("\"ab"),
            ("Unterminated string starting at".into(), 1, 1)
        );
        assert_eq!(err("\"a\\qb\""), ("Invalid \\escape".into(), 1, 3));
        assert_eq!(
            err("\"a\tb\""),
            ("Invalid control character at".into(), 1, 3)
        );
        assert_eq!(err("[-]"), ("Expecting value".into(), 1, 2));
        let Ok(PyValue::Dict(pairs)) = loads("{\"a\": 1, \"b\": NaN, \"a\": 2.5e0}") else {
            panic!("a dict");
        };
        assert_eq!(pairs[0], ("a".into(), PyValue::Float(2.5)));
        assert!(matches!(pairs[1].1, PyValue::Float(f) if f.is_nan()));
        assert_eq!(pairs.len(), 2);
    }
}
