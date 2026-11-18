//! Python value semantics over `serde_json::Value`, as the spike oracle reads JSON.
//!
//! Why: the spike's checks test Python truthiness (`if args.get("prompt")`)
//! and `str.splitlines`; the port must decide the same inputs the same way.
//! What: [`truthy`] is Python `bool()` of a decoded JSON value;
//! [`splitlines`] is `str.splitlines()` without `keepends`.
//! Test: `truthiness_matches_python`, `splitlines_matches_python` below.

use serde_json::Value;

/// Python `bool(value)` for a value decoded by `json.loads`.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// True for the line boundaries Python `str.splitlines` splits on.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Python `str.splitlines()`: `\r\n` is one break and a trailing break adds no line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !is_line_break(c) {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
            chars.next();
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness_matches_python() {
        let falsy = [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ];
        for v in &falsy {
            assert!(!truthy(Some(v)), "{v} should be falsy");
        }
        for v in &[
            json!(true),
            json!(1),
            json!(-0.5),
            json!("x"),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(Some(v)), "{v} should be truthy");
        }
        assert!(!truthy(None));
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd\n"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\nx"), ["", "", "x"]);
        assert_eq!(splitlines("a\u{2028}b"), ["a", "b"]);
    }
}
