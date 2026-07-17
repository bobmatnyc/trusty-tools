//! Tests for the embedded default agents & skills tables.
//!
//! Why: kept out of `mod.rs` so the include-table file stays thin per the
//! module's own "keep it lean" convention; test/benchmark files get the
//! wider 1500-SLOC cap (see repo `CLAUDE.md` SLOC policy) so the fuller
//! assertions here don't threaten `mod.rs`'s production budget.
//! What: projects every embedded agent `.md` and checks name consistency and
//! field-identity against the behavior the retired TOML fixtures produced
//! (#2897 Slice C); checks every embedded skill name is unique, non-empty,
//! and frontmatter-fenced.
//! Test: this file — self-describing.

use super::*;
use crate::agents::md_loader::project_embedded_md;

/// Every embedded agent `.md` parses, and its frontmatter `name:` matches the
/// table key it is filed under.
///
/// Why: A typo in either the `.md` frontmatter or the table entry would
/// silently break `agents::load_all_agents`'s embedded-fallback at runtime
/// instead of failing fast in CI.
/// Test: this test.
#[test]
fn default_agents_parse_and_names_match() {
    assert_eq!(DEFAULT_AGENTS.len(), 3);
    for agent in DEFAULT_AGENTS {
        let cfg = project_embedded_md(agent.name, agent.md);
        assert_eq!(
            cfg.agent.name, agent.name,
            "table key must match frontmatter name:"
        );
    }
}

/// Every embedded default agent's `.md`-derived `AgentConfig` is
/// field-identical to what the retired `.toml` fixture produced, for every
/// field the `.md` frontmatter format is able to express.
///
/// Why: this is the acceptance criterion for #2897 Slice C — converting the
/// asset format must not change runtime agent behavior. The expected values
/// below were captured directly from `AgentConfig::from_toml_str` on the
/// retired `engineer.toml`/`qa-agent.toml`/`code-reviewer.toml` fixtures
/// (still visible in this PR's diff) before they were deleted. `llm.temperature`
/// and `runner` are intentionally excluded: trusty-mpm's shared frontmatter
/// grammar has no `temperature:` key and no consumer ever reads
/// `AgentConfig.llm.temperature` (`grep -rn "llm.temperature" crates/trusty-code/src`
/// has no non-test/non-declaration hit), and `runner: None` from the `.md`
/// path is behaviorally identical to the TOML fixtures' explicit
/// `kind = "in_process"` — `RunnerKind::default()` is `InProcess` and no call
/// site distinguishes an absent `[runner]` from an explicit one (see
/// `agents::config`'s `runner_kind_defaults_to_in_process` test). The
/// system-prompt body is compared against `toml_content.trim()`, not the raw
/// TOML string, because `md_loader::extract_body` trims the composed body's
/// surrounding whitespace — including the TOML content's single trailing
/// `\n` — for every `.md` agent, disk or embedded; that trim is an
/// established, pre-existing property of the shared body-extraction path
/// (Slice B), not something this slice introduces.
/// What: one assertion block per default agent, covering `name`, `model`
/// (`None` for all three — neither format set it), `max_tokens`,
/// `tools.allowed` (exact list, `Some(...)` semantics preserved), and
/// `system_prompt.content`.
/// Test: this test.
#[test]
fn default_agents_field_identical_to_retired_toml() {
    let engineer = project_embedded_md("engineer", ENGINEER_MD);
    assert_eq!(engineer.agent.name, "engineer");
    assert_eq!(engineer.agent.model, None);
    assert_eq!(engineer.llm.max_tokens, Some(8192));
    assert_eq!(
        engineer.tools.and_then(|t| t.allowed),
        Some(vec![
            "read_file".to_string(),
            "write_file".to_string(),
            "write_files".to_string(),
            "edit".to_string(),
            "grep".to_string(),
            "glob".to_string(),
            "list_dir".to_string(),
            "bash".to_string(),
            "search_code".to_string(),
            "use_skill".to_string(),
            "finish_task".to_string(),
        ])
    );
    let engineer_toml_content = "You are a software engineer sub-agent. You implement the task you are given: read the existing code before writing new code, follow the project's established patterns and naming conventions, and prefer editing existing files over creating new ones.\n\nRules:\n- Correct, complete implementations over minimal ones. Do not sacrifice correctness for brevity.\n- Fix root causes, not symptoms.\n- Include error handling and input validation where it affects reliability.\n- Never leave dead code, commented-out blocks, or duplicate implementations of the same logic behind.\n- Write tests that cover the behavior you added or changed, then run them and report the actual (not assumed) results.\n- Never fabricate command output. If a command's output is empty or unavailable, say so rather than inventing a result.\n\nWhen you believe the task is complete, call `finish_task` with a summary of what changed and how you verified it.\n";
    assert_eq!(engineer.system_prompt.content, engineer_toml_content.trim());

    let qa_agent = project_embedded_md("qa-agent", QA_AGENT_MD);
    assert_eq!(qa_agent.agent.name, "qa-agent");
    assert_eq!(qa_agent.agent.model, None);
    assert_eq!(qa_agent.llm.max_tokens, Some(8192));
    assert_eq!(
        qa_agent.tools.and_then(|t| t.allowed),
        Some(vec![
            "read_file".to_string(),
            "grep".to_string(),
            "glob".to_string(),
            "list_dir".to_string(),
            "bash".to_string(),
            "search_code".to_string(),
            "use_skill".to_string(),
            "finish_task".to_string(),
        ])
    );
    let qa_agent_toml_content = "You are a QA sub-agent. Your job is to verify that an implementation actually does what it claims, not to trust the implementer's summary.\n\nRules:\n- Run the project's real test suite and quote the raw output; never summarize a test run in your own words in place of the output.\n- Treat \"0 tests ran\" or a suspiciously small number of skipped/ignored tests as a failure to investigate, not a pass.\n- Test the entry point end-to-end (the binary starts, the CLI runs, the endpoint responds) in addition to unit-level checks.\n- Cover edge cases and error paths, not just the happy path.\n- When you find a bug, report it precisely: the failing command, the actual output, and the expected output. Do not attempt to fix production code yourself — hand findings back to the engineer.\n\nWhen your verification pass is complete, call `finish_task` with a pass/fail verdict and the evidence behind it.\n";
    assert_eq!(qa_agent.system_prompt.content, qa_agent_toml_content.trim());

    let code_reviewer = project_embedded_md("code-reviewer", CODE_REVIEWER_MD);
    assert_eq!(code_reviewer.agent.name, "code-reviewer");
    assert_eq!(code_reviewer.agent.model, None);
    assert_eq!(code_reviewer.llm.max_tokens, Some(8192));
    assert_eq!(
        code_reviewer.tools.and_then(|t| t.allowed),
        Some(vec![
            "read_file".to_string(),
            "grep".to_string(),
            "glob".to_string(),
            "list_dir".to_string(),
            "search_code".to_string(),
            "use_skill".to_string(),
            "finish_task".to_string(),
        ])
    );
    let code_reviewer_toml_content = "You are an adversarial code-review sub-agent. You review code someone else wrote or changed; you do not write or edit production code yourself.\n\nRules:\n- Read the actual diff/changeset before forming an opinion — do not review from a description alone.\n- Prioritize correctness bugs, security issues, and data-loss risks over style nits.\n- Only report a finding you are reasonably confident about (roughly 80%+ confidence); note lower-confidence concerns separately as questions, not as findings.\n- Cite the exact file and line for every finding.\n- Give one of three verdicts: APPROVE, WARN (non-blocking issues, safe to merge with follow-up), or BLOCK (must be fixed before merge) — and justify the verdict with the findings that drove it.\n\nWhen your review is complete, call `finish_task` with the verdict and the list of findings.\n";
    assert_eq!(
        code_reviewer.system_prompt.content,
        code_reviewer_toml_content.trim()
    );
}

