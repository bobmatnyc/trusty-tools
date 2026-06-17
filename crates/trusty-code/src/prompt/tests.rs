//! Unit tests for the prompt-assembly layer (parity-spec §2, §4, §6).
//!
//! Why: The parity guarantee is only as strong as the assembler's determinism
//! and section discipline; these tests pin the stable order, idempotence,
//! separator collapse, and fallback-last placement the spec mandates.
//! What: Exercises [`assemble_system_prompt`] and [`PromptAssembler`] over the
//! full matrix of present/absent sections, plus shape checks on
//! [`BASE_PREAMBLE`] and [`BASE_PREAMBLE_VERSION`].
//! Test: this file.

use crate::agents::AgentConfig;
use crate::prompt::{
    BASE_PREAMBLE, BASE_PREAMBLE_VERSION, PromptAssembler, assemble_system_prompt,
};

/// Build an `AgentConfig` whose `system_prompt.content` is the given string.
///
/// Why: Tests need a minimal config that varies only by prompt content.
/// What: Returns a defaulted `AgentConfig` with `system_prompt.content` set.
/// Test: Used by the tests below (no assertions of its own).
fn config_with_prompt(content: &str) -> AgentConfig {
    let mut cfg = AgentConfig::default();
    cfg.system_prompt.content = content.to_string();
    cfg
}

/// With all four sections present, the assembled order is 1→2→3→4 (spec §2).
///
/// Why: A stable, fixed section order is the backbone of the parity guarantee.
/// What: Asserts the `str::find` positions of BASE, agent prompt, project
/// context, and fallback guidance are strictly increasing.
/// Test: this test.
#[test]
fn assembled_order_is_stable() {
    let cfg = config_with_prompt("AGENT_PROMPT_MARKER");
    let out = assemble_system_prompt(
        &cfg,
        Some("PROJECT_CONTEXT_MARKER"),
        Some("FALLBACK_GUIDANCE_MARKER"),
    );

    let base = out.find(BASE_PREAMBLE).expect("base present");
    let agent = out.find("AGENT_PROMPT_MARKER").expect("agent present");
    let project = out.find("PROJECT_CONTEXT_MARKER").expect("project present");
    let fallback = out
        .find("FALLBACK_GUIDANCE_MARKER")
        .expect("fallback present");

    assert!(base < agent, "BASE must precede agent prompt");
    assert!(agent < project, "agent prompt must precede project context");
    assert!(
        project < fallback,
        "project context must precede fallback guidance"
    );
}

/// Identical inputs produce byte-identical output across calls (spec §1).
///
/// Why: Parity requires determinism; assembly must never reorder or vary.
/// What: Calls the assembler twice with the same inputs and asserts equality.
/// Test: this test.
#[test]
fn base_identical_across_calls() {
    let cfg = config_with_prompt("You are an engineer.");
    let first = assemble_system_prompt(&cfg, Some("# Project\nrules"), None);
    let second = assemble_system_prompt(&cfg, Some("# Project\nrules"), None);
    assert_eq!(first, second);
}

/// Omitted optional sections never produce a doubled separator (spec §2c).
///
/// Why: A collapsed separator (`---\n\n\n\n---`) would be a malformed prompt and
/// signal a section-skipping bug.
/// What: Assembles with no project context and no fallback, then asserts the
/// output contains neither a doubled separator nor a trailing separator.
/// Test: this test.
#[test]
fn omitted_sections_produce_no_double_separator() {
    let cfg = config_with_prompt("Solo agent prompt.");
    let out = assemble_system_prompt(&cfg, None, None);

    assert!(
        !out.contains("\n\n---\n\n\n\n---\n\n"),
        "must not contain a doubled separator: {out:?}"
    );
    assert!(
        !out.ends_with("\n\n---\n\n"),
        "must not end with a dangling separator: {out:?}"
    );
    // Exactly one separator joins BASE and the single agent section.
    assert_eq!(out.matches("\n\n---\n\n").count(), 1);
}

/// An empty agent prompt is skipped, leaving BASE first with no extra rule.
///
/// Why: A blank `system_prompt.content` must not inject an empty section or a
/// dangling separator.
/// What: With an empty agent prompt and no other sections, the output equals
/// `BASE_PREAMBLE` exactly.
/// Test: this test.
#[test]
fn empty_agent_prompt_skipped() {
    let cfg = config_with_prompt("");
    let out = assemble_system_prompt(&cfg, None, None);

    assert!(out.starts_with(BASE_PREAMBLE), "must start with BASE");
    assert_eq!(out, BASE_PREAMBLE, "BASE only — no separators added");
    assert!(
        !out.contains("\n\n---\n\n"),
        "no separator with one section"
    );
}

/// Whitespace-only optional sections are treated as empty.
///
/// Why: A `CLAUDE.md` that is only whitespace (or a fallback string that is)
/// must not inject a meaningless section or a dangling separator.
/// What: Passes whitespace for both optional sections; output equals BASE only.
/// Test: this test.
#[test]
fn whitespace_only_sections_are_skipped() {
    let cfg = config_with_prompt("   \n  ");
    let out = assemble_system_prompt(&cfg, Some("\n\t  \n"), Some("   "));
    assert_eq!(out, BASE_PREAMBLE);
}

