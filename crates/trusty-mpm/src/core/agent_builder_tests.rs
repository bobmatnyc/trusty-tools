//! Tests for `agent_builder` — split out to keep `agent_builder.rs` under the
//! 500-line hard cap enforced by `scripts/check_line_cap.sh`.
//!
//! Why: the implementation file (agent_builder.rs) was approaching its frozen
//! budget; the #389 regression tests would have pushed it over, so the test
//! module was extracted here following the same pattern used elsewhere in the
//! crate (e.g. `delegation_authority_tests.rs`).
//! What: all unit and regression tests for [`compose_agent`] and
//! [`source_chain`], including colon-in-value round-trip checks.
//! Test: run with `cargo test -p trusty-mpm -- core::agent_builder`.

use super::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Write `<name>.md` into `dir` with the given raw content.
fn write_agent(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(format!("{name}.md")), content).expect("write agent");
}

#[test]
fn compose_base_only() {
    // An agent with no `extends` returns its own body under a merged
    // frontmatter block — no inheritance to resolve.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nFoundation content.\n",
    );
    let composed = compose_agent("base-agent", tmp.path()).unwrap();
    assert!(composed.starts_with("---\n"));
    assert!(composed.contains("name: base-agent"));
    assert!(composed.contains("role: base"));
    assert!(composed.contains("Foundation content."));
    // `extends` must never leak into the composed frontmatter.
    assert!(!composed.contains("extends:"));
}

#[test]
fn compose_engineer_chain() {
    // engineer -> base-engineer -> base-agent must concatenate bodies
    // base-first and merge frontmatter child-wins.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Engineer\n\nENGINEER BASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\n---\n\n# Engineer\n\nLEAF BODY\n",
    );
    let composed = compose_agent("engineer", tmp.path()).unwrap();

    // Child fields win in the merged frontmatter.
    assert!(composed.contains("name: engineer"));
    assert!(composed.contains("role: engineer"));
    assert!(composed.contains("model: sonnet"));

    // Bodies appear base-first.
    let base = composed.find("BASE BODY").expect("base body present");
    let mid = composed
        .find("ENGINEER BASE BODY")
        .expect("base-engineer body present");
    let leaf = composed.find("LEAF BODY").expect("leaf body present");
    assert!(base < mid, "base body must precede base-engineer body");
    assert!(mid < leaf, "base-engineer body must precede leaf body");
}

#[test]
fn cycle_detection() {
    // A extends B, B extends A -> the chain forms a cycle.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "agent-a",
        "---\nname: agent-a\nextends: agent-b\n---\n\nA body\n",
    );
    write_agent(
        tmp.path(),
        "agent-b",
        "---\nname: agent-b\nextends: agent-a\n---\n\nB body\n",
    );
    let err = compose_agent("agent-a", tmp.path()).unwrap_err();
    match err {
        AgentBuildError::Cycle(chain) => {
            assert!(chain.contains(&"agent-a".to_string()));
            assert!(chain.contains(&"agent-b".to_string()));
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn depth_exceeded() {
    // A chain longer than MAX_DEPTH must fail with DepthExceeded.
    let tmp = TempDir::new().unwrap();
    // Build level0 (root) .. level10 each extending the previous.
    write_agent(tmp.path(), "level0", "---\nname: level0\n---\n\nroot\n");
    for i in 1..=10 {
        write_agent(
            tmp.path(),
            &format!("level{i}"),
            &format!(
                "---\nname: level{i}\nextends: level{}\n---\n\nbody{i}\n",
                i - 1
            ),
        );
    }
    let err = compose_agent("level10", tmp.path()).unwrap_err();
    assert!(
        matches!(err, AgentBuildError::DepthExceeded(MAX_DEPTH)),
        "expected DepthExceeded, got {err:?}"
    );
}

#[test]
fn compose_missing_agent() {
    // A request for a non-existent source file must surface NotFound.
    let tmp = TempDir::new().unwrap();
    let err = compose_agent("ghost", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "ghost"));
}

#[test]
fn missing_parent_is_not_found() {
    // A child extending an absent parent must report the parent missing.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "child",
        "---\nname: child\nextends: nowhere\n---\n\nbody\n",
    );
    let err = compose_agent("child", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "nowhere"));
}

#[test]
fn unterminated_frontmatter_errors() {
    // A frontmatter block missing its closing `---` is a parse error.
    let tmp = TempDir::new().unwrap();
    write_agent(tmp.path(), "broken", "---\nname: broken\n\n# No close\n");
    let err = compose_agent("broken", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::FrontmatterParse(_)));
}

#[test]
fn source_chain_engineer() {
    // The resolved chain must list ancestors base-first.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\n---\n\nb\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nextends: base-agent\n---\n\nbe\n",
    );
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nextends: base-engineer\n---\n\ne\n",
    );
    let chain = source_chain("engineer", tmp.path()).unwrap();
    assert_eq!(chain, vec!["base-agent", "base-engineer", "engineer"]);
}

#[test]
fn source_chain_base_only() {
    // A base agent's chain is just itself.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\n---\n\nb\n",
    );
    let chain = source_chain("base-agent", tmp.path()).unwrap();
    assert_eq!(chain, vec!["base-agent"]);
}

// ── colon-in-value regression tests (issue #389) ─────────────────────────

#[test]
fn url_value_round_trips() {
    // A value that is a URL (`https://...`) contains multiple colons; the
    // parser must split on the FIRST colon only and preserve the full URL.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "web-agent",
        "---\nname: web-agent\nrole: web\nmodel: https://example.com/model-api\n---\n\n# Web\n",
    );
    // compose_agent must not error; the URL must survive round-trip.
    let composed = compose_agent("web-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("model: https://example.com/model-api"),
        "URL value must be preserved verbatim; got:\n{composed}"
    );
}

#[test]
fn timestamp_value_round_trips() {
    // ISO-8601 timestamps (`2026-06-05T14:31:34`) contain colons in the
    // time component; the parser must keep the entire timestamp.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "ts-agent",
        "---\nname: ts-agent\nrole: worker\ndescription: Created at 2026-06-05T14:31:34\n---\n\n# TS\n",
    );
    let composed = compose_agent("ts-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("2026-06-05T14:31:34"),
        "timestamp in description must survive; got:\n{composed}"
    );
}

#[test]
fn bedrock_model_id_round_trips() {
    // Model ids like `bedrock/us.anthropic.claude-sonnet-4-6` contain
    // slashes and dots; the full id must survive the round-trip without
    // truncation at any embedded separator.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "ai-agent",
        "---\nname: ai-agent\nrole: ai\nmodel: bedrock/us.anthropic.claude-sonnet-4-6\n---\n\n# AI\n",
    );
    let composed = compose_agent("ai-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("model: bedrock/us.anthropic.claude-sonnet-4-6"),
        "model id must be preserved; got:\n{composed}"
    );
}
