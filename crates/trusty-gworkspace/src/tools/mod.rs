//! MCP `tools/list` response — JSON Schema for every exposed tool.
//!
//! Why: Claude Code (and any MCP client) needs a machine-readable contract
//! describing what arguments each tool accepts so the model can fill them
//! correctly. Grouping the schemas by service (calendar, gmail, drive, docs,
//! sheets, slides, tasks) keeps each module focused and easy to grep next to
//! the dispatcher.
//! What: `tool_list_response()` concatenates every service group into a JSON
//! object of the shape `{"tools": [{"name", "description", "inputSchema"}, ...]}`.
//! Test: Unit tests below assert the tool count, required fields, and name
//! uniqueness across the assembled registry.

mod accounts;
mod calendar;
mod docs;
mod drive;
mod gmail;
mod schema;
mod sheets;
mod slides;
mod tasks;

use serde_json::{Value, json};

/// Build the full `tools/list` response.
///
/// Why: One function = one source of truth for the MCP contract.
/// What: Concatenates all 40+ tools across accounts, calendar, gmail, drive,
/// docs, sheets, slides, tasks.
/// Test: `tool_list_has_expected_count` asserts >= 40 tools.
pub fn tool_list_response() -> Value {
    let mut tools = Vec::<Value>::new();

    accounts::append(&mut tools);
    calendar::append(&mut tools);
    gmail::append(&mut tools);
    drive::append(&mut tools);
    docs::append(&mut tools);
    sheets::append(&mut tools);
    slides::append(&mut tools);
    tasks::append(&mut tools);

    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_expected_count() {
        let v = tool_list_response();
        let tools = v["tools"].as_array().expect("tools array");
        assert!(
            tools.len() >= 40,
            "expected >= 40 tools, got {}",
            tools.len()
        );
        for t in tools {
            assert!(t["name"].is_string(), "every tool has a name");
            assert!(t["description"].is_string(), "every tool has a description");
            assert!(
                t["inputSchema"]["type"] == "object",
                "every tool has object inputSchema"
            );
        }
    }

    #[test]
    fn every_tool_name_is_unique() {
        use std::collections::HashSet;
        let v = tool_list_response();
        let mut seen = HashSet::new();
        for t in v["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap().to_string();
            assert!(seen.insert(name.clone()), "duplicate tool: {name}");
        }
    }
}
