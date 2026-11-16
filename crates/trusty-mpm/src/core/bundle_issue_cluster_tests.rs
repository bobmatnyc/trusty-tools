//! The issue-cluster rule reaches the PM prompt and the version-control agent.
//!
//! Why: `tm-workflow` already said "bugfix batching is the default", yet PMs
//! kept dispatching one PR per issue, because neither the resident PM prompt
//! nor `version-control.md` stated it (owner ruling 2026-11-16).
//! What: asserts the composed WORKFLOW section, `version-control.md`, and the
//! `tm-workflow` skill each state one branch/PR per cluster, one commit per
//! issue, and `Refs #N`.
//! Test: this file; run with `cargo test -p trusty-mpm --lib core::bundle::tests::issue_cluster`.
use super::{agent, plain, skill_text};
use crate::core::content_source::test_support::rc;
use crate::core::instruction_pipeline::workflow_section;

/// Panics naming `surface` when `text` lacks any of `phrases`.
fn assert_states(surface: &str, text: &str, phrases: &[&str]) {
    let text = plain(text);
    for phrase in phrases {
        assert!(
            text.contains(phrase),
            "{surface} must state the issue-cluster rule: missing `{phrase}`"
        );
    }
}

#[test]
fn pm_workflow_section_states_the_issue_cluster_rule() {
    assert_states(
        "the PM WORKFLOW section",
        &workflow_section(rc()),
        &[
            "## Issue Clusters",
            "one engineer, branch and PR per cluster",
            "one commit per issue",
            "one critic round and one gate run per PR",
        ],
    );
}

#[test]
fn version_control_states_one_branch_per_cluster() {
    assert_states(
        "version-control.md",
        agent("version-control.md"),
        &[
            "One branch and one PR per cluster",
            "one commit per issue",
            "every issue in the body as `Refs #N`, never `Closes`",
            "squash commit message lists every issue",
        ],
    );
}

#[test]
fn tm_workflow_skill_carries_the_cluster_detail() {
    assert_states(
        "tm-workflow.md",
        skill_text("skills/tm-workflow.md"),
        &[
            "issue-cluster rule",
            "Each issue gets its own commit",
            "moves every issue in the cluster",
            "Keep agent slots full up to the band",
            "Stack PRs only for a true dependency chain",
        ],
    );
}
