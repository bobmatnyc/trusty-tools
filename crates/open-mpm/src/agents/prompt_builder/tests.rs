//! Unit tests for layered system-prompt assembly.
//!
//! Why: Pins the slot ordering (goal → harness → memory → base → project →
//! user-memory → skills → mcp → subagent → output-style), the CLAUDE.md
//! ancestor walk, and the section-dedup pass so prompt-assembly refactors
//! can't silently reorder or drop layers.
//! What: Exercises `SystemPromptBuilder` builder methods, `walk_project_instructions`,
//! `layer_count`, and `build`, plus the compiled-in harness-protocol constants.
//! Test: This module IS the test surface.

use super::*;
use crate::test_env::HOME_LOCK;
use std::fs;

fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!("open-mpm-prompt-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn builder_empty_has_only_base() {
    let b = SystemPromptBuilder::new("base prompt");
    assert_eq!(b.layer_count(), 1);
    assert_eq!(b.build(), "base prompt");
}

#[test]
fn build_joins_with_separator() {
    let out = SystemPromptBuilder::new("BASE")
        .add_skill("SKILL A")
        .add_skill("SKILL B")
        .add_subagent_context("SUB")
        .build();
    let parts: Vec<&str> = out.split(LAYER_SEPARATOR).collect();
    assert_eq!(parts, vec!["BASE", "SKILL A", "SKILL B", "SUB"]);
}

#[test]
fn user_memory_injected_between_project_and_skills() {
    let out = SystemPromptBuilder::new("BASE")
        .with_user_memory("## User Memory\nprefers snake_case")
        .add_skill("SKILL")
        .build();
    let parts: Vec<&str> = out.split(LAYER_SEPARATOR).collect();
    assert_eq!(
        parts,
        vec!["BASE", "## User Memory\nprefers snake_case", "SKILL"]
    );
}

#[test]
fn user_memory_empty_string_is_ignored() {
    let b = SystemPromptBuilder::new("BASE").with_user_memory("   ");
    assert_eq!(b.layer_count(), 1);
    assert_eq!(b.build(), "BASE");
}

#[test]
fn user_memory_counted_in_layer_count() {
    let b = SystemPromptBuilder::new("BASE")
        .with_user_memory("MEMORY")
        .add_skill("S");
    assert_eq!(b.layer_count(), 3);
}

#[test]
fn user_memory_after_project_layers() {
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempdir();
    fs::write(root.join("CLAUDE.md"), "PROJECT RULES").unwrap();

    unsafe {
        std::env::set_var("HOME", tempdir());
    }
    let out = SystemPromptBuilder::new("BASE")
        .walk_project_instructions(&root)
        .with_user_memory("USER PREFS")
        .add_skill("SKILL")
        .build();

    let project_pos = out.find("PROJECT RULES").unwrap();
    let memory_pos = out.find("USER PREFS").unwrap();
    let skill_pos = out.find("SKILL").unwrap();
    assert!(
        project_pos < memory_pos,
        "project layers must precede user memory"
    );
    assert!(memory_pos < skill_pos, "user memory must precede skills");
}

#[test]
fn root_before_subdir() {
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempdir();
    let nested = root.join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join("CLAUDE.md"), "ROOT RULES").unwrap();
    fs::write(nested.join("CLAUDE.md"), "NESTED RULES").unwrap();

    unsafe {
        std::env::set_var("HOME", tempdir());
    }
    let builder = SystemPromptBuilder::new("BASE").walk_project_instructions(&nested);

    assert_eq!(builder.project_layers.len(), 2);

    let out = builder.build();
    assert!(out.contains("ROOT RULES"));
    assert!(out.contains("NESTED RULES"));
    let root_pos = out.find("ROOT RULES").unwrap();
    let nested_pos = out.find("NESTED RULES").unwrap();
    assert!(root_pos < nested_pos, "root CLAUDE.md must precede nested");
}

