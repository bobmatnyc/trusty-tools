//! The 56-row rule table of [`crate::policy`], one row per cua-driver 0.23.2 tool.
//!
//! Why: kept apart from the checks so the table reads as the spike's `RULES`
//! dict (`policy.py:161-231`) row for row.
//! What: 6 ALLOW, 14 FILTER and 36 DENY rows. `invoke_menu` is FILTER behind
//! a path deny list (#11695; it was DENY in Phase 2, plan section 1, item 3).
//! [`BROKER_RULES`] holds tools the broker serves itself, which no driver lists:
//! `screenshot_window` (#11694), `click_at` and `drag_at` (#11696), and
//! `focus_window`, `move_window` and `resize_window` (#11696 Phase B), and
//! `read_clipboard` and `write_clipboard` (#11696 Phase C).
//! Test: `every_inventory_tool_is_classified`, `verdict_counts_match_the_plan`
//! in `tests/spike_policy.rs`; `screenshot_rule_is_filter_and_not_default`.

use crate::policy::{
    Check, Rule, Verdict, background_only, element_addressed, hotkey_safe, launch_in_scope,
    no_modifiers, no_paths, no_prompt, no_screenshot, pid_in_scope, press_key_safe, text_bounded,
    window_only, window_scope_only,
};

const fn allow(tool: &'static str, reason: &'static str) -> Rule {
    Rule {
        tool,
        verdict: Verdict::Allow,
        reason,
        checks: &[],
    }
}

const fn filter(tool: &'static str, reason: &'static str, checks: &'static [Check]) -> Rule {
    Rule {
        tool,
        verdict: Verdict::Filter,
        reason,
        checks,
    }
}

const fn deny(tool: &'static str, reason: &'static str) -> Rule {
    Rule {
        tool,
        verdict: Verdict::Deny,
        reason,
        checks: &[],
    }
}

/// `_INPUT` plus element addressing, as the spike composes it for clicks.
const CLICK: &[Check] = &[
    pid_in_scope,
    background_only,
    window_scope_only,
    no_modifiers,
    element_addressed,
];

