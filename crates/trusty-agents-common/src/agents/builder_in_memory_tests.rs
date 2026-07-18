//! Tests for `builder_in_memory` — split out to keep the implementation file
//! focused, mirroring the `builder`/`builder_tests` split.
//!
//! Why: covers the in-memory `extends:` composer added for issue #2958
//! Slice E1 — single-level extends, a multi-level chain through BASE
//! templates, the missing-source and cycle error paths, and byte-for-byte
//! equivalence against the fs `compose_agent` path for identical content
//! (the property the whole slice exists to guarantee: swapping the fs
//! backend for an embedded asset map must not change composed output).
//! What: exercises [`compose_agent_in_memory`] and [`InMemorySources`]
//! directly.
//! Test: run with `cargo test -p trusty-agents-common -- agents::builder_in_memory`.

use super::*;
use crate::agents::builder::{AgentBuildError, compose_agent};
use std::fs;
use tempfile::TempDir;

#[test]
fn compose_in_memory_single_level() {
    // An agent with no `extends` returns its own body under a merged
    // frontmatter block, identically to the fs path's `compose_base_only`.
    let sources = build_in_memory_source_map([(
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nFoundation content.\n",
    )]);
    let composed = compose_agent_in_memory("base-agent", &sources).unwrap();
    assert!(composed.starts_with("---\n"));
    assert!(composed.contains("name: base-agent"));
    assert!(composed.contains("role: base"));
    assert!(composed.contains("Foundation content."));
    assert!(!composed.contains("extends:"));
}

#[test]
fn compose_in_memory_multi_level_base_chain() {
    // engineer -> BASE-ENGINEER -> BASE-AGENT: a three-deep chain resolved
    // through uppercase-stemmed BASE templates (the embedded-asset naming
    // convention issue #2958 Slice E2 will use), exercising both the walk
    // and the case-folded lookup in one test.
    let sources = build_in_memory_source_map([
        (
            "BASE-AGENT",
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE BODY\n",
        ),
        (
            "BASE-ENGINEER",
            "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Engineer\n\nENGINEER BASE BODY\n",
        ),
        (
            "rust-engineer",
            "---\nname: rust-engineer\nrole: engineer\nextends: BASE-ENGINEER\nmodel: sonnet\n---\n\n# Rust Engineer\n\nLEAF BODY\n",
        ),
    ]);
    let composed = compose_agent_in_memory("rust-engineer", &sources).unwrap();

    assert!(composed.contains("name: rust-engineer"));
    assert!(composed.contains("role: engineer"));
    assert!(composed.contains("model: sonnet"));

    let base = composed.find("BASE BODY").expect("base body present");
    let mid = composed
        .find("ENGINEER BASE BODY")
        .expect("base-engineer body present");
    let leaf = composed.find("LEAF BODY").expect("leaf body present");
    assert!(base < mid, "base body must precede base-engineer body");
    assert!(mid < leaf, "base-engineer body must precede leaf body");
}

#[test]
fn case_insensitive_in_memory_resolve() {
    // `extends: base-qa` (lowercase, the convention concrete agents use)
    // must resolve against a source registered under the uppercase stem
    // `BASE-QA` (the convention BASE template assets use), matching the fs
    // path's case-insensitive `SourceMap` behavior.
    let sources = build_in_memory_source_map([
        (
            "BASE-QA",
            "---\nname: base-qa\nrole: base-qa\n---\n\nQA BASE BODY\n",
        ),
        (
            "qa",
            "---\nname: qa\nrole: qa\nextends: base-qa\n---\n\nQA LEAF BODY\n",
        ),
    ]);
    let composed = compose_agent_in_memory("qa", &sources).unwrap();
    assert!(composed.contains("QA BASE BODY"));
    assert!(composed.contains("QA LEAF BODY"));
}

#[test]
fn compose_in_memory_missing_source_errors() {
    // A request for a name absent from the map must surface NotFound,
    // mirroring the fs path's `compose_missing_agent`.
    let sources = InMemorySources::new();
    let err = compose_agent_in_memory("ghost", &sources).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "ghost"));
}

#[test]
fn in_memory_missing_extends_target_errors() {
    // A child whose `extends:` parent is absent from the map must report
    // the PARENT missing, mirroring the fs path's `missing_parent_is_not_found`.
    let sources = build_in_memory_source_map([(
        "child",
        "---\nname: child\nextends: nowhere\n---\n\nbody\n",
    )]);
    let err = compose_agent_in_memory("child", &sources).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "nowhere"));
}

#[test]
fn in_memory_cycle_detection() {
    // agent-a extends agent-b, agent-b extends agent-a -> Cycle, mirroring
    // the fs path's `cycle_detection`.
    let sources = build_in_memory_source_map([
        (
            "agent-a",
            "---\nname: agent-a\nextends: agent-b\n---\n\nA body\n",
        ),
        (
            "agent-b",
            "---\nname: agent-b\nextends: agent-a\n---\n\nB body\n",
        ),
    ]);
    let err = compose_agent_in_memory("agent-a", &sources).unwrap_err();
    match err {
        AgentBuildError::Cycle(chain) => {
            assert!(chain.contains(&"agent-a".to_string()));
            assert!(chain.contains(&"agent-b".to_string()));
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn fs_and_in_memory_compose_are_byte_equivalent() {
    // The property Slice E1 exists to guarantee: composing identical
    // content through the fs backend (`compose_agent`) and the in-memory
    // backend (`compose_agent_in_memory`) must produce byte-identical
    // output, so trusty-code's embedded-asset roster (issue #2958 Slice E2)
    // behaves exactly like trusty-mpm's on-disk roster.
    let base_agent = "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE BODY\n";
    let base_engineer = "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Engineer\n\nENGINEER BASE BODY\n";
    let engineer = "---\nname: engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\nskills: [toolchains-rust-core]\n---\n\n# Engineer\n\nLEAF BODY\n";

    // fs backend
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("base-agent.md"), base_agent).unwrap();
    fs::write(tmp.path().join("base-engineer.md"), base_engineer).unwrap();
    fs::write(tmp.path().join("engineer.md"), engineer).unwrap();
    let fs_composed = compose_agent("engineer", tmp.path()).unwrap();

    // in-memory backend, identical content
    let sources = build_in_memory_source_map([
        ("base-agent", base_agent),
        ("base-engineer", base_engineer),
        ("engineer", engineer),
    ]);
    let in_memory_composed = compose_agent_in_memory("engineer", &sources).unwrap();

    assert_eq!(
        fs_composed, in_memory_composed,
        "fs and in-memory composition must be byte-identical for identical source content"
    );
}