/// The embedded fallback still fires when the disk `.claude/agents` dir is
/// empty, and still yields exactly the three default agent names — proving
/// the `.md` conversion did not break `load_all_agents`'s fallback wiring.
///
/// Why: #2897 Slice C's non-breaking claim rests on this: a fresh project
/// with no `.claude/agents/` must still boot with `engineer`/`qa-agent`/
/// `code-reviewer` available, exactly as it did when the defaults were TOML.
/// What: calls `crate::agents::load_all_agents` on a nonexistent directory
/// and asserts the three names come back.
/// Test: this test.
#[test]
fn embedded_fallback_still_fires_and_yields_same_three_names() {
    let agents = crate::agents::load_all_agents(std::path::Path::new("/nonexistent/agents/dir"));
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
    assert_eq!(names, vec!["engineer", "qa-agent", "code-reviewer"]);
}

/// Every embedded skill name is unique and non-empty.
///
/// Why: `skills::discover_skill_metadata` sorts and dedupes by name; a
/// duplicate embedded name would silently shadow another skill.
/// Test: this test.
#[test]
fn default_skills_names_are_unique() {
    assert_eq!(DEFAULT_SKILLS.len(), 28);
    let mut names: Vec<&str> = DEFAULT_SKILLS.iter().map(|s| s.name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "no duplicate embedded skill names");
    for skill in DEFAULT_SKILLS {
        assert!(!skill.name.is_empty());
        assert!(
            skill.skill_md.trim_start().starts_with("---"),
            "embedded skill '{}' must open with a frontmatter fence",
            skill.name
        );
    }
}
