//! Page-op frames: field sets, validation, result shapes and the journal copy
//! (`pages.py`, `page_query.py`, `page_fields.py`).
//!
//! Why: a code session must refuse a malformed op before anything reaches the
//! worker, and must relay only results in the contract's shape; a byte that
//! differs from Python changes what an agent sees (contract "Field types").
//! What: [`validate`] runs Python's checks in Python's order and returns the
//! op name; [`controls`] fills the optional controls; [`shaped`] and
//! [`clicked`] check a worker result; [`remembered`] is the journal's copy of
//! a request; [`py_eq`] is Python `==` on JSON values, which the journal uses
//! to tell a retry from a `Conflict`.
//! Test: `validation_refuses_what_python_refuses` and the shape tests below;
//! `page_ops_replay_byte_identically` in `tests/parity_pages.rs`.

use std::sync::OnceLock;

use base64::Engine as _;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use sha2::{Digest, Sha256};

use crate::pages_url::floor_host;
use crate::pyjson::Json;
use crate::refusal::{MAX_STRING, Refusal, need_request_id, request_string};

/// Each op's required fields (`pages.OPS`), in Python's order.
pub const OPS: &[(&str, &[&str])] = &[
    ("tabs", &["op"]),
    ("select", &["op", "tab"]),
    ("page_read", &["op", "lease"]),
    ("page_click", &["op", "lease", "request_id", "ref"]),
    ("page_hover", &["op", "lease", "request_id", "ref"]),
    ("page_type", &["op", "lease", "request_id", "ref", "text"]),
    (
        "page_scroll",
        &["op", "lease", "request_id", "to", "ref", "wait_ms"],
    ),
    (
        "page_press",
        &["op", "lease", "request_id", "ref", "key", "modifiers"],
    ),
    (
        "page_select",
        &["op", "lease", "request_id", "ref", "value"],
    ),
    ("page_drag", &["op", "lease", "request_id", "ref", "target"]),
    ("page_navigate", &["op", "lease", "request_id", "url"]),
    (
        "page_upload",
        &["op", "lease", "request_id", "ref", "files"],
    ),
    ("page_downloads", &["op", "lease"]),
    ("page_find", &["op", "lease"]),
    ("page_back", &["op", "lease", "request_id"]),
    ("page_forward", &["op", "lease", "request_id"]),
    ("page_reload", &["op", "lease", "request_id"]),
    ("release", &["op", "lease"]),
    ("switch", &["op", "lease"]),
];

/// History ops: they act on the tab, not a document (`HISTORY`).
pub const HISTORY: &[&str] = &["page_back", "page_forward", "page_reload"];

/// An error that ends the agent session with no reply (a Python `ValueError`).
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// A refusal, answered as a frame.
    Refused(Refusal),
    /// Close the connection without a reply.
    Close,
}

impl From<Refusal> for Stop {
    fn from(r: Refusal) -> Stop {
        Stop::Refused(r)
    }
}

fn invalid() -> Refusal {
    Refusal::new("InvalidRequest")
}

/// The op's entry in [`OPS`].
pub fn op_entry(op: &str) -> Option<(&'static str, &'static [&'static str])> {
    OPS.iter().find(|(name, _)| *name == op).copied()
}

fn defaults(op: &str) -> Vec<(&'static str, Json)> {
    match op {
        "page_read" => vec![
            ("scope_ref", Json::Null),
            ("max_chars", Json::Int(50000)),
            ("viewport_only", Json::Bool(false)),
            ("include_text", Json::Bool(false)),
        ],
        "page_find" => vec![
            ("role", Json::Null),
            ("name", Json::Null),
            ("text", Json::Null),
            ("within_ref", Json::Null),
            ("limit", Json::Int(10)),
        ],
        "page_click" => vec![("dialog", Json::from("dismiss"))],
        _ => Vec::new(),
    }
}

fn str_within(v: &Json, max: usize) -> bool {
    v.as_str().is_some_and(|s| {
        let n = s.chars().count();
        n > 0 && n <= max
    })
}

fn get<'a>(fields: &'a [(&str, Json)], name: &str) -> Option<&'a Json> {
    fields.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// The op's controls with defaults filled, else `InvalidRequest` (`controls`).
