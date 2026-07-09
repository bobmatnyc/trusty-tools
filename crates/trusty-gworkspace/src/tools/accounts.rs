//! Account-management tool definitions.
//!
//! Why: Groups the Google Workspace account/profile tools so the top-level
//! registry stays a thin dispatcher.
//! What: Appends the `list_accounts` tool to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, tool};
use serde_json::{Value, json};

/// Append the accounts tool group to the registry.
///
/// Why: Keeps account tools colocated and testable via the aggregate.
/// What: Pushes `list_accounts` onto `tools`.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "list_accounts",
        "List configured Google Workspace account profiles available on this machine.",
        json!({ "account": account_schema() }),
        &[],
    ));
}
