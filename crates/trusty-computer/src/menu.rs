//! Menu invocation: the snapshot allowlist, the path deny list and the menu tree.
//!
//! Why: `invoke_menu` is the most reliable way to drive a Mac app, but every
//! app's menus also reach Quit, Close, Log Out, Shut Down and cross-app
//! surfaces (Services, Open With, Share, System Settings). #11695 lifts the
//! DENY on `invoke_menu` only behind an explicit grant.
//! Authority check: menu invocation is limited to the granted app (the
//! `invoke_menu` rule's `pid_in_scope`, the broker's window and tool checks),
//! and a path must be one [`allowed_paths`] listed in the window's last
//! snapshot (the broker's menu gate), so it is an allowlist, not a deny list.
//! What: [`allowed_paths`] walks a raw `get_window_state` result. It refuses
//! by position the first two menu-bar items, the Apple menu and the app menu
//! (Services, Hide, Quit, Settings live there in every language), and by label
//! every path holding a [`DENIED_ITEMS`] entry. [`path_allowed`] is the FILTER
//! check on a call's `path` (shape and labels, before the snapshot gate);
//! [`menu_paths`] is the `computer_read` view of [`allowed_paths`].
//! The labels are English; a localized item outside the first two menus that
//! means Close or Share is caught only if its label is listed (see #11695).
//! Test: `tests/menu_policy.rs`; live: `tests/live_menu.rs`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::policy::{PolicyError, Scope};

/// Item labels refused at any depth of a path, in [`normalize`] form.
///
/// A label equal to one of these, or starting with one and a space
/// (`Quit Safari`, `Close Window`), is denied.
pub const DENIED_ITEMS: [&str; 20] = [
    // Destructive: end the app, a window or the user's session.
    "quit",
    "close",
    "force quit",
    "log out",
    "shut down",
    "restart",
    "move to trash",
    "hide others",
    "show all",
    // Hand content to the printer or a caller-chosen file.
    "print",
    "export",
    "save as",
    // Cross-app: hand content or control to another app.
    "services",
    "open with",
    "share",
    "system settings",
    "system preferences",
    // The Apple menu and its contents.
    "apple",
    "\u{f8ff}",
    "recent items",
];
/// Most path segments the driver accepts (its `maxItems`).
pub const MAX_PATH_SEGMENTS: usize = 16;
/// Longest label the driver accepts (its `maxLength`), in code points.
pub const MAX_LABEL_CHARS: usize = 200;
/// Menu-bar items refused by position: the Apple menu, then the app menu.
pub const DENIED_TOP_POSITIONS: usize = 2;
/// Most menu paths [`menu_paths`] returns.
pub const MAX_MENU_PATHS: usize = 1000;
/// Roles that carry one segment of a menu path.
const ITEM_ROLES: [&str; 2] = ["AXMenuBarItem", "AXMenuItem"];

/// `label` trimmed, lowercased, whitespace collapsed, a trailing ellipsis dropped.
pub fn normalize(label: &str) -> String {
    let words: Vec<&str> = label.split_whitespace().collect();
    let joined = words.join(" ").to_lowercase();
    let trimmed = joined
        .trim_end_matches('\u{2026}')
        .trim_end_matches("...")
        .trim_end();
    trimmed.to_owned()
}

