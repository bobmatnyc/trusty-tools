//! Comment survival across the two `config.toml` channel edits (#7609 slice 7).
//!
//! Why: every one of these fails against `origin/main`, where
//! `DocumentMut::remove` took the comment block above the removed table with
//! it and a replaced `[[channels]]` array lost both its heading and its place
//! in the file. That loss was found live — a `PUT /api/channels` deleted an
//! operator's seven-line `tickets-mcp`/ADR-0014 note.
//! What: byte-exact assertions on the rendered document, because "the comment
//! survived" means the operator's own text, not a paraphrase of it.
//! Test: this module IS the test.

use super::{remove_preserving_comments, replace_array_of_tables};
use toml_edit::DocumentMut;

/// A document shaped like the operator's real file: an unrelated comment block
/// between the legacy table and the rest of the config, and a heading above
/// `[[channels]]`.
const FIXTURE: &str = "\
# trusty-agents global configuration

[mcp]
inject_for_roles = [\"ctrl\"]

# tickets-mcp is intentionally NOT a `driver = \"direct\"` (OpenRPC) endpoint:
# the `tickets-mcp` binary speaks MCP framing, not OpenRPC `rpc.discover`.
# The former dead stubs were retired per ADR-0014 (native Rust MCP, PR #2624).

[[listeners]]
name = \"gmail-personal\"
connector = \"gmail\"

# the harness-wide channels
[[channels]]
id = \"gmail-personal\"
name = \"gmail-personal\"

[log_drain]
enabled = false
";

/// The unrelated comment block above `[[listeners]]` survives its removal.
///
/// Pre-fix this fails: `remove("listeners")` deleted the block with the table,
/// exactly as the live `PUT /api/channels` did.
#[test]
fn removing_a_table_keeps_the_comment_above_it() {
    let mut document: DocumentMut = FIXTURE.parse().expect("fixture parses");
    assert!(remove_preserving_comments(&mut document, "listeners"));
    let rendered = document.to_string();
    assert!(
        !rendered.contains("[[listeners]]"),
        "the legacy table is gone:\n{rendered}"
    );
    for line in [
        "# tickets-mcp is intentionally NOT a `driver = \"direct\"` (OpenRPC) endpoint:",
        "# the `tickets-mcp` binary speaks MCP framing, not OpenRPC `rpc.discover`.",
        "# The former dead stubs were retired per ADR-0014 (native Rust MCP, PR #2624).",
    ] {
        assert!(rendered.contains(line), "lost `{line}` from:\n{rendered}");
    }
    assert!(
        rendered.contains("# the harness-wide channels"),
        "the channels heading survived too:\n{rendered}"
    );
    // The salvaged block lands above the table that followed it, not at random.
    let salvaged = rendered.find("# tickets-mcp").expect("salvaged block");
    let channels = rendered.find("[[channels]]").expect("channels header");
    assert!(salvaged < channels, "order preserved:\n{rendered}");
    // Still a document, not a string with comments glued into it.
    rendered
        .parse::<DocumentMut>()
        .expect("the edited document re-parses");
}

/// A removed table that was LAST hands its comment block to the trailer.
#[test]
fn a_trailing_table_hands_its_comment_to_the_trailer() {
    let source = "[mcp]\ninject_for_roles = []\n\n# keep this note\n[[listeners]]\nname = \"x\"\n";
    let mut document: DocumentMut = source.parse().expect("parses");
    assert!(remove_preserving_comments(&mut document, "listeners"));
    let rendered = document.to_string();
    assert!(!rendered.contains("[[listeners]]"), "{rendered}");
    assert!(rendered.contains("# keep this note"), "{rendered}");
}

/// Removing a key that is not there changes nothing and says so.
#[test]
fn removing_an_absent_key_is_a_no_op() {
    let mut document: DocumentMut = FIXTURE.parse().expect("fixture parses");
    assert!(!remove_preserving_comments(&mut document, "listeners_typo"));
    assert_eq!(document.to_string(), FIXTURE);
}

/// A replaced `[[channels]]` keeps its heading comment and its position.
///
/// Pre-fix this fails twice: the heading was dropped, and the position-less
/// replacement rendered ahead of `[mcp]`'s own sub-tables.
#[test]
fn a_replaced_array_keeps_its_comment_and_place() {
    let mut document: DocumentMut = FIXTURE.parse().expect("fixture parses");
    let rendered_array: DocumentMut = "[[channels]]\nid = \"gmail-personal\"\nname = \"renamed\"\n"
        .parse()
        .expect("replacement parses");
    replace_array_of_tables(
        &mut document,
        "channels",
        rendered_array.get("channels").cloned().expect("array"),
    );
    let out = document.to_string();
    assert!(out.contains("# the harness-wide channels"), "{out}");
    assert!(out.contains("name = \"renamed\""), "{out}");
    let listeners = out.find("[[listeners]]").expect("listeners still there");
    let channels = out.find("[[channels]]").expect("channels header");
    let log_drain = out.find("[log_drain]").expect("log_drain header");
    assert!(
        listeners < channels && channels < log_drain,
        "the array stayed where it was:\n{out}"
    );
}

/// A list that gained an entry renders as ONE block, never split across the
/// tables it used to sit among.
#[test]
fn a_grown_array_is_rendered_as_one_block_at_the_end() {
    let mut document: DocumentMut = FIXTURE.parse().expect("fixture parses");
    let rendered_array: DocumentMut =
        "[[channels]]\nid = \"one\"\n\n[[channels]]\nid = \"two\"\n\n[[channels]]\nid = \"three\"\n"
            .parse()
            .expect("replacement parses");
    replace_array_of_tables(
        &mut document,
        "channels",
        rendered_array.get("channels").cloned().expect("array"),
    );
    let out = document.to_string();
    let reparsed: DocumentMut = out.parse().expect("re-parses");
    assert_eq!(
        reparsed
            .get("channels")
            .and_then(|item| item.as_array_of_tables())
            .map(|array| array.len()),
        Some(3),
        "{out}"
    );
    let log_drain = out.find("[log_drain]").expect("log_drain header");
    let first = out.find("[[channels]]").expect("first channels header");
    assert!(
        log_drain < first,
        "the whole block moved to the end:\n{out}"
    );
    assert!(
        out[first..].find("[log_drain]").is_none(),
        "nothing splits the block:\n{out}"
    );
}