#[test]
fn agents_md_recognized_alongside_claude_md() {
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempdir();
    fs::write(root.join("CLAUDE.md"), "CLAUDE MD BODY").unwrap();
    fs::write(root.join("AGENTS.md"), "AGENTS MD BODY").unwrap();

    unsafe {
        std::env::set_var("HOME", tempdir());
    }
    let builder = SystemPromptBuilder::new("BASE").walk_project_instructions(&root);
    assert_eq!(builder.project_layers.len(), 2);

    let out = builder.build();
    assert!(out.contains("CLAUDE MD BODY"));
    assert!(out.contains("AGENTS MD BODY"));
    let c = out.find("CLAUDE MD BODY").unwrap();
    let a = out.find("AGENTS MD BODY").unwrap();
    assert!(c < a, "CLAUDE.md must precede AGENTS.md at the same level");
}

#[test]
fn labeled_separators_contain_path() {
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempdir();
    let claude = root.join("CLAUDE.md");
    fs::write(&claude, "ROOT RULES").unwrap();

    unsafe {
        std::env::set_var("HOME", tempdir());
    }
    let out = SystemPromptBuilder::new("BASE")
        .walk_project_instructions(&root)
        .build();

    let expected_label = format!("[from: {}]", claude.display());
    assert!(
        out.contains(&expected_label),
        "expected separator with {expected_label:?}; got: {out}"
    );
}

#[test]
fn walk_skips_missing_files() {
    let root = tempdir();
    let nested = root.join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    let builder = SystemPromptBuilder::new("BASE").walk_project_instructions(&nested);
    assert!(builder.project_layers.is_empty());
}

#[test]
fn walk_picks_up_real_project_claude_md() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let claude = manifest_dir.join("CLAUDE.md");
    if !claude.exists() {
        return;
    }
    let out = SystemPromptBuilder::new("BASE")
        .walk_project_instructions(&manifest_dir)
        .build();
    assert!(out.contains("open-mpm"));
}

#[test]
fn build_prepends_goal_block_when_set() {
    let goal = crate::context::GoalBlock {
        primary: "Ship it".into(),
        secondary: vec!["fast".into()],
        task_split_required: false,
    };
    let out = SystemPromptBuilder::new("BASE BODY")
        .with_goal_block(Some(goal))
        .build();
    let goal_pos = out.find("## TASK GOALS").expect("goal header present");
    let base_pos = out.find("BASE BODY").expect("base present");
    assert!(goal_pos < base_pos, "goal header must precede base");
}

#[test]
fn build_without_goal_block_omits_header() {
    let out = SystemPromptBuilder::new("BASE")
        .with_goal_block(None)
        .build();
    assert!(!out.contains("## TASK GOALS"));
}

#[test]
fn layer_count_tracks_additions() {
    let b = SystemPromptBuilder::new("BASE")
        .add_skill("S")
        .add_subagent_context("X");
    assert_eq!(b.layer_count(), 3);
}

#[test]
fn harness_layer_injected_before_base() {
    let out = SystemPromptBuilder::new("BASE BODY")
        .add_harness_layer("HARNESS PROTOCOL")
        .build();
    let h_pos = out.find("HARNESS PROTOCOL").expect("harness present");
    let b_pos = out.find("BASE BODY").expect("base present");
    assert!(h_pos < b_pos, "harness layer must precede base");
}

#[test]
fn harness_layer_absent_when_empty() {
    let out = SystemPromptBuilder::new("BASE")
        .add_harness_layer("   ")
        .add_harness_layer("")
        .build();
    assert_eq!(out, "BASE");
}

#[test]
fn harness_layer_counted_in_layer_count() {
    let b = SystemPromptBuilder::new("BASE")
        .add_harness_layer("H1")
        .add_harness_layer("H2")
        .add_skill("S");
    assert_eq!(b.layer_count(), 4);
}

// Content + ordering tests for the compiled-in harness-protocol constants
// live in the sibling `harness_tests` module.

// ── MCP + project memory layers (#241) ────────────────────────────────