///
/// Why: the worker refuses a malformed control, so the broker refuses it first
/// and sends nothing. What: fills defaults, then checks each control's type and
/// range as the worker's SPECS do.
/// Test: `validation_refuses_what_python_refuses`.
pub fn controls(request: &Json) -> Result<Vec<(&'static str, Json)>, Refusal> {
    let op = request.get("op").and_then(Json::as_str).unwrap_or("");
    let mut fields = defaults(op);
    for (k, v) in &mut fields {
        if let Some(given) = request.get(k) {
            *v = given.clone();
        }
    }
    for name in ["scope_ref", "within_ref"] {
        if let Some(r) = get(&fields, name)
            && *r != Json::Null
            && !str_within(r, MAX_STRING)
        {
            return Err(invalid());
        }
    }
    let terms = ["role", "name", "text"];
    for name in terms {
        if let Some(t) = get(&fields, name)
            && *t != Json::Null
            && !str_within(t, 200)
        {
            return Err(invalid());
        }
    }
    if let Some(limit) = get(&fields, "limit") {
        if !matches!(limit, Json::Int(n) if (1..=50).contains(n)) {
            return Err(invalid());
        }
        if terms.iter().all(|t| get(&fields, t) == Some(&Json::Null)) {
            return Err(invalid());
        }
    }
    if let Some(size) = get(&fields, "max_chars")
        && !matches!(size, Json::Int(n) if (1000..=250_000).contains(n))
    {
        return Err(invalid());
    }
    for name in ["viewport_only", "include_text"] {
        if get(&fields, name).is_some_and(|v| !matches!(v, Json::Bool(_))) {
            return Err(invalid());
        }
    }
    if let Some(d) = get(&fields, "dialog")
        && !matches!(d.as_str(), Some("accept" | "dismiss"))
    {
        return Err(invalid());
    }
    Ok(fields)
}

fn keycat() -> &'static [(u32, u32)] {
    static TABLE: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("keycat.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| {
                let (a, b) = l.split_once('\t')?;
                Some((
                    u32::from_str_radix(a, 16).ok()?,
                    u32::from_str_radix(b, 16).ok()?,
                ))
            })
            .collect()
    })
}

const NAMED_KEYS: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Space",
    " ",
];
const MODIFIERS: &[&str] = &["Shift", "Control", "Alt", "Meta"];

/// A named key, or one code point outside the C and Z categories (`key_allowed`).
pub fn key_allowed(key: &Json) -> bool {
    let Some(key) = key.as_str() else {
        return false;
    };
    if NAMED_KEYS.contains(&key) {
        return true;
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    let cp = u32::from(c);
    let table = keycat();
    let i = table.partition_point(|(start, _)| *start <= cp);
    !(i > 0 && table[i - 1].1 >= cp)
}

/// `base64.b64decode(data, validate=True)`: canonical padding, trailing bits allowed.
fn b64_len(data: &str) -> Option<usize> {
    let config = GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical);
    GeneralPurpose::new(&STANDARD, config)
        .decode(data)
        .ok()
        .map(|b| b.len())
}

/// 1-10 `{name, type, data}` entries whose base64 decodes within 512 KiB (`valid_files`).
pub fn valid_files(files: &Json) -> bool {
    let Json::Array(items) = files else {
        return false;
    };
    if items.is_empty() || items.len() > 10 {
        return false;
    }
    let mut total = 0;
    for item in items {
        if !crate::pairing::key_set_is(item, &["name", "type", "data"]) {
            return false;
        }
        let (Some(name), Some(kind), Some(data)) = (
            item.get("name").and_then(Json::as_str),
            item.get("type").and_then(Json::as_str),
            item.get("data").and_then(Json::as_str),
        ) else {
            return false;
        };
        let n = name.chars().count();
        if n == 0 || n > 255 || name.contains(['/', '\\']) || kind.chars().count() > 255 {
            return false;
        }
        match b64_len(data) {
            Some(len) => total += len,
            None => return false,
        }
    }
    total <= 512 * 1024
}

fn press_ok(request: &Json) -> bool {
    let mods = match request.get("modifiers") {
        Some(Json::Array(m)) => m,
        _ => return false,
    };
    let names: Vec<&str> = mods.iter().filter_map(Json::as_str).collect();
    let unique = names
        .iter()
        .enumerate()
        .all(|(i, m)| !names[..i].contains(m));
    request.get("key").is_some_and(key_allowed)
        && names.len() == mods.len()
        && names.iter().all(|m| MODIFIERS.contains(m))
        && unique
}

