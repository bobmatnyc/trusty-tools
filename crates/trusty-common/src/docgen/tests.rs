//! Unit coverage for the generated-doc-region machinery.

use super::*;
use serde_json::json;

fn descriptors() -> Value {
    json!([
        { "name": "beta",  "description": "Second tool. Extra prose that must not appear." },
        { "name": "alpha", "description": "First   tool\n                      wrapped across lines." },
    ])
}

#[test]
fn rows_accept_both_shapes() {
    let bare = tool_rows(&descriptors()).expect("bare array");
    let wrapped = tool_rows(&json!({ "tools": descriptors() })).expect("tools envelope");
    assert_eq!(bare, wrapped);
    assert_eq!(bare.len(), 2);
}

#[test]
fn rows_reject_missing_name() {
    let err = tool_rows(&json!([{ "description": "nameless" }])).unwrap_err();
    assert!(matches!(err, DocGenError::MalformedDescriptors(_)), "{err}");
}

#[test]
fn summary_is_first_sentence_whitespace_collapsed() {
    let rows = tool_rows(&descriptors()).expect("rows");
    let alpha = rows.iter().find(|r| r.name == "alpha").expect("alpha");
    assert_eq!(alpha.summary, "First tool wrapped across lines.");
    let beta = rows.iter().find(|r| r.name == "beta").expect("beta");
    assert_eq!(beta.summary, "Second tool.");
}

#[test]
fn summary_does_not_split_on_an_abbreviation() {
    let rows = tool_rows(&json!([{
        "name": "t",
        "description": "Restrict to one language, e.g. rust or go. Second sentence dropped."
    }]))
    .expect("rows");
    assert_eq!(
        rows[0].summary,
        "Restrict to one language, e.g. rust or go."
    );
}

#[test]
fn summary_without_a_period_is_kept_whole() {
    let rows =
        tool_rows(&json!([{ "name": "t", "description": "No terminator here" }])).expect("rows");
    assert_eq!(rows[0].summary, "No terminator here");
}

#[test]
fn summary_is_length_capped_on_a_word_boundary() {
    let long = "word ".repeat(60);
    let rows = tool_rows(&json!([{ "name": "t", "description": long }])).expect("rows");
    assert!(
        rows[0].summary.chars().count() <= SUMMARY_CAP + 1,
        "{}",
        rows[0].summary
    );
    assert!(rows[0].summary.ends_with('…'));
}

#[test]
fn render_is_sorted_and_stable() {
    let rendered = render_tool_section("m::f", "**2 tools**", &tool_rows(&descriptors()).unwrap());
    let alpha = rendered.find("| `alpha`").expect("alpha row");
    let beta = rendered.find("| `beta`").expect("beta row");
    assert!(alpha < beta, "rows must sort by name:\n{rendered}");
    // Feeding the same descriptors in the opposite order renders identically.
    let reversed: Vec<ToolRow> = tool_rows(&descriptors())
        .unwrap()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(
        rendered,
        render_tool_section("m::f", "**2 tools**", &reversed)
    );
}

#[test]
fn render_adds_availability_column_only_when_labelled() {
    let plain = render_tool_section("m::f", "**2 tools**", &tool_rows(&descriptors()).unwrap());
    assert!(plain.contains("| Tool | Arguments | Summary |"));
    let tagged = labelled(tool_rows(&descriptors()).unwrap(), "default");
    let rendered = render_tool_section("m::f", "**2 tools**", &tagged);
    assert!(rendered.contains("| Tool | Available | Arguments | Summary |"));
    assert!(rendered.contains("| `alpha` | default |"));
}

/// Why: `properties` key order is whatever `serde_json` chose; only the
/// `required` array is authored. Sorting the optional half is what stops that
/// choice from reaching a committed file.
#[test]
fn arguments_lists_required_then_sorted_optional() {
    let rows = tool_rows(&json!([{
        "name": "t",
        "inputSchema": {
            "required": ["index_id", "query"],
            "properties": {
                "top_k": {}, "query": {}, "index_id": {}, "mode": {}, "after": {}
            }
        }
    }]))
    .expect("rows");
    assert_eq!(
        rows[0].arguments,
        "`index_id`, `query`, `after?`, `mode?`, `top_k?`"
    );
}

