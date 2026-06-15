//! MCP tool catalog for the trusty-mpm orchestration + session-manager server.
//!
//! Why: `tools/list` must advertise a JSON Schema for every tool so Claude Code
//! knows how to call it. Keeping the catalog separate from the dispatch logic
//! makes the tool surface easy to audit and version. Since #1221 the catalog
//! spans two concepts — the original orchestration/bug-reporting tools
//! ([`core`]) and the new session-lifecycle tools ([`session`]) — so this is a
//! thin facade that re-exports both and concatenates their descriptors, keeping
//! each leaf file well under the 500-SLOC production cap.
//! What: [`tool_catalog`] builds the eighteen MCP tool descriptors (nine core +
//! six session + three console — #1222); [`TOOL_CATALOG`] lists their names for
//! tests and the startup log; [`tool`] is the shared descriptor builder used by
//! every submodule.
//! Test: the `tests` module below asserts the catalog has the expected count,
//! well-formed entries, and names matching [`TOOL_CATALOG`].

use serde_json::{Value, json};

pub mod console;
pub mod core;
pub mod session;

/// Canonical names of every tool the server exposes, in catalog order.
///
/// Why: tests, the daemon's startup log, and the loopback-`/rpc` audit all want
/// the authoritative list without re-parsing the JSON schema. Keeping it exact
/// resolves the #1221 review nit about ambiguous existing-vs-new counts: there
/// are exactly NINE pre-existing tools and SIX new session-lifecycle tools.
/// What: a static slice of the eighteen tool names — the six orchestration
/// tools, the three bug-reporting tools, the six session-lifecycle tools, then
/// the three console-facing tools (#1222).
/// Test: `catalog_names_match_constant`.
pub const TOOL_CATALOG: [&str; 18] = [
    // ── 9 pre-existing tools (core.rs) ───────────────────────────────────────
    "session_list",
    "session_status",
    "agent_delegate",
    "memory_protect",
    "circuit_breaker_status",
    "hook_event",
    "list_recent_errors",
    "preview_bug_report",
    "report_bug",
    // ── 6 new session-lifecycle tools (#1221, session.rs) ────────────────────
    "session_new",
    "session_stop",
    "session_resume",
    "session_decommission",
    "session_activity",
    "session_send",
    // ── 3 console-facing tools (#1222, console.rs) ───────────────────────────
    "console_metrics",
    "supervisor_status",
    "auto_resume_set",
];

/// Build the MCP tool descriptor list returned by `tools/list`.
///
/// Why: Claude Code reads `inputSchema` to validate calls; a single builder
/// keeps the schemas and the dispatch argument-parsing in lockstep across both
/// the core and session-lifecycle tool groups.
/// What: concatenates [`core::core_tools`] (nine descriptors),
/// [`session::session_tools`] (six descriptors), and [`console::console_tools`]
/// (three descriptors) in catalog order, returning eighteen
/// `{ name, description, inputSchema }` objects.
/// Test: `catalog_has_expected_tool_count` and `every_tool_has_input_schema`.
pub fn tool_catalog() -> Vec<Value> {
    let mut tools = core::core_tools();
    tools.extend(session::session_tools());
    tools.extend(console::console_tools());
    tools
}

/// Assemble one MCP tool descriptor object.
///
/// Why: every descriptor has the same `{ name, description, inputSchema }` shape;
/// a shared builder keeps the two tool-group modules terse and consistent.
/// What: returns a JSON object wrapping the three fields.
/// Test: exercised by every entry in `tool_catalog` via the tests below.
pub(crate) fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_expected_tool_count() {
        // 9 pre-existing + 6 session-lifecycle + 3 console-facing tools = 18.
        assert_eq!(tool_catalog().len(), 18);
        assert_eq!(TOOL_CATALOG.len(), 18);
    }

    #[test]
    fn catalog_names_match_constant() {
        let names: Vec<String> = tool_catalog()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, TOOL_CATALOG);
    }

    #[test]
    fn every_tool_has_input_schema() {
        for t in tool_catalog() {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn bug_reporting_tools_present() {
        let catalog = tool_catalog();
        let names: Vec<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_recent_errors"), "{names:?}");
        assert!(names.contains(&"preview_bug_report"), "{names:?}");
        assert!(names.contains(&"report_bug"), "{names:?}");
    }

    #[test]
    fn session_tools_present() {
        let catalog = tool_catalog();
        let names: Vec<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "session_new",
            "session_stop",
            "session_resume",
            "session_decommission",
            "session_activity",
            "session_send",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }

    #[test]
    fn console_tools_present() {
        let catalog = tool_catalog();
        let names: Vec<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in ["console_metrics", "supervisor_status", "auto_resume_set"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }
}
