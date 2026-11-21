//! Tool allow list and argument filter for the cua-driver 0.23.2 surface.
//!
//! Why: the driver daemon runs with full desktop rights, so the broker is the
//! place that narrows what an agent may ask it to do (port of the spike's
//! `policy.py`, plan section 4 step 3).
//! What: every one of the 56 tools `cua-driver list-tools` reports has a
//! [`Rule`] in [`RULES`]: ALLOW, FILTER or DENY. A FILTER tool passes only once
//! every argument check accepts. A tool not in the table is denied. One change
//! from the spike: `invoke_menu` is DENY (plan section 1, item 3).
//! Test: `tests/spike_policy.rs`.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::pyvalue::truthy;

pub use crate::policy_rules::RULES;

/// The cua-driver version this table was written against.
pub const DRIVER_VERSION: &str = "0.23.2";
/// Arguments that make the daemon write a file at a caller-chosen path.
pub const PATH_WRITE_ARGS: [&str; 2] = ["debug_image_out", "screenshot_out_file"];
/// Maximum text an agent may type or set in one call, in code points.
pub const MAX_TEXT_CHARS: usize = 2000;
/// Chords that quit, close, hide or switch apps, or open system UI.
///
/// Keys are in [`canonical_key`] form; a chord holding every key of one of
/// these, in any order and with any extra keys, is refused.
pub const DENIED_CHORDS: [&[&str]; 7] = [
    &["cmd", "q"],
    &["cmd", "w"],
    &["cmd", "h"],
    &["cmd", "tab"],
    &["cmd", "space"],
    &["cmd", "esc", "option"],
    &["cmd", "ctrl", "q"],
];

/// How a tool is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Forwarded as is.
    Allow,
    /// Forwarded once every check accepts the arguments.
    Filter,
    /// Never forwarded.
    Deny,
}

/// A call the policy refuses; `code` is stable for audit and tests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct PolicyError {
    /// Stable refusal code, as the spike's `PolicyError.code`.
    pub code: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

impl PolicyError {
    fn new(code: &'static str, detail: impl Into<String>) -> PolicyError {
        PolicyError {
            code,
            detail: detail.into(),
        }
    }
}

/// The apps a grant covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Process ids a call may name.
    pub pids: BTreeSet<i64>,
    /// Bundle ids `launch_app` may name.
    pub bundle_ids: BTreeSet<String>,
}

/// One argument check of a FILTER rule.
pub type Check = fn(&Map<String, Value>, &Scope) -> Result<(), PolicyError>;

/// A tool's verdict, the reason for it and its argument checks.
#[derive(Debug)]
pub struct Rule {
    /// The tool name as `list-tools` prints it.
    pub tool: &'static str,
    /// ALLOW, FILTER or DENY.
    pub verdict: Verdict,
    /// Why the tool has this verdict.
    pub reason: &'static str,
    /// Checks run in order; the first refusal wins.
    pub checks: &'static [Check],
}

/// The rule for `tool`, if the table has one.
pub fn rule(tool: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.tool == tool)
}

/// Return the rule a call passes, or the refusal.
///
/// Why: the single gate before any call reaches the daemon.
/// What: unknown tool -> `unknown_tool`; DENY -> `denied_tool`; non-object
/// args -> `bad_args`; then each check of the rule in order.
/// Postcondition: an `Ok` rule is never DENY and every one of its checks
/// accepted `args`.
/// Test: `unknown_tool_is_denied`, `brief_named_tools_are_denied`,
/// `invoke_menu_is_denied`, `non_object_args_are_refused`; the `refuses!` and
/// `admits!` rows in `tests/spike_policy.rs`.
pub fn check_call(tool: &str, args: &Value, scope: &Scope) -> Result<&'static Rule, PolicyError> {
    let rule = rule(tool).ok_or_else(|| PolicyError::new("unknown_tool", tool))?;
    if rule.verdict == Verdict::Deny {
        return Err(PolicyError::new(
            "denied_tool",
            format!("{tool}: {}", rule.reason),
        ));
    }
    let args = args
        .as_object()
        .ok_or_else(|| PolicyError::new("bad_args", "args must be an object"))?;
    for check in rule.checks {
        check(args, scope)?;
    }
    debug_assert!(rule.verdict != Verdict::Deny);
    Ok(rule)
}