fn scroll_ok(request: &Json) -> bool {
    let to = request.get("to").and_then(Json::as_str);
    let r#ref = request.get("ref");
    let is_str = matches!(r#ref, Some(Json::Str(_)));
    let ref_ok =
        matches!(r#ref, Some(Json::Null)) || matches!(r#ref, Some(Json::Str(s)) if !s.is_empty());
    matches!(to, Some("down" | "up" | "top" | "bottom" | "element"))
        && (to == Some("element")) == is_str
        && ref_ok
        && matches!(request.get("wait_ms"), Some(Json::Int(w)) if (0..=5000).contains(w))
}

/// Exact fields and types per op; returns the op name (`validate`).
///
/// Why: contract "Field types": a malformed op is refused before it reaches
/// the worker. What: Python's checks in Python's order, so the same refusal
/// wins when a frame breaks several rules; a missing `request_id` names its field.
/// Test: `validation_refuses_what_python_refuses`.
pub fn validate(request: &Json) -> Result<&'static str, Refusal> {
    let op = request.get("op").and_then(Json::as_str);
    let Some((op, required)) = op.and_then(op_entry) else {
        return Err(invalid());
    };
    need_request_id(request, required)?;
    let optional = defaults(op);
    let Json::Object(pairs) = request else {
        return Err(invalid());
    };
    let known = |k: &str| required.contains(&k) || optional.iter().any(|(o, _)| *o == k);
    if !required.iter().all(|k| request.get(k).is_some()) || !pairs.iter().all(|(k, _)| known(k)) {
        return Err(invalid());
    }
    controls(request)?;
    for name in ["lease", "request_id", "ref", "target"] {
        let nullable = op == "page_scroll"
            || (op == "page_press" && matches!(request.get(name), None | Some(Json::Null)));
        if request.get(name).is_some() && !(name == "ref" && nullable) {
            request_string(request, name)?;
        }
    }
    if (op == "page_press" && !press_ok(request)) || (op == "page_scroll" && !scroll_ok(request)) {
        return Err(invalid());
    }
    if let Some(tab) = request.get("tab")
        && !matches!(tab, Json::Int(t) if *t >= 0)
    {
        return Err(invalid());
    }
    for name in ["text", "value"] {
        if let Some(item) = request.get(name)
            && item.as_str().is_none_or(|s| s.chars().count() > MAX_STRING)
        {
            return Err(invalid());
        }
    }
    if request.get("files").is_some_and(|f| !valid_files(f)) {
        return Err(invalid());
    }
    if let Some(url) = request.get("url") {
        if !str_within(url, MAX_STRING) {
            return Err(invalid());
        }
        if floor_host(Some(url)).is_none() {
            return Err(Refusal::new("InvalidUrl"));
        }
    }
    Ok(op)
}

/// One Python type a shaped field may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// `str`
    Str,
    /// `NoneType`
    Null,
    /// `bool`
    Bool,
    /// `int`
    Int,
    /// `float`
    Float,
    /// `list`
    List,
}

fn ty_of(v: &Json) -> Option<Ty> {
    Some(match v {
        Json::Str(_) => Ty::Str,
        Json::Null => Ty::Null,
        Json::Bool(_) => Ty::Bool,
        Json::Int(_) | Json::UInt(_) => Ty::Int,
        Json::Float(_) => Ty::Float,
        Json::Array(_) => Ty::List,
        _ => return None,
    })
}

/// A result shape: each key and its allowed types.
pub type Shape = &'static [(&'static str, &'static [Ty])];

const OPT: &[Ty] = &[Ty::Str, Ty::Null];
/// `PROBE` (`select`).
pub const PROBE: Shape = &[
    ("access", &[Ty::Bool]),
    ("document", OPT),
    ("origin", OPT),
    ("title", OPT),
];
/// `READ` (`page_read`).
pub const READ: Shape = &[
    ("document", &[Ty::Str]),
    ("origin", &[Ty::Str]),
    ("url", &[Ty::Str]),
    ("title", &[Ty::Str]),
    ("text", &[Ty::Str]),
    ("elements", &[Ty::List]),
    ("truncated", &[Ty::Bool]),
];
/// `FOUND` (`page_find`).
pub const FOUND: Shape = &[
    ("document", &[Ty::Str]),
    ("origin", &[Ty::Str]),
    ("matches", &[Ty::List]),
    ("truncated", &[Ty::Bool]),
];
/// `DONE`.
pub const DONE: Shape = &[("document", &[Ty::Str])];
/// `CUT`: a `page_type`/`page_press` that a maxlength cut (#11352).
pub const CUT: Shape = &[
    ("document", &[Ty::Str]),
    ("truncated", &[Ty::Bool]),
    ("typed", &[Ty::Int]),
    ("accepted", &[Ty::Int]),
];
/// `SCROLLED`.
pub const SCROLLED: Shape = &[
    ("document", &[Ty::Str]),
    ("scroll_y", &[Ty::Int]),
    ("scroll_height", &[Ty::Int]),
    ("viewport_height", &[Ty::Int]),
    ("grew", &[Ty::Bool]),
    ("at_end", &[Ty::Bool]),
];
/// `SELECTED` (#11285).
pub const SELECTED: Shape = &[
    ("document", &[Ty::Str]),
    ("changed", &[Ty::Bool]),
    ("value", &[Ty::Str]),
];
/// `SWITCHED` (#11293).
pub const SWITCHED: Shape = &[("window", &[Ty::Int]), ("focused", &[Ty::Bool])];
/// `DRAGGED` (#11288).
pub const DRAGGED: Shape = &[("document", &[Ty::Str]), ("dropped", &[Ty::Bool])];
/// `LANDED` (navigate and history).
pub const LANDED: Shape = &[
    ("access", &[Ty::Bool]),
    ("document", OPT),
    ("origin", OPT),
    ("settled", &[Ty::Bool]),
];
/// `LISTED` (`page_downloads`).
pub const LISTED: Shape = &[("origin", &[Ty::Str]), ("downloads", &[Ty::List])];
/// `DOWNLOAD`: one entry of `LISTED`.
pub const DOWNLOAD: Shape = &[
    ("id", &[Ty::Int]),
    ("path", &[Ty::Str]),
    ("url", &[Ty::Str]),
    ("mime", &[Ty::Str]),
    ("state", &[Ty::Str]),
    ("bytes", &[Ty::Int, Ty::Float]),
    ("total", &[Ty::Int, Ty::Float]),
    ("error", OPT),
    ("started", &[Ty::Str]),
];