#[test]
fn mcp_layer_appears_after_skills_before_subagent() {
    let out = SystemPromptBuilder::new("BASE")
        .add_skill("SKILL")
        .add_mcp_layer("## MCP TOOLS")
        .add_subagent_context("SUB")
        .build();
    let s = out.find("SKILL").unwrap();
    let m = out.find("## MCP TOOLS").unwrap();
    let sub = out.find("SUB").unwrap();
    assert!(s < m, "skill must precede MCP layer");
    assert!(m < sub, "MCP layer must precede subagent layer");
}

#[test]
fn mcp_layer_empty_input_is_dropped() {
    let b = SystemPromptBuilder::new("BASE").add_mcp_layer("   ");
    assert_eq!(b.layer_count(), 1);
    assert_eq!(b.build(), "BASE");
}

#[test]
fn mcp_layer_counted_in_layer_count() {
    let b = SystemPromptBuilder::new("BASE")
        .add_skill("S")
        .add_mcp_layer("MCP");
    assert_eq!(b.layer_count(), 3);
}

#[test]
fn memory_layer_renders_bullet_list() {
    let out = SystemPromptBuilder::new("BASE BODY")
        .add_memory_layer(vec!["fact one".into(), "fact two".into()])
        .build();
    assert!(out.contains("## Project Memory"));
    assert!(out.contains("- fact one"));
    assert!(out.contains("- fact two"));
    // Must precede base.
    let mem_pos = out.find("## Project Memory").unwrap();
    let base_pos = out.find("BASE BODY").unwrap();
    assert!(mem_pos < base_pos, "memory layer must precede base");
}

#[test]
fn memory_layer_empty_vec_is_noop() {
    let b = SystemPromptBuilder::new("BASE").add_memory_layer(vec![]);
    assert_eq!(b.layer_count(), 1);
    assert_eq!(b.build(), "BASE");
}

#[test]
fn memory_layer_filters_empty_strings() {
    let b = SystemPromptBuilder::new("BASE").add_memory_layer(vec![
        "".into(),
        "   ".into(),
        "real".into(),
    ]);
    assert_eq!(b.layer_count(), 2);
    let out = b.build();
    assert!(out.contains("- real"));
}

#[test]
fn memory_layer_counted_in_layer_count() {
    let b = SystemPromptBuilder::new("BASE")
        .add_memory_layer(vec!["one".into()])
        .add_skill("S");
    assert_eq!(b.layer_count(), 3);
}

