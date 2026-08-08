//! The MCP tool section of `README.md` and `CLAUDE.md` is generated from
//! `tool_descriptors()`, not maintained by hand.
//!
//! Why (#5205 follow-up): both files carried the same table, both said "18
//! tools" against a real surface of 21, and both cited a function
//! `tool_definitions` that does not exist in this crate. One generator call
//! now feeds both files, and the citation is compiler-checked.
//! What: renders the section from the real descriptor function and asserts the
//! checked-in region matches. With `UPDATE_DOCS=1` it rewrites the region
//! instead.
//! Test: this file. Regenerate with
//! `UPDATE_DOCS=1 cargo test -p trusty-search --test generated_docs`.

use std::path::Path;

use trusty_common::descriptor_source;
use trusty_common::docgen::{assert_region, count_note, render_tool_section, tool_rows};
use trusty_search::mcp::tools::{tool_descriptors, tool_descriptors_pinned};

/// Marker id shared by both target files.
const REGION: &str = "mcp-tools";

/// The command the failure message points a developer at.
const REGEN: &str = "UPDATE_DOCS=1 cargo test -p trusty-search --test generated_docs";

/// Render the section body once; both `README.md` and `CLAUDE.md` get it.
fn section() -> String {
    let rows = tool_rows(&tool_descriptors()).expect("descriptor shape");
    render_tool_section(
        &descriptor_source!(trusty_search::mcp::tools::tool_descriptors),
        &count_note(&[("", rows.len())]),
        &rows,
    )
}

/// Why: `tool_descriptors_pinned` is the other candidate oracle, so picking
/// `tool_descriptors` needs to be a proven choice rather than an assumption.
/// Pinned is a schema-level transform of the same list: it moves `index_id`
/// from required to optional and annotates that one property. It therefore
/// never changes the roster or any tool's own description — only the
/// requiredness the `Arguments` column reflects, which is why the README
/// documents the unpinned default that `serve` uses without `--index`.
#[test]
fn pinned_descriptors_do_not_change_the_documented_roster() {
    let roster = |v: &serde_json::Value| -> Vec<(String, String)> {
        tool_rows(v)
            .expect("rows")
            .into_iter()
            .map(|r| (r.name, r.summary))
            .collect()
    };
    let base = roster(&tool_descriptors());
    for pin in [None, Some("some-project")] {
        assert_eq!(
            base,
            roster(&tool_descriptors_pinned(pin)),
            "pinned={pin:?} changed the documented tool roster"
        );
    }
    // The unpinned variant is byte-identical, which is what makes
    // `tool_descriptors` — not `tool_descriptors_pinned(None)` — the oracle.
    assert_eq!(tool_descriptors(), tool_descriptors_pinned(None));
}

#[test]
fn readme_mcp_tool_section_is_generated() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert_region(&path, REGION, &section(), REGEN);
}

#[test]
fn claude_md_mcp_tool_section_is_generated() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("CLAUDE.md");
    assert_region(&path, REGION, &section(), REGEN);
}
