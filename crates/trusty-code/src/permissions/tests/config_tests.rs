//! Parser and fail-closed-validation tests for `crate::permissions::config`
//! (#7948).
//!
//! Why: a malformed `permissions:` block must fail the agent load, never
//! degrade to "no permissions". Every rejection case below fails if its error
//! arm ever becomes a default.
//! What: decision words, literal-prefix computation, both value shapes, block
//! extraction from a frontmatter that is not valid YAML, and five rejections.
//! Test: this module.

use crate::permissions::config::{
    PermissionConfigError, RuleBody, RuleDecision, parse_permissions,
};
use crate::permissions::parse_permissions as parse_public;

/// Wrap a `permissions:` block in a minimal agent document.
fn doc(block: &str) -> String {
    format!("---\nname: t\nrole: engineer\n{block}---\n\nBody.\n")
}

/// The three decision words parse, and nothing else does.
#[test]
fn decision_parses_the_three_words() {
    assert_eq!(RuleDecision::parse("allow"), Some(RuleDecision::Allow));
    assert_eq!(RuleDecision::parse("ask"), Some(RuleDecision::Ask));
    assert_eq!(RuleDecision::parse("deny"), Some(RuleDecision::Deny));
    assert_eq!(RuleDecision::Allow.as_str(), "allow");
    assert_eq!(RuleDecision::Ask.as_str(), "ask");
    assert_eq!(RuleDecision::Deny.as_str(), "deny");
    assert_eq!(RuleDecision::parse("ALLOW"), None, "no case coercion");
    assert_eq!(RuleDecision::parse("allowed"), None);
}

/// The tie-break order is deny > ask > allow, by rank.
#[test]
fn decision_rank_orders_deny_over_ask_over_allow() {
    assert!(RuleDecision::Deny.rank() > RuleDecision::Ask.rank());
    assert!(RuleDecision::Ask.rank() > RuleDecision::Allow.rank());
}

/// Literal-prefix length stops at the first glob metacharacter.
#[test]
fn literal_prefix_stops_at_the_first_metacharacter() {
    let map = parse_permissions(
        "t",
        &doc("permissions:\n  bash:\n    \"git status*\": allow\n    \"git *\": ask\n    \"*\": deny\n"),
    )
    .expect("parses")
    .expect("present");
    let RuleBody::Args(args) = &map.rules[0].body else {
        panic!("expected an argument table");
    };
    let lens: Vec<usize> = args.iter().map(|a| a.glob.literal_prefix_len()).collect();
    assert_eq!(lens, vec!["git status".len(), "git ".len(), 0]);
}

/// Both value shapes parse: a bare decision string and an argument table.
#[test]
fn tool_level_string_and_arg_level_table_both_parse() {
    let map = parse_permissions(
        "t",
        &doc("permissions:\n  read_file: allow\n  bash:\n    \"rm *\": deny\n"),
    )
    .expect("parses")
    .expect("present");
    assert_eq!(map.len(), 2);
    assert!(matches!(
        map.rules[0].body,
        RuleBody::Tool(RuleDecision::Allow)
    ));
    let RuleBody::Args(args) = &map.rules[1].body else {
        panic!("expected an argument table");
    };
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].decision, RuleDecision::Deny);
    assert_eq!(args[0].glob.pattern(), "rm *");
}

/// A bare `permissions:` line is an EMPTY map, not an absent one.
#[test]
fn empty_mapping_parses_to_an_empty_map() {
    let map = parse_permissions("t", &doc("permissions:\n"))
        .expect("parses")
        .expect("the key was written, so the map is present");
    assert!(map.is_empty());
}

/// An agent with no `permissions:` key at all parses to `None`.
#[test]
fn absent_key_parses_to_none() {
    assert!(parse_permissions("t", &doc("")).expect("parses").is_none());
}

/// The opencode-style example (flow table, quoted glob key, catch-all) parses.
#[test]
fn example_block_from_the_spec_parses() {
    let block = "permissions:\n  \
                 read_file: allow\n  \
                 bash: { \"git status*\": allow, \"git diff*\": allow, \"rm *\": deny, \"*\": ask }\n  \
                 \"mcp__*\": ask\n";
    let map = parse_public("t", &doc(block))
        .expect("the documented example must parse")
        .expect("present");
    assert_eq!(map.len(), 3);
}

/// FAIL-CLOSED: an unknown decision word rejects the whole block, naming the
/// file, the key, and the value.
#[test]
fn unknown_decision_word_is_rejected() {
    let err = parse_permissions("agents/t.md", &doc("permissions:\n  bash: allowed\n"))
        .expect_err("a typo'd decision must fail the load, never degrade");
    assert!(
        matches!(err, PermissionConfigError::UnknownDecision { .. }),
        "got {err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("agents/t.md"), "must name the file: {text}");
    assert!(text.contains("bash"), "must name the key: {text}");
    assert!(text.contains("allowed"), "must quote the value: {text}");
}

/// FAIL-CLOSED: an uncompilable glob rejects the whole block.
#[test]
fn bad_glob_is_rejected() {
    let err = parse_permissions("agents/t.md", &doc("permissions:\n  \"a[\": allow\n"))
        .expect_err("an unparseable glob must fail the load");
    assert!(
        matches!(err, PermissionConfigError::BadGlob { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("agents/t.md"));
}

/// FAIL-CLOSED: a value that is neither a decision string nor a table rejects.
#[test]
fn non_string_non_table_value_is_rejected() {
    let err = parse_permissions("agents/t.md", &doc("permissions:\n  bash:\n    - deny\n"))
        .expect_err("a list value must fail the load");
    match err {
        PermissionConfigError::BadValue { found, .. } => assert_eq!(found, "a list"),
        other => panic!("got {other:?}"),
    }
}

/// FAIL-CLOSED: a `permissions:` scalar (not a mapping) rejects.
#[test]
fn scalar_permissions_value_is_rejected() {
    let err = parse_permissions("agents/t.md", &doc("permissions: allow\n"))
        .expect_err("a scalar block must fail the load");
    match err {
        PermissionConfigError::NotAMapping { found, .. } => assert_eq!(found, "a string"),
        other => panic!("got {other:?}"),
    }
}

/// FAIL-CLOSED: YAML that does not parse rejects.
#[test]
fn malformed_yaml_is_rejected() {
    let err = parse_permissions(
        "agents/t.md",
        &doc("permissions:\n  bash: { \"rm *\": deny\n"),
    )
    .expect_err("unbalanced YAML must fail the load");
    assert!(
        matches!(err, PermissionConfigError::Yaml { .. }),
        "got {err:?}"
    );
}

/// Other frontmatter keys that are not valid YAML must not stop the block
/// from parsing.
#[test]
fn frontmatter_that_is_not_yaml_still_parses_permissions() {
    let document = "---\nname: t\ndescription: Does X: then Y\npermissions:\n  read_file: allow\n---\n\nBody.\n";
    let map = parse_permissions("t", document)
        .expect("the surrounding frontmatter must not be parsed")
        .expect("present");
    assert_eq!(map.len(), 1);
}

/// Block extraction stops at the next top-level frontmatter key.
#[test]
fn block_stops_at_the_next_frontmatter_key() {
    let document =
        "---\npermissions:\n  read_file: allow\ntcode_tools: [read_file, bash]\n---\n\nBody.\n";
    let map = parse_permissions("t", document)
        .expect("parses")
        .expect("present");
    assert_eq!(map.len(), 1, "`tcode_tools:` is a sibling key, not a rule");
    assert_eq!(map.rules[0].glob.pattern(), "read_file");
}
