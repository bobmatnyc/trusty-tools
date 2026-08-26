//! The MCP tool section of `README.md` and `CLAUDE.md` is generated from the
//! descriptor functions, not maintained by hand.
//!
//! Why (#5205 follow-up): both files said "17 tools" and cited a function
//! `tool_definitions` that does not exist here. The real surface is also
//! feature-dependent — 19 tools by default, 22 with `--features review` — so a
//! section stating one number is false under the other configuration.
//! What: renders one table covering both configurations, with an `Available`
//! column, from `tool_descriptors()` plus the always-compiled
//! `descriptors::review_tool_descriptors()`.
//! Test: this file. Regenerate with
//! `UPDATE_DOCS=1 cargo test -p trusty-analyze --test generated_docs`.

use std::path::Path;

use trusty_analyze::mcp::{descriptors, tool_descriptors};
use trusty_common::descriptor_source;
use trusty_common::docgen::{
    assert_region, count_note, labelled, render_tool_section, tool_rows, ToolRow,
};

/// Marker id shared by both target files.
const REGION: &str = "mcp-tools";

/// The command the failure message points a developer at.
const REGEN: &str = "UPDATE_DOCS=1 cargo test -p trusty-analyze --test generated_docs";

/// Rows for the three `tr_review_*` tools, labelled with the feature that ships them.
fn review_rows() -> Vec<ToolRow> {
    let descs = serde_json::Value::Array(descriptors::review_tool_descriptors());
    labelled(
        tool_rows(&descs).expect("review descriptor shape"),
        "`--features review`",
    )
}

/// Rows for the tools every build serves.
///
/// Why: under `--features review` the dispatcher's `tool_descriptors()` already
/// contains the review tools. Subtracting them by name keeps the rendered
/// default set — and therefore the checked-in file — byte-identical in both
/// build configurations, which is the only way one committed table can be true
/// for both.
fn default_rows() -> Vec<ToolRow> {
    let review: Vec<String> = review_rows().into_iter().map(|r| r.name).collect();
    let rows = tool_rows(&tool_descriptors()).expect("descriptor shape");
    labelled(
        rows.into_iter()
            .filter(|r| !review.contains(&r.name))
            .collect(),
        "always",
    )
}

/// Render the section body once; both `README.md` and `CLAUDE.md` get it.
fn section() -> String {
    let default = default_rows();
    let review = review_rows();
    let note = count_note(&[
        ("with default features", default.len()),
        ("with `--features review`", default.len() + review.len()),
    ]);
    let mut rows = default;
    rows.extend(review);
    render_tool_section(
        &format!(
            "{} + {}",
            descriptor_source!(trusty_analyze::mcp::tool_descriptors),
            descriptor_source!(trusty_analyze::mcp::descriptors::review_tool_descriptors)
        ),
        &note,
        &rows,
    )
}

/// Why: the section claims 19 tools by default and 22 under the feature. This
/// asserts the claim against whichever build is running, so a `--features
/// review` run proves the second half and a default run proves the first.
#[test]
fn section_is_correct_for_this_build_configuration() {
    let live: Vec<String> = tool_rows(&tool_descriptors())
        .expect("descriptor shape")
        .into_iter()
        .map(|r| r.name)
        .collect();
    let default: Vec<String> = default_rows().into_iter().map(|r| r.name).collect();
    let review: Vec<String> = review_rows().into_iter().map(|r| r.name).collect();

    assert_eq!(review.len(), 3, "review roster changed: {review:?}");
    if cfg!(feature = "review") {
        assert_eq!(live.len(), default.len() + review.len());
        for name in &review {
            assert!(
                live.contains(name),
                "`--features review` build omits {name}"
            );
        }
    } else {
        assert_eq!(
            live, default,
            "default build serves something other than the `always` rows"
        );
        for name in &review {
            assert!(
                !live.contains(name),
                "default build unexpectedly serves {name}"
            );
        }
    }
}

/// Why: `review_tool_descriptors` used to live inside the `#[cfg(feature =
/// "review")]` module, and CI never builds this crate with that feature. If it
/// is gated again the documented review rows go unverified, so pin the fact
/// that a default build can still read them.
#[test]
fn review_descriptors_are_readable_without_the_feature() {
    assert_eq!(descriptors::review_tool_descriptors().len(), 3);
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

/// Why: `README.md` keeps a hand-written tool-to-method table outside the
/// markers, because the method a tool forwards to lives in the dispatcher's
/// match arms and is not derivable from the descriptors. Hand-written means it
/// can drift — which is the defect this whole mechanism exists to remove. It
/// cannot be generated, but it can be constrained: every name it lists must be
/// a real tool.
///
/// #6287 renamed the section from "HTTP equivalents" to "RPC equivalents"
/// along with the transport. The test name is unchanged so its history stays
/// findable.
/// What: scrapes the leading `` `tool` `` cell of each row in that table and
/// asserts the tool exists. Deliberately one-directional — a tool with no
/// distinct RPC method is free to be absent from the table.
/// Test: this test.
#[test]
fn http_equivalents_name_only_real_tools() {
    let readme = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("README.md");
    let table = readme
        .split("### RPC equivalents")
        .nth(1)
        .expect("README.md lost its `### RPC equivalents` section")
        .split("\n## ")
        .next()
        .expect("section body");

    let known: Vec<String> = default_rows()
        .into_iter()
        .chain(review_rows())
        .map(|r| r.name)
        .collect();
    let mut checked = 0usize;
    for line in table.lines().filter(|l| l.starts_with("| `")) {
        let name = line
            .trim_start_matches("| `")
            .split('`')
            .next()
            .expect("tool name cell");
        assert!(
            known.contains(&name.to_string()),
            "the RPC-equivalents table names `{name}`, which is not a registered tool"
        );
        checked += 1;
    }
    assert!(
        checked >= 15,
        "only matched {checked} rows — the table's shape changed"
    );
}