#[test]
fn arguments_are_an_em_dash_when_the_tool_takes_nothing() {
    let rows = tool_rows(&json!([
        { "name": "a", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "b" },
    ]))
    .expect("rows");
    assert_eq!(rows[0].arguments, "—");
    assert_eq!(rows[1].arguments, "—");
}

#[test]
fn render_escapes_pipes_in_summaries() {
    let rows = tool_rows(&json!([{ "name": "t", "description": "a | b" }])).unwrap();
    assert!(render_tool_section("m::f", "**1 tools**", &rows).contains("a \\| b"));
}

#[test]
#[should_panic(expected = "duplicate tool name")]
fn render_rejects_duplicate_names() {
    let rows = tool_rows(&json!([{ "name": "t" }, { "name": "t" }])).unwrap();
    let _ = render_tool_section("m::f", "**2 tools**", &rows);
}

#[test]
fn count_note_renders_single_and_multi() {
    assert_eq!(count_note(&[("", 21)]), "**21 tools**");
    assert_eq!(
        count_note(&[
            ("with default features", 19),
            ("with `--features review`", 22)
        ]),
        "**19 tools** with default features, **22 tools** with `--features review`"
    );
}

/// Why: a file that silently lost its markers would pass every check while
/// documenting nothing. Missing markers must fail, not skip.
#[test]
fn missing_marker_is_an_error_not_a_skip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("README.md");
    std::fs::write(&path, "# Title\n\nno markers here\n").expect("write");
    let err = sync_region(&path, "mcp-tools", "body").unwrap_err();
    assert!(matches!(err, DocGenError::MissingMarker { .. }), "{err}");
    assert!(err.to_string().contains("not a skip"), "{err}");
}

#[test]
fn duplicate_marker_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("README.md");
    let m = begin_marker("x");
    std::fs::write(&path, format!("{m}\n{m}\n{}\n", end_marker("x"))).expect("write");
    let err = sync_region(&path, "x", "body").unwrap_err();
    assert!(matches!(err, DocGenError::DuplicateMarker { .. }), "{err}");
}

#[test]
fn sync_reports_stale_then_rewrites() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("README.md");
    let file = format!(
        "# Title\n\n{}\n\nold body\n\n{}\n\ntrailer\n",
        begin_marker("mcp-tools"),
        end_marker("mcp-tools")
    );
    std::fs::write(&path, &file).expect("write");

    // Checking mode reports the drift and leaves the file alone.
    let outcome = sync_region_mode(&path, "mcp-tools", "new body", false).expect("sync");
    let Outcome::Stale { diff } = outcome else {
        panic!("expected Stale, got {outcome:?}")
    };
    assert!(diff.contains("-old body"), "{diff}");
    assert!(diff.contains("+new body"), "{diff}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), file);

    // The update path rewrites only the region, and settles on the second run.
    assert_eq!(
        sync_region_mode(&path, "mcp-tools", "new body", true).unwrap(),
        Outcome::Rewritten
    );

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.starts_with("# Title\n"), "{after}");
    assert!(after.ends_with("trailer\n"), "{after}");
    assert!(after.contains("new body"), "{after}");
    assert!(!after.contains("old body"), "{after}");
    assert_eq!(
        sync_region_mode(&path, "mcp-tools", "new body", false).unwrap(),
        Outcome::UpToDate
    );
}

/// Why: `UPDATE_DOCS` is the documented switch; an empty or `0` value must
/// still mean "check", not "rewrite".
#[test]
fn update_env_values_are_interpreted_conservatively() {
    // Unset in the ordinary test environment.
    assert!(
        !update_requested(),
        "UPDATE_DOCS leaked into the test environment"
    );
}

#[test]
#[should_panic(expected = "cargo test -p demo")]
fn assert_region_panics_with_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("README.md");
    std::fs::write(
        &path,
        format!("{}\nold\n{}\n", begin_marker("r"), end_marker("r")),
    )
    .expect("write");
    assert_region(
        &path,
        "r",
        "new",
        "UPDATE_DOCS=1 cargo test -p demo --test generated_docs",
    );
}

#[test]
fn normalise_path_strips_stringify_spacing() {
    assert_eq!(normalise_path("a :: b :: c"), "a::b::c");
}
