//! Account-management tool definitions.
//!
//! Why: Groups the Google Workspace account/profile tools so the top-level
//! registry stays a thin dispatcher.
//! What: Appends `list_accounts`, `set_default_account`, `remove_account`,
//! and `add_account` to the shared registry vector (issue #3503).
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, tool};
use serde_json::{Value, json};

/// Append the accounts tool group to the registry.
///
/// Why: Keeps account tools colocated and testable via the aggregate.
/// What: Pushes `list_accounts`, `set_default_account`, `remove_account`, and
/// `add_account` onto `tools`.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "list_accounts",
        "List configured Google Workspace account profiles available on this machine.",
        json!({ "account": account_schema() }),
        &[],
    ));
    tools.push(tool(
        "set_default_account",
        "Set which configured Google Workspace account profile is used by default when a tool \
         call omits `account`.",
        json!({
            "name": {
                "type": "string",
                "description": "The profile name to make the default (must already exist — see list_accounts).",
            },
        }),
        &["name"],
    ));
    tools.push(tool(
        "remove_account",
        "Remove a configured Google Workspace account profile from local storage. Does not \
         revoke Google's grant. If the removed profile was the default, another remaining \
         profile is automatically promoted to default.",
        json!({
            "name": {
                "type": "string",
                "description": "The profile name to remove.",
            },
        }),
        &["name"],
    ));
    tools.push(tool(
        "add_account",
        "Authorize a new Google Workspace account profile (or re-authorize an existing one) via \
         OAuth consent. Returns a consent URL the user must open in a browser; this call blocks \
         briefly waiting for that consent to complete and reports whether it succeeded or timed \
         out (safe to retry either way — no partial state is ever stored).",
        json!({
            "profile": {
                "type": "string",
                "description": "Profile name to store the token under (default: the shared default profile name).",
            },
            "no_default": {
                "type": "boolean",
                "description": "Do NOT mark this profile as the default, even if none exists yet. Mutually exclusive with make_default.",
            },
            "make_default": {
                "type": "boolean",
                "description": "Explicitly make this profile the default, displacing any existing one. Mutually exclusive with no_default.",
            },
            "timeout_secs": {
                "type": "integer",
                "description": "How long to wait for the user to complete browser consent, in seconds (clamped to 10-90; default 60).",
            },
        }),
        &[],
    ));
}