/// True when `label` names a [`DENIED_ITEMS`] entry.
///
/// Test: `denied_labels_match_prefix_case_and_ellipsis`.
pub fn denied(label: &str) -> bool {
    let label = normalize(label);
    DENIED_ITEMS.iter().any(|d| {
        label == *d
            || label
                .strip_prefix(d)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

fn bad_path(detail: impl Into<String>) -> PolicyError {
    PolicyError {
        code: "bad_menu_path",
        detail: detail.into(),
    }
}

/// The FILTER check of `invoke_menu`: `path` is a short list of labels, none denied.
///
/// Why: the driver resolves exactly the labels it is given, so a refused
/// label anywhere in the path refuses the call (#11695).
/// What: `bad_menu_path` unless `path` is 2 to [`MAX_PATH_SEGMENTS`]
/// non-blank strings of at most [`MAX_LABEL_CHARS`]; `denied_menu_path` when
/// any segment is [`denied`].
/// Postcondition: an `Ok` path has no denied segment.
/// Test: `menu_denied_path_refused`, `menu_path_shape_is_checked`.
pub fn path_allowed(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    let path = args
        .get("path")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .ok_or_else(|| bad_path("path must be a list of strings"))?;
    if path.len() < 2 || path.len() > MAX_PATH_SEGMENTS {
        return Err(bad_path(format!(
            "path must hold a menu and an item, at most {MAX_PATH_SEGMENTS} labels"
        )));
    }
    if path
        .iter()
        .any(|s| s.trim().is_empty() || s.chars().count() > MAX_LABEL_CHARS)
    {
        return Err(bad_path(format!(
            "each label must be 1 to {MAX_LABEL_CHARS} characters"
        )));
    }
    if let Some(hit) = path.iter().find(|s| denied(s)) {
        return Err(PolicyError {
            code: "denied_menu_path",
            detail: format!("{hit:?} is on the menu deny list"),
        });
    }
    Ok(())
}

/// `invoke_menu` names exactly an integer `pid`, an integer `window_id` and a `path`.
///
/// Why: the window ties the call to the grant's one window, and the driver's
/// `session` label is the broker's to set, not the agent's.
/// Test: `menu_path_shape_is_checked`.
pub fn menu_args_only(args: &Map<String, Value>, _: &Scope) -> Result<(), PolicyError> {
    if !args.get("window_id").is_some_and(Value::is_u64) {
        return Err(bad_path("invoke_menu must name an integer window_id"));
    }
    match args
        .keys()
        .find(|k| !matches!(k.as_str(), "pid" | "window_id" | "path"))
    {
        Some(extra) => Err(bad_path(format!("{extra} is not allowed"))),
        None => Ok(()),
    }
}

/// A hashable stand-in for an `element_index` / `parent_index` value.
fn key(value: Option<&Value>) -> String {
    value.map_or_else(|| "null".to_owned(), Value::to_string)
}

/// The label chain from the menu bar down to `element`, or `None` when it is
/// not a labelled menu item under an AXMenuBar, or its walk loops.
fn chain(element: &Value, by_index: &HashMap<String, &Value>) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut node = element;
    loop {
        let idx = key(node.get("element_index"));
        if seen.contains(&idx) || seen.len() > 4 * MAX_PATH_SEGMENTS {
            return None;
        }
        seen.push(idx.clone());
        let role = node.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "AXMenuBar" {
            out.reverse();
            return Some(out);
        }
        if ITEM_ROLES.contains(&role) {
            let label = node.get("label").and_then(Value::as_str).unwrap_or("");
            if label.trim().is_empty() {
                return None;
            }
            out.push((label.trim().to_owned(), idx.clone()));
        }
        let parent = node.get("parent_index")?;
        if parent.is_null() || key(Some(parent)) == idx {
            return None;
        }
        node = by_index.get(&key(Some(parent)))?;
    }
}

/// The menu-bar items of `elements`, in tree order: children of an AXMenuBar.
fn top_items(elements: &[Value], by_index: &HashMap<String, &Value>) -> Vec<String> {
    elements
        .iter()
        .filter(|e| e.get("role").and_then(Value::as_str) == Some("AXMenuBarItem"))
        .filter(|e| {
            let parent = by_index.get(&key(e.get("parent_index")));
            parent.and_then(|p| p.get("role")).and_then(Value::as_str) == Some("AXMenuBar")
        })
        .map(|e| key(e.get("element_index")))
        .collect()
}

/// The invocable menu paths of a raw `get_window_state` result, and whether
/// more than [`MAX_MENU_PATHS`] existed.
///
/// Why: the broker admits an `invoke_menu` path only when it is one of these,
/// so a label the deny list does not know (a localized "Dienste") is still
/// refused unless it sits in a menu the agent may use (#11695).
/// What: every labelled AXMenuBarItem / AXMenuItem under an AXMenuBar, as its
/// trimmed label path, in tree order. A path is left out when its menu-bar
/// item is one of the first [`DENIED_TOP_POSITIONS`] (the Apple menu and the
/// app menu, in any language), when any segment is [`denied`], or when it is
/// a bare menu-bar item or deeper than [`MAX_PATH_SEGMENTS`].
/// Postcondition: every returned path passes [`path_allowed`].
/// Test: `menu_paths_drop_denied_and_apple_menu`, `menu_paths_refuse_first_two_menus_by_position`,
/// `menu_paths_ignore_window_elements`.
pub fn allowed_paths(result: &Value) -> (Vec<Vec<String>>, bool) {
    let elements: &[Value] = result
        .pointer("/structuredContent/elements")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let by_index: HashMap<String, &Value> = elements
        .iter()
        .map(|e| (key(e.get("element_index")), e))
        .collect();
    let tops = top_items(elements, &by_index);
    let refused_tops = &tops[..tops.len().min(DENIED_TOP_POSITIONS)];
    let mut paths: Vec<Vec<String>> = Vec::new();
    for element in elements {
        let role = element.get("role").and_then(Value::as_str).unwrap_or("");
        if !ITEM_ROLES.contains(&role) {
            continue;
        }
        let Some(chain) = chain(element, &by_index) else {
            continue;
        };
        let Some((_, top)) = chain.first() else {
            continue;
        };
        // #11695: a top not among the menu-bar items (none listed) is refused too.
        if !tops.contains(top) || refused_tops.contains(top) {
            continue;
        }
        if chain.len() < 2 || chain.len() > MAX_PATH_SEGMENTS {
            continue;
        }
        if chain.iter().any(|(label, _)| denied(label)) {
            continue;
        }
        let labels: Vec<String> = chain.into_iter().map(|(l, _)| l).collect();
        if paths.contains(&labels) {
            continue;
        }
        if paths.len() == MAX_MENU_PATHS {
            return (paths, true);
        }
        paths.push(labels);
    }
    (paths, false)
}

/// The `computer_read` view of [`allowed_paths`]: `{"paths": [...], "truncated": bool}`.
///
/// Test: `menu_paths_drop_denied_and_apple_menu`.
pub fn menu_paths(result: &Value) -> Value {
    let (paths, truncated) = allowed_paths(result);
    json!({"paths": paths, "truncated": truncated})
}

/// The `path` of an `invoke_menu` call as trimmed labels, the form
/// [`allowed_paths`] lists; `None` when it is not a list of strings.
pub fn call_path(args: &Map<String, Value>) -> Option<Vec<String>> {
    args.get("path")?
        .as_array()?
        .iter()
        .map(|s| s.as_str().map(|s| s.trim().to_owned()))
        .collect()
}
