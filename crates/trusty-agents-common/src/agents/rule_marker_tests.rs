//! Tests for the rule-id marker grammar and the composer strip (#10058).
use super::*;
use crate::agents::builder::compose_agent;

#[test]
fn marker_id_accepts_the_grammar_and_rejects_the_rest() {
    for (line, want) in [
        ("<!-- rule:r-049 -->", Some("r-049")),
        ("<!-- rule:r-078b -->\n", Some("r-078b")),
        (
            "  <!-- rule:tm-workflow/pre-pull-collision-pointer -->  ",
            Some("tm-workflow/pre-pull-collision-pointer"),
        ),
        ("<!-- rule: spaced -->", None),
        ("<!-- rule:Upper -->", None),
        ("<!-- rule:snake_case -->", None),
        ("<!-- rule:trailing- -->", None),
        ("<!-- rule:double//slash -->", None),
        ("<!-- rule: -->", None),
        ("<!-- rule:ok --> trailing prose", None),
        ("prose <!-- rule:ok -->", None),
        ("<!-- #8107: an authoring comment -->", None),
    ] {
        assert_eq!(marker_id(line), want, "{line:?}");
    }
}

#[test]
fn rule_ids_skips_fenced_examples_and_malformed_lines() {
    let text = "<!-- rule:a/one -->\nText.\n\n```md\n<!-- rule:in-fence -->\n<!-- rule:Bad -->\n```\n\
                <!-- rule:Bad_Id -->\n~~~\n<!-- rule:tilde -->\n~~~\n<!-- rule:b -->\nMore.\n";
    assert_eq!(rule_ids(text), ["a/one", "b"]);
    assert_eq!(malformed_markers(text), ["<!-- rule:Bad_Id -->"]);
}

#[test]
fn strip_removes_marker_lines_and_keeps_every_other_byte() {
    let original =
        "## Heading\n\nFirst rule.\n\n- bullet one\n- bullet two\n\n```\n<!-- rule:kept -->\n```\n";
    let marked = "## Heading\n\n<!-- rule:h/first -->\nFirst rule.\n\n- bullet one\n<!-- rule:h/two -->\n- bullet two\n\n```\n<!-- rule:kept -->\n```\n";
    assert_eq!(strip_rule_markers(marked), original);
    assert!(
        matches!(strip_rule_markers(original), Cow::Owned(_)),
        "a fenced marker still costs one copy"
    );
    let plain = "No marker here.\n<!-- #8107: authoring note -->\n";
    assert!(matches!(strip_rule_markers(plain), Cow::Borrowed(_)));
    assert_eq!(strip_rule_markers(plain), plain);
}

/// Why: #10058 — a marker in any chain link must not reach the deployed body.
/// What: a two-link chain with markers in both composes to the same text as
/// the unmarked chain.
/// Test: this test.
#[test]
fn compose_agent_drops_rule_markers_from_every_chain_link() {
    let write = |dir: &std::path::Path, base: &str, leaf: &str| {
        std::fs::write(dir.join("base-agent.md"), base).expect("base");
        std::fs::write(dir.join("leaf.md"), leaf).expect("leaf");
    };
    let plain = tempfile::TempDir::new().expect("tempdir");
    write(
        plain.path(),
        "---\nname: base-agent\n---\n\n# Base\n\nBase rule.\n",
        "---\nname: leaf\nextends: base-agent\n---\n\nLeaf rule.\n",
    );
    let marked = tempfile::TempDir::new().expect("tempdir");
    write(
        marked.path(),
        "---\nname: base-agent\n---\n\n# Base\n\n<!-- rule:base-agent/base-rule -->\nBase rule.\n",
        "---\nname: leaf\nextends: base-agent\n---\n\n<!-- rule:leaf/leaf-rule -->\nLeaf rule.\n",
    );
    let composed = compose_agent("leaf", marked.path()).expect("compose marked");
    assert!(!composed.contains(RULE_MARKER_OPEN), "{composed}");
    assert_eq!(
        composed,
        compose_agent("leaf", plain.path()).expect("compose plain")
    );
}
