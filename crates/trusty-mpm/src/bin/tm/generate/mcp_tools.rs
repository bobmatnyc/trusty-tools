//! Renders `references/mcp-tools.md` from the in-process MCP tool catalog
//! (source #2 of the issue #2913 design-research brief).
//!
//! Why: `trusty_mpm::mcp::tools::tool_catalog()` is the best source in the
//! whole system — a single canonical, structured, in-memory table (name,
//! description, full JSON Schema) already used to build `tools/list`
//! responses. Calling it directly (in-process, same crate) can never drift
//! from what the MCP server actually advertises; no parsing required.
//! What: [`render`] renders trusty-mpm's own 31 tools in full — each with its
//! description and a one-line-per-parameter schema summary (required params
//! marked) — plus pointer paragraphs for the three sibling daemon MCP
//! surfaces (trusty-search, trusty-analyze, trusty-memory), which stay
//! out-of-scope for direct extraction (each lives in its own crate; pulling
//! them in would add a cross-crate dependency trusty-mpm does not otherwise
//! have — matches how `tm.md` already treats memory/search: pointer, not
//! inline enumeration).
//! Test: `mcp_tools_render_contains_known_tool`,
//! `mcp_tools_render_lists_all_catalog_tools`.

use std::fmt::Write as _;

use serde_json::Value;
use trusty_mpm::mcp::tools::tool_catalog;

/// Render the full MCP tool reference.
///
/// Why: an operator/agent debugging an MCP call needs the exact parameter
/// shape without round-tripping a live `tools/list` call.
/// What: one `##` section per tool (catalog order — already stable/versioned
/// in `TOOL_CATALOG`), followed by a pointer section for sibling daemons.
/// Test: `mcp_tools_render_contains_known_tool`,
/// `mcp_tools_render_lists_all_catalog_tools`.
pub(crate) fn render() -> String {
    let catalog = tool_catalog();
    let mut out = String::new();
    out.push_str("# MCP Tool Reference\n\n");
    out.push_str(
        "Generated from `trusty_mpm::mcp::tools::tool_catalog()` — trusty-mpm's \
         own MCP tool surface (`tools/list` over the `serve --stdio` bridge), \
         in catalog order. Regenerate with `tm generate capabilities`.\n\n",
    );
    let _ = writeln!(out, "{} tools.\n", catalog.len());

    for tool in &catalog {
        render_tool(tool, &mut out);
    }

    out.push_str("## Sibling Daemon MCP Surfaces\n\n");
    out.push_str(
        "Each sibling daemon owns its own tool catalog in its own crate — out of \
         scope for direct extraction here. See that daemon's own descriptor \
         source for its exact tool list:\n\n",
    );
    out.push_str("- **trusty-search** — `crates/trusty-search/src/mcp/tools/descriptors.rs`\n");
    out.push_str("- **trusty-analyze** — `crates/trusty-analyze/src/mcp/descriptors.rs`\n");
    out.push_str("- **trusty-memory** — `crates/trusty-memory/src/mcp_service.rs`\n");
    out
}

/// Render one tool descriptor as a `##` section with a parameter table.
fn render_tool(tool: &Value, out: &mut String) {
    let name = tool["name"].as_str().unwrap_or("<unnamed>");
    let description = tool["description"].as_str().unwrap_or("");
    let _ = writeln!(out, "## `{name}`\n");
    let _ = writeln!(out, "{description}\n");

    let schema = &tool["inputSchema"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    match schema["properties"].as_object() {
        Some(props) if !props.is_empty() => {
            out.push_str("| Parameter | Type | Required |\n|---|---|---|\n");
            let mut names: Vec<&String> = props.keys().collect();
            names.sort();
            for name in names {
                let prop = &props[name];
                let ty = prop["type"].as_str().unwrap_or("any");
                let req = if required.contains(&name.as_str()) {
                    "yes"
                } else {
                    "no"
                };
                let _ = writeln!(out, "| `{name}` | `{ty}` | {req} |");
            }
            out.push('\n');
        }
        _ => out.push_str("No parameters.\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tools_render_contains_known_tool() {
        let rendered = render();
        assert!(rendered.contains("## `session_list`"), "{rendered}");
        assert!(rendered.contains("## `agent_delegate`"), "{rendered}");
        assert!(rendered.contains("trusty-search"), "{rendered}");
    }

    #[test]
    fn mcp_tools_render_lists_all_catalog_tools() {
        let rendered = render();
        for tool in tool_catalog() {
            let name = tool["name"].as_str().unwrap();
            assert!(
                rendered.contains(&format!("## `{name}`")),
                "missing {name} in rendered output"
            );
        }
    }

    #[test]
    fn mcp_tools_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}