/// When supplied, fallback guidance is the final section (spec §4, D4).
///
/// Why: Placing fallback last makes a strong model's prompt a strict prefix of
/// a weak model's, yielding clean diffs and easy disclosure.
/// What: With all sections present, asserts the output ends with the fallback
/// text and that fallback follows the project context.
/// Test: this test.
#[test]
fn fallback_guidance_is_last() {
    let cfg = config_with_prompt("Agent prompt.");
    let out = assemble_system_prompt(&cfg, Some("Project context."), Some("FALLBACK_LAST"));

    assert!(
        out.ends_with("FALLBACK_LAST"),
        "fallback must be last: {out:?}"
    );
    let project = out.find("Project context.").expect("project present");
    let fallback = out.find("FALLBACK_LAST").expect("fallback present");
    assert!(project < fallback);
}

/// Native-tier models (no fallback) get exactly sections 1→2→3 (spec D4).
///
/// Why: Confirms the prefix-compatibility design: the common case omits §4.
/// What: With a project context but no fallback, asserts the output does not
/// contain the fallback and ends with the project context.
/// Test: this test.
#[test]
fn native_tier_omits_fallback_section() {
    let cfg = config_with_prompt("Agent prompt.");
    let out = assemble_system_prompt(&cfg, Some("PROJECT_END"), None);
    assert!(out.ends_with("PROJECT_END"));
    assert_eq!(out.matches("\n\n---\n\n").count(), 2);
}

/// `BASE_PREAMBLE` contains every load-bearing block of spec §2a.
///
/// Why: The preamble is a machine-readable contract; a missing block would
/// silently weaken every agent's instructions.
/// What: Asserts the presence of the tool-use, filesystem, output, and finish
/// signals via their key phrases.
/// Test: this test.
#[test]
fn base_preamble_contains_required_blocks() {
    assert!(
        BASE_PREAMBLE.contains("tool call"),
        "tool-use protocol block missing"
    );
    assert!(
        BASE_PREAMBLE.contains("project root"),
        "filesystem-safety block missing"
    );
    assert!(
        BASE_PREAMBLE.contains("## Summary"),
        "output-convention block missing"
    );
    assert!(
        BASE_PREAMBLE.contains("contains no tool call"),
        "finish-convention block missing"
    );
    assert!(
        BASE_PREAMBLE.contains("trusty-code harness"),
        "identity/role block missing"
    );
}

/// `BASE_PREAMBLE` carries no host- or model-specific tokens (spec §2a/§3).
///
/// Why: Any model name, provider name, or host path in the BASE preamble would
/// break the byte-identical guarantee.
/// What: Asserts the preamble does not mention common provider/model slugs.
/// Test: this test.
#[test]
fn base_preamble_is_model_agnostic() {
    for forbidden in ["claude", "openai", "gpt", "anthropic", "qwen", "deepseek"] {
        assert!(
            !BASE_PREAMBLE.to_lowercase().contains(forbidden),
            "BASE_PREAMBLE must not mention provider/model token {forbidden:?}"
        );
    }
}

/// `BASE_PREAMBLE_VERSION` is a three-component semver-shaped string (spec D5).
///
/// Why: The parity report records this token; a malformed version would make
/// the report ambiguous.
/// What: Splits on `.` and asserts three numeric components.
/// Test: this test.
#[test]
fn base_preamble_version_is_semver_shaped() {
    let parts: Vec<&str> = BASE_PREAMBLE_VERSION.split('.').collect();
    assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH");
    for part in parts {
        assert!(
            part.chars().all(|c| c.is_ascii_digit()),
            "non-numeric version component: {part:?}"
        );
    }
}

/// The `PromptAssembler` struct produces the same output as the free function.
///
/// Why: The two entry points must never diverge.
/// What: Compares `PromptAssembler::assemble` to `assemble_system_prompt` for
/// identical inputs.
/// Test: this test.
#[test]
fn assembler_struct_matches_free_function() {
    let cfg = config_with_prompt("Agent prompt.");
    let via_struct = PromptAssembler.assemble(&cfg, Some("ctx"), Some("fb"));
    let via_fn = assemble_system_prompt(&cfg, Some("ctx"), Some("fb"));
    assert_eq!(via_struct, via_fn);
}

/// All four sections are present in the fully-populated assembled prompt.
///
/// Why: Acceptance criterion — "all sections present in assembled prompt".
/// What: Asserts BASE, agent, project, and fallback markers all appear.
/// Test: this test.
#[test]
fn all_sections_present_when_supplied() {
    let cfg = config_with_prompt("AGENT");
    let out = assemble_system_prompt(&cfg, Some("PROJECT"), Some("FALLBACK"));
    assert!(out.contains(BASE_PREAMBLE));
    assert!(out.contains("AGENT"));
    assert!(out.contains("PROJECT"));
    assert!(out.contains("FALLBACK"));
}