/// `value` as an object holding exactly `shape`'s keys and types, else `BrowserRefused`.
pub fn shaped(value: &Json, shape: Shape) -> Result<Json, Refusal> {
    let keys: Vec<&str> = shape.iter().map(|(k, _)| *k).collect();
    let typed = shape.iter().all(|(k, allowed)| {
        value
            .get(k)
            .and_then(ty_of)
            .is_some_and(|t| allowed.contains(&t))
    });
    if crate::pairing::key_set_is(value, &keys) && typed {
        Ok(value.clone())
    } else {
        Err(Refusal::new("BrowserRefused"))
    }
}

/// A click result: `document`, plus a well-formed `dialog` when one opened (`clicked`).
pub fn clicked(raw: &Json) -> Result<Json, Refusal> {
    let refused = || Refusal::new("BrowserRefused");
    let Json::Object(pairs) = raw else {
        return Err(refused());
    };
    let others = pairs.iter().filter(|(k, _)| k.as_str() != "dialog");
    if others.map(|(k, _)| k.as_str()).collect::<Vec<_>>() != ["document"] {
        return Err(refused());
    }
    if !matches!(raw.get("document"), Some(Json::Str(_))) {
        return Err(refused());
    }
    if let Some(seen) = raw.get("dialog") {
        let kinds: Shape = &[
            ("type", &[Ty::Str]),
            ("message", &[Ty::Str]),
            ("accepted", &[Ty::Bool]),
        ];
        shaped(seen, kinds)?;
        let kind = seen.get("type").and_then(Json::as_str);
        let message = seen.get("message").and_then(Json::as_str).unwrap_or("");
        if !matches!(kind, Some("alert" | "confirm" | "prompt" | "beforeunload"))
            || message.chars().count() > 1024
        {
            return Err(refused());
        }
    }
    Ok(raw.clone())
}

/// The journal's copy of a request: each file's data becomes its SHA-256 (#11289).
pub fn remembered(request: &Json) -> Json {
    let Some(Json::Array(files)) = request.get("files") else {
        return request.clone();
    };
    let digests = files
        .iter()
        .map(|f| match f {
            Json::Object(_) => {
                let data = f.get("data").and_then(Json::as_str).unwrap_or("None");
                let mut out = f.clone();
                out.insert(
                    "data",
                    Json::from(hex::encode(Sha256::digest(data.as_bytes()))),
                );
                out
            }
            other => other.clone(),
        })
        .collect();
    let mut out = request.clone();
    out.insert("files", Json::Array(digests));
    out
}

fn number(v: &Json) -> Option<f64> {
    match v {
        Json::Bool(b) => Some(f64::from(u8::from(*b))),
        // Python compares an int with a float exactly; i64 to f64 is exact below 2^53.
        Json::Int(i) => Some(*i as f64),
        Json::UInt(u) => Some(*u as f64),
        Json::Float(f) => Some(*f),
        _ => None,
    }
}

/// Python `==` on decoded JSON: objects ignore key order, `1 == 1.0 == True`.
pub fn py_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| py_eq(v, w))
        }
        (Json::Int(x), Json::Int(y)) => x == y,
        _ => match (number(a), number(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
