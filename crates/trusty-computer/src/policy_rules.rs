//! The 56-row rule table of [`crate::policy`], one row per cua-driver 0.23.2 tool.
//!
//! Why: kept apart from the checks so the table reads as the spike's `RULES`
//! dict (`policy.py:161-231`) row for row.
//! What: 6 ALLOW, 13 FILTER and 37 DENY rows. `invoke_menu` is DENY here; the
//! spike had it FILTER (plan section 1, item 3).
//! Test: `every_inventory_tool_is_classified`, `verdict_counts_match_the_plan`
//! in `tests/spike_policy.rs`.

use crate::policy::{
    Check, Rule, Verdict, background_only, element_addressed, hotkey_safe, launch_in_scope,
    no_modifiers, no_paths, no_prompt, no_screenshot, pid_in_scope, press_key_safe, text_bounded,
    window_scope_only,
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
    // #11460: every menu holds Restart, Shut Down and Log Out; a path allowlist is deferred.
    deny("invoke_menu", "menus reach Restart, Shut Down and Log Out"),
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
