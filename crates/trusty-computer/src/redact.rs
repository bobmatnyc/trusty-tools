//! Strip the app menu bar from a `get_window_state` reply, and fingerprint it.
//!
//! Why: a pid-scoped AX snapshot includes the app's AXMenuBar, whose Apple
//! menu lists Recent Items plus Restart, Shut Down and Log Out items an agent
//! could press by token. A depth cap does not help: the menu bar is a root
//! sibling of the window (spike `redact.py`, GO-STOP.md:62-71).
//! What: [`strip_menu_bar`] drops every element whose `parent_index` chain
//! reaches an AXMenuBar and returns the tokens it kept; [`strip_markdown`]
//! drops the same subtree from markdown by indentation; [`fingerprint`]
//! hashes the redacted tree so a click can be confirmed by a state change;
//! [`redact_reply`] applies all three to a whole daemon result.
//! Test: `tests/spike_redact.rs`.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::pyvalue::{splitlines, truthy};

/// Roles whose subtree is removed.
pub const MENU_ROLES: [&str; 1] = ["AXMenuBar"];

/// A redacted reply, the tokens it exposes, the count removed and its fingerprint.
#[derive(Debug, Clone, PartialEq)]
pub struct Redacted {
    /// The result with menu-bar elements and markdown removed.
    pub result: Value,
    /// `element_token`s of the elements that remain.
    pub tokens: BTreeSet<String>,
    /// Elements removed.
    pub removed: usize,
    /// [`fingerprint`] of the redacted structured part; empty when none.
    pub fingerprint: String,
}

/// Leading spaces of `line` (spaces only, as the spike's `_indent`).
fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The spike's `^\s*- (\[\d+\] )?AXMenuBar(\s|$)`.
fn is_menu_line(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("- ") else {
        return false;
    };
    let rest = match rest.strip_prefix('[') {
        Some(after) => {
            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            match after[digits..].strip_prefix("] ") {
                Some(tail) if digits > 0 => tail,
                _ => return false,
            }
        }
        None => rest,
    };
    rest.strip_prefix("AXMenuBar")
        .is_some_and(|tail| tail.chars().next().is_none_or(char::is_whitespace))
}

/// Remove each AXMenuBar line and every deeper-indented line under it.
///
/// Why: the text part and `tree_markdown` repeat the tree in markdown.
/// What: Python `str.splitlines`, then lines kept unless inside a cut
/// subtree, joined with `\n` (a trailing newline is dropped, as in the spike).
/// Test: `strip_markdown_resumes_after_subtree`.
pub fn strip_markdown(markdown: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut cut_at: Option<usize> = None;
    for line in splitlines(markdown) {
        let depth = indent(line);
        if cut_at.is_some_and(|c| depth > c) {
            continue;
        }
        cut_at = None;
        if is_menu_line(line) {
            cut_at = Some(depth);
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// A hashable stand-in for a Python dict key decoded from JSON.
fn key(value: Option<&Value>) -> String {
    value.map_or_else(|| "null".to_owned(), Value::to_string)
}

/// Python `str()` of a token value.
fn token_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// True when `element` is, or descends from, a menu-role element.
fn under_menu(
    element: &Value,
    by_index: &HashMap<String, &Value>,
    cache: &mut HashMap<String, bool>,
) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut node = Some(element);
    let mut verdict = false;
    while let Some(current) = node {
        let idx = key(current.get("element_index"));
        if let Some(hit) = cache.get(&idx) {
            verdict = *hit;
            break;
        }
        // A parent cycle would loop forever in the spike; here it ends the walk.
        if seen.contains(&idx) {
            break;
        }
        seen.push(idx.clone());
        let role = current.get("role").and_then(Value::as_str);
        if role.is_some_and(|r| MENU_ROLES.contains(&r)) {
            verdict = true;
            break;
        }
        node = match current.get("parent_index") {
            None | Some(Value::Null) => None,
            Some(parent) if key(Some(parent)) == idx => None,
            Some(parent) => by_index.get(&key(Some(parent))).copied(),
        };
    }
    for idx in seen {
        cache.insert(idx, verdict);
    }
    verdict
}

/// Return (redacted structured content, exposed tokens, elements removed).
///
/// Why: an agent must never see or press a menu-bar element.
/// What: drops elements under an AXMenuBar, strips `tree_markdown`, lowers
/// `element_count` / `returned_element_count` by the count removed (floor 0)
/// and records it as `redacted_menu_bar_elements`.
/// Postcondition: no kept element has a menu role or a menu ancestor, and the
/// tokens are exactly the truthy `element_token`s of kept elements.
/// Test: `redact_keeps_window_and_drops_menu_subtree`.
pub fn strip_menu_bar(
    structured: &Map<String, Value>,
) -> (Map<String, Value>, BTreeSet<String>, usize) {
    let elements: &[Value] = structured
        .get("elements")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let by_index: HashMap<String, &Value> = elements
        .iter()
        .map(|e| (key(e.get("element_index")), e))
        .collect();
    let mut cache = HashMap::new();
    let kept: Vec<Value> = elements
        .iter()
        .filter(|e| !under_menu(e, &by_index, &mut cache))
        .cloned()
        .collect();
    let removed = elements.len() - kept.len();
    let tokens: BTreeSet<String> = kept
        .iter()
        .filter_map(|e| e.get("element_token"))
        .filter(|t| truthy(Some(t)))
        .map(token_text)
        .collect();
    let mut out = structured.clone();
    out.insert("elements".to_owned(), Value::Array(kept));
    if let Some(Value::String(md)) = out.get("tree_markdown") {
        let stripped = strip_markdown(md);
        out.insert("tree_markdown".to_owned(), Value::String(stripped));
    }
    for count in ["element_count", "returned_element_count"] {
        if let Some(n) = out.get(count).and_then(Value::as_i64) {
            let lowered = n
                .saturating_sub(i64::try_from(removed).unwrap_or(i64::MAX))
                .max(0);
            out.insert(count.to_owned(), Value::from(lowered));
        }
    }
    out.insert(
        "redacted_menu_bar_elements".to_owned(),
        Value::from(removed),
    );
    (out, tokens, removed)
}

/// Python `repr` of a scalar JSON value, for the no-markdown fingerprint.
fn py_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(Value::String(s)) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::from(quote);
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
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Some(other) => other.to_string(),
    }
}

