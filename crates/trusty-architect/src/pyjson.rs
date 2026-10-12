//! JSON text in the exact shape Python's `json.dumps` writes.
//!
//! Why: the poller's files (`alerts.jsonl`, `poll-state.json`) are read by
//! other prototype tools and compared byte for byte in the A2 parity corpus
//! (#9171). `serde_json` writes `{"a":1}`; Python writes `{"a": 1}`, and with
//! the default `ensure_ascii=True` it also escapes every non-ASCII character.
//! What: [`to_python_json`] serializes with `serde_json` (struct field order is
//! kept) and rewrites the compact text: `", "` and `": "` separators outside
//! strings, and, when `ensure_ascii`, `\uXXXX` escapes (UTF-16 surrogate pairs
//! above U+FFFF, lowercase hex) for every character outside `' '..='~'`.
//! Floats keep `serde_json`'s spelling, which equals Python's `repr` for the
//! values the scripts write (seconds since the epoch, small integers as floats).
//! Test: `python_separators_and_ascii_escapes`.

use serde::Serialize;

/// `json.dumps(value)` (`ensure_ascii=true`) or
/// `json.dumps(value, ensure_ascii=False)` (`ensure_ascii=false`).
///
/// # Errors
/// Whatever `serde_json` reports for a value it cannot serialize.
pub fn to_python_json<T: Serialize + ?Sized>(
    value: &T,
    ensure_ascii: bool,
) -> Result<String, serde_json::Error> {
    let compact = serde_json::to_string(value)?;
    let mut out = String::with_capacity(compact.len() + compact.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            push_char(&mut out, c, ensure_ascii);
            continue;
        }
        out.push(c);
        match c {
            '"' => in_string = true,
            ',' | ':' => out.push(' '),
            _ => {}
        }
    }
    Ok(out)
}

/// Appends one string-body character, escaping it when Python would.
fn push_char(out: &mut String, c: char, ensure_ascii: bool) {
    // serde_json already escapes `"`, `\` and C0 controls exactly as Python
    // does; only DEL and non-ASCII differ, and only under ensure_ascii.
    if !ensure_ascii || (' '..='~').contains(&c) || c < ' ' {
        out.push(c);
        return;
    }
    let mut units = [0u16; 2];
    for unit in c.encode_utf16(&mut units) {
        out.push_str(&format!("\\u{unit:04x}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_separators_and_ascii_escapes() -> Result<(), serde_json::Error> {
        let v = serde_json::json!({"a": [1, "x, y: z"], "b": "\u{276f} \u{1f4c5}\u{7f}\n\""});
        let bs = '\\';
        let ascii = format!(
            r#"{{"a": [1, "x, y: z"], "b": "{bs}u276f {bs}ud83d{bs}udcc5{bs}u007f{bs}n{bs}""}}"#
        );
        assert_eq!(to_python_json(&v, true)?, ascii);
        assert_eq!(
            to_python_json(&v, false)?,
            "{\"a\": [1, \"x, y: z\"], \"b\": \"\u{276f} \u{1f4c5}\u{7f}\\n\\\"\"}"
        );
        Ok(())
    }
}