/// `args.get(key, default) != expected` in Python terms.
fn differs(args: &Map<String, Value>, key: &str, expected: &str) -> bool {
    args.get(key).is_some_and(|v| v.as_str() != Some(expected))
}

pub(crate) fn pid_in_scope(args: &Map<String, Value>, scope: &Scope) -> Result<(), PolicyError> {
    let pid = match args.get("pid") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n,
        _ => {
            return Err(PolicyError::new(
                "pid_required",
                "a scoped call must name an integer pid",
            ));
        }
    };
    if pid.as_i64().is_some_and(|p| scope.pids.contains(&p)) {
        return Ok(());
    }
    Err(PolicyError::new(
        "pid_out_of_scope",
        format!("pid {pid} is not in the grant scope"),
    ))
}

pub(crate) fn background_only(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if differs(args, "delivery_mode", "background") {
        return Err(PolicyError::new(
            "foreground_delivery",
            "delivery_mode must be background",
        ));
    }
    Ok(())
}

pub(crate) fn window_scope_only(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if differs(args, "scope", "window") {
        return Err(PolicyError::new("desktop_scope", "scope must be window"));
    }
    if args.contains_key("target") {
        return Err(PolicyError::new(
            "raw_target",
            "the target selector is not exposed",
        ));
    }
    Ok(())
}

pub(crate) fn no_modifiers(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if ["modifier", "modifiers"]
        .iter()
        .any(|k| truthy(args.get(*k)))
    {
        return Err(PolicyError::new(
            "modifier",
            "modified input needs foreground delivery",
        ));
    }
    Ok(())
}

pub(crate) fn element_addressed(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    // #11460: a token and a snapshot index together leave it open which one the daemon acts on.
    if args.contains_key("element_token")
        && (args.contains_key("snapshot_id") || args.contains_key("element_index"))
    {
        return Err(PolicyError::new(
            "dual_address",
            "name element_token or snapshot_id with element_index, not both",
        ));
    }
    let has_token = truthy(args.get("element_token"));
    let has_index = args.contains_key("element_index") && truthy(args.get("snapshot_id"));
    if !(has_token || has_index) {
        return Err(PolicyError::new(
            "pixel_address",
            "address the element by token or index",
        ));
    }
    if args.contains_key("x") || args.contains_key("y") {
        return Err(PolicyError::new(
            "pixel_address",
            "pixel coordinates are not allowed",
        ));
    }
    Ok(())
}

pub(crate) fn text_bounded(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    for key in ["text", "value"] {
        let ok = match args.get(key) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.chars().count() <= MAX_TEXT_CHARS,
            Some(_) => false,
        };
        if !ok {
            return Err(PolicyError::new(
                "text_too_long",
                format!("{key} must be a string of at most {MAX_TEXT_CHARS}"),
            ));
        }
    }
    Ok(())
}

/// The driver's name for `key`, lowercased, with every alias folded in.
///
/// Why: cua-driver 0.23.2 accepts `cmd/command`, `option/alt`, `ctrl/control`
/// and `escape` (its `hotkey` help text), so `["command","q"]` must meet the
/// same deny as `["cmd","q"]`.
/// Test: `chord_aliases_order_and_supersets_are_denied`.
pub fn canonical_key(key: &str) -> String {
    let lower = key.trim().to_lowercase();
    match lower.as_str() {
        "command" | "\u{2318}" | "meta" | "super" | "win" | "windows" => "cmd",
        "opt" | "alt" | "\u{2325}" => "option",
        "control" | "\u{2303}" | "^" => "ctrl",
        "\u{21e7}" => "shift",
        "escape" | "\u{238b}" => "esc",
        "spacebar" => "space",
        "\u{21e5}" => "tab",
        _ => return lower,
    }
    .to_owned()
}