#[test]
fn full_layer_ordering_with_all_slots() {
    let goal = crate::context::GoalBlock {
        primary: "Goal".into(),
        secondary: vec![],
        task_split_required: false,
    };
    let out = SystemPromptBuilder::new("BASE_BODY")
        .with_goal_block(Some(goal))
        .add_harness_layer("HARNESS")
        .add_memory_layer(vec!["MEM_FACT".into()])
        .with_user_memory("USER_MEM")
        .add_skill("SKILL_ONE")
        .add_mcp_layer("MCP_BLOCK")
        .add_subagent_context("SUB_CTX")
        .build();
    // Expected: goal → harness → memory → base → user_memory → skill → mcp → subagent
    let pos = |needle: &str| {
        out.find(needle)
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    assert!(pos("## TASK GOALS") < pos("HARNESS"));
    assert!(pos("HARNESS") < pos("MEM_FACT"));
    assert!(pos("MEM_FACT") < pos("BASE_BODY"));
    assert!(pos("BASE_BODY") < pos("USER_MEM"));
    assert!(pos("USER_MEM") < pos("SKILL_ONE"));
    assert!(pos("SKILL_ONE") < pos("MCP_BLOCK"));
    assert!(pos("MCP_BLOCK") < pos("SUB_CTX"));
}

// ── #420: Output-style compression + section dedup ───────────────────

#[test]
fn output_style_full_appended_at_end() {
    let out = SystemPromptBuilder::new("BASE BODY")
        .add_skill("SKILL CONTENT")
        .with_output_style(crate::compress::OutputStyle::Full)
        .build();
    let base_pos = out.find("BASE BODY").unwrap();
    let skill_pos = out.find("SKILL CONTENT").unwrap();
    let style_pos = out
        .find("Output Style: Full")
        .expect("style fragment present");
    assert!(base_pos < skill_pos);
    assert!(skill_pos < style_pos, "output style must be last layer");
}

#[test]
fn output_style_none_appends_nothing() {
    let out = SystemPromptBuilder::new("BASE")
        .with_output_style(crate::compress::OutputStyle::None)
        .build();
    assert!(!out.contains("Output Style"));
    assert_eq!(out, "BASE");
}

#[test]
fn output_style_ultra_contains_telegraphic_rule() {
    let out = SystemPromptBuilder::new("BASE")
        .with_output_style(crate::compress::OutputStyle::Ultra)
        .build();
    assert!(out.contains("Output Style: Ultra"));
    assert!(out.contains("telegraphic"));
}

#[test]
fn build_runs_section_dedup() {
    // A single skill that itself contains two identical `## Shared Heading`
    // sections — section dedup should drop the second occurrence. (Two
    // separate skill layers are interleaved with LAYER_SEPARATOR `---`,
    // which breaks exact-match dedup; that's expected behavior.)
    let dup_skill = "intro\n## Shared Heading\nbody text here\n## Shared Heading\nbody text here";
    let out = SystemPromptBuilder::new("BASE")
        .add_skill(dup_skill)
        .build();
    let count = out.matches("## Shared Heading").count();
    assert_eq!(count, 1, "section dedup should drop duplicate heading");
}

#[test]
fn output_style_counted_in_layer_count() {
    let b = SystemPromptBuilder::new("BASE").with_output_style(crate::compress::OutputStyle::Full);
    assert_eq!(b.layer_count(), 2);
    let b = SystemPromptBuilder::new("BASE").with_output_style(crate::compress::OutputStyle::None);
    assert_eq!(b.layer_count(), 1);
}

// ── #478: Agent-identity template variables ──────────────────────────

#[test]
fn agent_model_substituted_in_prompt() {
    let out = SystemPromptBuilder::new("running on {{AGENT_MODEL}} today")
        .with_agent_context("anthropic/claude-opus-4-7", "claude-code")
        .build();
    assert!(
        out.contains("running on anthropic/claude-opus-4-7 today"),
        "AGENT_MODEL placeholder must be replaced; got: {out}"
    );
    assert!(
        !out.contains("{{AGENT_MODEL}}"),
        "placeholder must not survive substitution"
    );
}

#[test]
fn agent_runner_substituted_in_prompt() {
    let out = SystemPromptBuilder::new("runner is {{AGENT_RUNNER}}")
        .with_agent_context("some/model", "in-process")
        .build();
    assert!(
        out.contains("runner is in-process"),
        "AGENT_RUNNER placeholder must be replaced; got: {out}"
    );
    assert!(!out.contains("{{AGENT_RUNNER}}"));
}

#[test]
fn agent_context_noop_when_no_placeholders() {
    let out = SystemPromptBuilder::new("plain base prompt")
        .with_agent_context("model", "runner")
        .build();
    assert_eq!(out, "plain base prompt");
}

#[test]
fn harness_layer_after_goal_block_before_base() {
    let goal = crate::context::GoalBlock {
        primary: "Ship it".into(),
        secondary: vec![],
        task_split_required: false,
    };
    let out = SystemPromptBuilder::new("BASE BODY")
        .with_goal_block(Some(goal))
        .add_harness_layer("HARNESS PROTOCOL")
        .build();
    let goal_pos = out.find("## TASK GOALS").expect("goal header");
    let harness_pos = out.find("HARNESS PROTOCOL").expect("harness");
    let base_pos = out.find("BASE BODY").expect("base");
    assert!(goal_pos < harness_pos, "goal precedes harness");
    assert!(harness_pos < base_pos, "harness precedes base");
}
