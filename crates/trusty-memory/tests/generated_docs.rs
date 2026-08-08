//! The MCP tool section of `README.md` is generated from `tool_definitions()`,
//! not maintained by hand.
//!
//! Why (#5205 follow-up): the README said "25 tools" against a real surface of
//! 45, and listed a subset by hand with the rest named in a prose sentence
//! that also drifted. This crate ships no `CLAUDE.md`, so `README.md` is the
//! only target.
//! What: renders the section from the real descriptor function and asserts the
//! checked-in region matches. With `UPDATE_DOCS=1` it rewrites the region.
//! Test: this file, plus `tools::tests::tool_definitions_lists_all_tools`,
//! which owns the required-roster half of the contract. Regenerate with
//! `UPDATE_DOCS=1 cargo test -p trusty-memory --test generated_docs`.

use std::path::Path;

use trusty_common::descriptor_source;
use trusty_common::docgen::{assert_region, count_note, render_tool_section, tool_rows};
use trusty_memory::tools::{tool_definitions, tool_definitions_with};

/// Marker id for the region.
const REGION: &str = "mcp-tools";

/// The command the failure message points a developer at.
const REGEN: &str = "UPDATE_DOCS=1 cargo test -p trusty-memory --test generated_docs";

/// Render the section body.
fn section() -> String {
    let rows = tool_rows(&tool_definitions()).expect("descriptor shape");
    render_tool_section(
        &descriptor_source!(trusty_memory::tools::tool_definitions),
        &count_note(&[("", rows.len())]),
        &rows,
    )
}

/// Why: `tool_definitions_with(true)` is the other candidate oracle — the shape
/// served when the daemon was started with `--palace <name>`. It only drops
/// `palace` from each tool's `required` array, so it never changes the roster
/// or any description, only the requiredness the `Arguments` column reflects.
/// The README documents the default shape, where `palace` is required.
#[test]
fn default_palace_variant_does_not_change_the_documented_roster() {
    let roster = |v: &serde_json::Value| -> Vec<(String, String)> {
        tool_rows(v)
            .expect("rows")
            .into_iter()
            .map(|r| (r.name, r.summary))
            .collect()
    };
    let base = roster(&tool_definitions());
    for has_default in [false, true] {
        assert_eq!(
            base,
            roster(&tool_definitions_with(has_default)),
            "has_default={has_default} changed the documented roster"
        );
    }
    assert_eq!(tool_definitions(), tool_definitions_with(false));
}

#[test]
fn readme_mcp_tool_section_is_generated() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert_region(&path, REGION, &section(), REGEN);
}