/// Stable 16-hex-digit hash of the redacted window tree.
///
/// Why: a click is confirmed only when the redacted tree changed.
/// What: SHA-256 of `tree_markdown`; without one, of the Python repr of
/// `(role, label, value)` per element (approximate for unusual strings).
/// Test: `fingerprint_ignores_menu_changes_but_sees_window_changes`.
pub fn fingerprint(structured: &Map<String, Value>) -> String {
    let text = match structured.get("tree_markdown") {
        Some(Value::String(s)) => s.clone(),
        _ => {
            let rows: Vec<String> = structured
                .get("elements")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|e| {
                    let f = |k: &str| py_repr(e.get(k));
                    format!("({}, {}, {})", f("role"), f("label"), f("value"))
                })
                .collect();
            format!("[{}]", rows.join(", "))
        }
    };
    let digest = hex::encode(Sha256::digest(text.as_bytes()));
    digest[..16].to_owned()
}

/// Redact a whole daemon `get_window_state` result (structured and text parts).
///
/// Why: the text content repeats the tree, so both parts must be stripped.
/// What: every text part of `content` is stripped. A result without an
/// object `structuredContent` then has no tokens, zero removed and an empty
/// fingerprint (the broker refuses such a snapshot); otherwise
/// `structuredContent` is replaced by [`strip_menu_bar`]'s output.
/// Test: `snapshot_strips_menu_bar_from_structured_and_text`,
/// `text_parts_are_stripped_without_a_structured_part`.
pub fn redact_reply(result: &Value) -> Redacted {
    let passthrough = || Redacted {
        result: result.clone(),
        tokens: BTreeSet::new(),
        removed: 0,
        fingerprint: String::new(),
    };
    let Some(obj) = result.as_object() else {
        return passthrough();
    };
    let mut out = obj.clone();
    // #11460: the text parts are stripped whether or not a structured part exists.
    if let Some(parts) = obj.get("content").and_then(Value::as_array) {
        let content: Vec<Value> = parts
            .iter()
            .map(|part| match part.get("text").and_then(Value::as_str) {
                Some(text) if part.is_object() => {
                    let mut part = part.clone();
                    part["text"] = Value::String(strip_markdown(text));
                    part
                }
                _ => part.clone(),
            })
            .collect();
        out.insert("content".to_owned(), Value::Array(content));
    }
    let Some(structured) = obj.get("structuredContent").and_then(Value::as_object) else {
        return Redacted {
            result: Value::Object(out),
            ..passthrough()
        };
    };
    let (structured, tokens, removed) = strip_menu_bar(structured);
    let fingerprint = fingerprint(&structured);
    out.insert("structuredContent".to_owned(), Value::Object(structured));
    Redacted {
        result: Value::Object(out),
        tokens,
        removed,
        fingerprint,
    }
}