/// Every tool cua-driver 0.23.2 lists, with its verdict.
pub static RULES: [Rule; 56] = [
    // Read-only metadata with no screen content.
    allow("list_apps", "app names and pids only"),
    allow("get_screen_size", "display geometry"),
    allow("get_session", "content-free session status"),
    allow("list_sessions", "content-free session list"),
    allow("start_session", "lifecycle only"),
    allow("end_session", "lifecycle only"),
    filter(
        "check_permissions",
        "status only; prompt flags refused",
        &[no_prompt],
    ),
    // Observation scoped to a granted pid.
    filter(
        "list_windows",
        "window titles leak; pid must be in scope",
        &[pid_in_scope],
    ),
    filter(
        "get_window_state",
        "AX tree of a scoped pid, no screenshot",
        &[pid_in_scope, no_screenshot, no_paths],
    ),
    filter(
        "verify_state",
        "predicate check on a scoped window",
        &[pid_in_scope, no_screenshot],
    ),
    // Input, background and element-addressed only.
    filter(
        "click",
        "element click in a scoped pid",
        &[
            pid_in_scope,
            background_only,
            window_scope_only,
            no_modifiers,
            element_addressed,
            no_paths,
        ],
    ),
    filter("double_click", "as click", CLICK),
    filter("right_click", "as click", CLICK),
    filter("scroll", "scroll a scoped element", CLICK),
    filter(
        "set_value",
        "AX set on a scoped element",
        &[pid_in_scope, element_addressed, text_bounded],
    ),
    filter(
        "type_text",
        "AX text insert into a scoped element",
        &[
            pid_in_scope,
            background_only,
            window_scope_only,
            no_modifiers,
            element_addressed,
            text_bounded,
        ],
    ),
    filter(
        "press_key",
        "single key to a scoped pid; quit and switch chords refused",
        &[
            pid_in_scope,
            background_only,
            window_scope_only,
            press_key_safe,
        ],
    ),
    filter(
        "hotkey",
        "chord to a scoped pid; quit and switch chords refused",
        &[
            pid_in_scope,
            background_only,
            window_scope_only,
            hotkey_safe,
        ],
    ),
    filter(
        "launch_app",
        "background launch of a scoped bundle id only",
        &[launch_in_scope],
    ),
    // #11695: granted only by name; scoped pid and window, path deny list.
    filter(
        "invoke_menu",
        "menu path in a scoped pid; destructive and cross-app items refused",
        &[
            pid_in_scope,
            crate::menu::menu_args_only,
            crate::menu::path_allowed,
        ],
    ),
    // Denied: named by the brief.
    deny("kill_app", "kill -9 of any pid"),
    deny("clipboard_read", "returns privacy-sensitive clipboard text"),
    deny("bring_to_front", "steals focus from the user"),
    deny("set_config", "changes daemon config for every caller"),
    deny(
        "replay_trajectory",
        "replays arbitrary recorded tool calls, bypassing the filter",
    ),
    // Denied: added from the inventory.
    deny(
        "clipboard_write",
        "overwrites the user's clipboard; can read any absolute path",
    ),
    deny(
        "get_desktop_state",
        "full-display screenshot of the user's screen",
    ),
    deny(
        "get_accessibility_tree",
        "every window and app on the desktop, unscoped",
    ),
    deny("zoom", "screen capture; off until the grant layer exists"),
    deny(
        "get_cursor_position",
        "real pointer position; not needed for element clicks",
    ),
    deny("move_cursor", "can move a cursor; pixel path"),
    deny("drag", "pixel-only gesture"),
    deny(
        "set_window_frame",
        "moves and resizes windows on the user's screen",
    ),
    deny("start_recording", "screen recording to a caller path"),
    deny("stop_recording", "recording is denied"),
    deny(
        "get_recording_state",
        "recording is denied; leaks output path",
    ),
    deny(
        "check_for_update",
        "network call to GitHub; version is pinned",
    ),
    deny("install_ffmpeg", "downloads and installs a binary"),
    deny(
        "health_report",
        "diagnostics include capture probes; operator-only",
    ),
    deny("get_config", "operator-only"),
    deny("escalate_session", "deprecated capture-scope escalation"),
    deny("get_session_state", "deprecated alias"),
    deny("get_agent_cursor_state", "overlay control; not needed"),
    deny(
        "set_agent_cursor_enabled",
        "can hide the agent cursor indicator from the user",
    ),
    deny("set_agent_cursor_motion", "overlay control; not needed"),
    deny("set_agent_cursor_theme", "overlay control; not needed"),
    // Denied: browser family. trusty-browser owns browsers.
    deny("page", "legacy page tool runs JavaScript in a browser"),
    deny("browser_prepare", "attaches DevTools to a browser"),
    deny("get_browser_state", "reads page content"),
    deny("browser_click", "browser control"),
    deny("browser_dialog", "browser control"),
    deny("browser_download", "writes files"),
    deny("browser_navigate", "browser control"),
    deny("browser_pointer", "browser control"),
    deny("browser_set_input_files", "uploads local files"),
    deny("browser_type", "browser control"),
];

/// Broker tools: driven through a driver tool, never named to the daemon.
pub static BROKER_RULES: [Rule; 8] = [
    // #11694: driven as get_window_state with a screenshot; see `crate::screenshot`.
    filter(
        crate::screenshot::SCREENSHOT_TOOL,
        "image of the granted window only; refused when a password field shows",
        &[pid_in_scope, window_only],
    ),
    // #11696: driven as click / drag after a frame read; see `crate::pointer`.
    filter(
        crate::pointer::CLICK_AT_TOOL,
        "left click at a point inside the granted window's frame at event time",
        &[pid_in_scope, crate::pointer::click_args],
    ),
    filter(
        crate::pointer::DRAG_TOOL,
        "left drag between points inside the granted window's frame at event time",
        &[pid_in_scope, crate::pointer::drag_args],
    ),
    // #11696 Phase B: driven as bring_to_front / set_window_frame after an owner read.
    filter(
        crate::window_mgmt::FOCUS_TOOL,
        "bring the granted window of the granted pid to the front",
        &[pid_in_scope, crate::window_mgmt::focus_args],
    ),
    filter(
        crate::window_mgmt::MOVE_TOOL,
        "move the granted window of the granted pid, size kept",
        &[pid_in_scope, crate::window_mgmt::move_args],
    ),
    filter(
        crate::window_mgmt::RESIZE_TOOL,
        "resize the granted window of the granted pid, origin kept",
        &[pid_in_scope, crate::window_mgmt::resize_args],
    ),
    // #11696 Phase C: driven as clipboard_read / clipboard_write, text only.
    filter(
        crate::clipboard::READ_TOOL,
        "clipboard text, size-capped and redacted; grant flag clipboard_read",
        &[crate::clipboard::read_args],
    ),
    filter(
        crate::clipboard::WRITE_TOOL,
        "plain text to the clipboard, size-capped; grant flag clipboard_write",
        &[crate::clipboard::write_args],
    ),
];
