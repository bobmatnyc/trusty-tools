//! Content + ordering tests for the compiled-in harness-protocol layers.
//!
//! Why: The harness layer content is contractual — agents rely on the
//! `out_dir` + `## Summary` + `write_file` + `finish_task` protocol rules.
//! These tests fail loudly if someone empties or reshapes the constants in
//! `harness_protocol.rs`, catching silent regressions in agent behavior, and
//! pin that the harness layer is injected before the agent's base prompt.
//! What: Asserts the `BASE_PROTOCOL` / `CLAUDE_CODE_PROTOCOL` /
//! `FINISH_TASK_PROTOCOL` constants carry their required markers, and that
//! `SystemPromptBuilder` orders the harness layer ahead of the base content.
//! Test: This module IS the test surface.

use super::SystemPromptBuilder;

#[test]
fn harness_base_protocol_contains_output_directory_rule() {
    let p = crate::agents::harness_protocol::BASE_PROTOCOL;
    assert!(!p.trim().is_empty(), "BASE_PROTOCOL must not be empty");
    assert!(
        p.contains("out_dir") || p.contains("Output Directory"),
        "BASE_PROTOCOL must contain output-directory instructions"
    );
    assert!(
        p.contains("Summary"),
        "BASE_PROTOCOL must contain summary requirement"
    );
}

#[test]
fn harness_claude_code_protocol_contains_write_file() {
    assert!(
        crate::agents::harness_protocol::CLAUDE_CODE_PROTOCOL.contains("write_file"),
        "CLAUDE_CODE_PROTOCOL must contain write_file instructions"
    );
}

#[test]
fn harness_finish_task_protocol_contains_finish_task() {
    assert!(
        crate::agents::harness_protocol::FINISH_TASK_PROTOCOL.contains("finish_task"),
        "FINISH_TASK_PROTOCOL must contain finish_task instructions"
    );
}

#[test]
fn prompt_builder_harness_layer_precedes_agent_base() {
    let builder =
        SystemPromptBuilder::new("AGENT_ROLE_CONTENT").add_harness_layer("HARNESS_CONTENT");
    let built = builder.build();
    let harness_pos = built.find("HARNESS_CONTENT").expect("harness missing");
    let agent_pos = built.find("AGENT_ROLE_CONTENT").expect("agent missing");
    assert!(
        harness_pos < agent_pos,
        "Harness layer must appear before agent base content"
    );
}