/// Refuse a chord of `keys` holding every key of a [`DENIED_CHORDS`] entry.
///
/// What: keys are folded by [`canonical_key`] and compared as a set, so the
/// order does not matter and a superset (`cmd+shift+q`) is refused too.
/// Test: `chord_aliases_order_and_supersets_are_denied`.
fn chord_safe(keys: &[&str]) -> Result<(), PolicyError> {
    let chord: BTreeSet<String> = keys.iter().map(|k| canonical_key(k)).collect();
    let denied = DENIED_CHORDS
        .iter()
        .any(|d| d.iter().all(|k| chord.contains(*k)));
    if denied {
        let joined: Vec<&str> = chord.iter().map(String::as_str).collect();
        return Err(PolicyError::new("denied_chord", joined.join("+")));
    }
    Ok(())
}

fn bad_keys(detail: &str) -> PolicyError {
    PolicyError::new("bad_keys", detail)
}

pub(crate) fn hotkey_safe(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    let keys = args
        .get("keys")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .ok_or_else(|| bad_keys("keys must be a list of strings"))?;
    chord_safe(&keys)
}

pub(crate) fn press_key_safe(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_keys("key must be a string"))?;
    let list = "keys must be a list of strings";
    // Python `[*mods, key]` over `args.get("modifiers") or []`.
    let mut keys: Vec<&str> = match args.get("modifiers") {
        m if !truthy(m) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad_keys(list))?,
        Some(Value::Object(o)) => o.keys().map(String::as_str).collect(),
        Some(Value::String(s)) => s
            .char_indices()
            .map(|(i, c)| &s[i..i + c.len_utf8()])
            .collect(),
        _ => return Err(bad_keys(list)),
    };
    keys.push(key);
    chord_safe(&keys)
}

pub(crate) fn launch_in_scope(args: &Map<String, Value>, scope: &Scope) -> Result<(), PolicyError> {
    let bundle = args.get("bundle_id");
    let in_scope = bundle
        .and_then(Value::as_str)
        .is_some_and(|b| scope.bundle_ids.contains(b));
    if !in_scope {
        let shown = bundle.map_or_else(|| "None".to_owned(), Value::to_string);
        return Err(PolicyError::new(
            "app_out_of_scope",
            format!("bundle_id {shown} is not in scope"),
        ));
    }
    for key in [
        "name",
        "urls",
        "additional_arguments",
        "webkit_inspector_port",
    ] {
        if args.contains_key(key) {
            return Err(PolicyError::new(
                "launch_arg",
                format!("{key} is not allowed"),
            ));
        }
    }
    Ok(())
}

pub(crate) fn no_prompt(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if truthy(args.get("prompt")) || truthy(args.get("probe_direct_capture")) {
        return Err(PolicyError::new(
            "tcc_prompt",
            "a tool call may not raise a TCC prompt",
        ));
    }
    Ok(())
}

pub(crate) fn no_screenshot(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if truthy(args.get("include_screenshot")) {
        return Err(PolicyError::new("screenshot", "screenshots are off"));
    }
    if differs(args, "capture_mode", "ax") {
        return Err(PolicyError::new("screenshot", "capture_mode must be ax"));
    }
    Ok(())
}

pub(crate) fn no_paths(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    // PATH_WRITE_ARGS is sorted, so the first hit is `sorted(hit)[0]`.
    match PATH_WRITE_ARGS.iter().find(|k| args.contains_key(**k)) {
        Some(hit) => Err(PolicyError::new(
            "path_write",
            format!("{hit} writes a file at a caller path"),
        )),
        None => Ok(()),
    }
}
