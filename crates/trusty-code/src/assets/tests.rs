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
use crate::agents::md_loader::{project_embedded_md, project_embedded_md_with_extends};

/// Every embedded agent `.md` parses (directly for the 3 originals, via the
/// in-memory extends composer for the 28 roster agents), and its frontmatter
/// `name:` matches the table key it is filed under.
///
/// Why: A typo in either the `.md` frontmatter or the table entry would
/// silently break `agents::load_all_agents`'s embedded-fallback at runtime
/// instead of failing fast in CI. This is also the acceptance test for Slice
/// E3 (#2958): every one of the 32 entries must actually compose (a `Composed`
/// variant that panics here rather than resolving would otherwise only be
/// caught at runtime by `load_embedded_default_agents`'s log-and-skip path).
/// Test: this test.
#[test]
fn default_agents_parse_and_names_match() {
    assert_eq!(
        DEFAULT_AGENTS.len(),
        32,
        "4 originals (engineer, qa-agent, code-reviewer, pm) + 28 roster agents"
    );
    for agent in DEFAULT_AGENTS {
        let cfg = match agent {
            EmbeddedAgent::Direct { name, md } => project_embedded_md(name, md),
            EmbeddedAgent::Composed { name } => project_embedded_md_with_extends(name)
                .unwrap_or_else(|e| panic!("roster agent '{name}' failed to compose: {e}")),
        };
        assert_eq!(
            cfg.agent.name,
            agent.name(),
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
/// empty, and yields the full 32-agent roster with the original 4 defaults
/// intact as the first four entries — proving Slice E3's roster expansion
/// (and #3437's `pm` addition) did not disturb the original fallback wiring
/// `.md` (#2897 Slice C) established.
///
/// Why: #2897 Slice C's non-breaking claim rests on this: a fresh project
/// with no `.claude/agents/` must still boot with `engineer`/`qa-agent`/
/// `code-reviewer`/`pm` available, exactly as it did when the defaults were
/// TOML — Slice E3 only ADDS the 28 roster agents after them, never replaces
/// or reorders the originals.
/// What: calls `crate::agents::load_all_agents` on a nonexistent directory;
/// asserts the returned names' first four entries are exactly
/// `["engineer", "qa-agent", "code-reviewer", "pm"]` and the full 32-name
/// list matches `crate::assets::DEFAULT_AGENTS`'s declared order with no
/// duplicates.
/// Test: this test.
#[test]
fn embedded_fallback_still_fires_and_yields_32_agents_with_original_4_intact() {
    let agents = crate::agents::load_all_agents(std::path::Path::new("/nonexistent/agents/dir"));
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();

    assert_eq!(names.len(), 32, "32-agent roster: 4 originals + 28 roster");
    assert_eq!(
        &names[..4],
        &["engineer", "qa-agent", "code-reviewer", "pm"],
        "the original 4 defaults must remain first and intact"
    );

    let expected: Vec<&str> = DEFAULT_AGENTS.iter().map(|a| a.name()).collect();
    assert_eq!(
        names, expected,
        "fallback order must match DEFAULT_AGENTS's declared order exactly"
    );

    let mut deduped = names.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(deduped.len(), names.len(), "no duplicate agent names");
}

/// The 5 `BASE-*` extends templates are NEVER dispatchable — they must not
/// appear anywhere in `DEFAULT_AGENTS` or the fallback roster it produces.
///
/// Why: #2958's roster decision is explicit that `BASE-AGENT`, `BASE-ENGINEER`,
/// `BASE-OPS`, `BASE-QA`, and `BASE-RESEARCH` are extends-sources ONLY. A
/// `BASE-*` entry leaking into the dispatchable roster would let a caller
/// invoke a template fragment (no concrete role, designed to be composed
/// into a leaf agent, not run standalone) as if it were a real agent.
/// What: asserts none of the 32 `DEFAULT_AGENTS` names matches any of the 5
/// base template names (case-insensitive, since the source table keys them
/// `BASE-QA.md` while `extends:` references use `base-qa`).
/// Test: this test.
#[test]
fn base_templates_are_never_dispatchable() {
    const BASE_NAMES: &[&str] = &[
        "base-agent",
        "base-engineer",
        "base-ops",
        "base-qa",
        "base-research",
    ];
    for agent in DEFAULT_AGENTS {
        let lower = agent.name().to_ascii_lowercase();
        assert!(
            !BASE_NAMES.contains(&lower.as_str()),
            "BASE template '{}' must never be dispatchable",
            agent.name()
        );
    }
}

/// No two entries in the 32-agent `DEFAULT_AGENTS` roster share a dispatch
/// name — in particular, trusty-mpm's own `engineer` agent (excluded from
/// the roster upstream specifically because it collides with tcode's
/// `engineer` default) does not sneak back in under any composed entry.
///
/// Why: the #2958 roster decision explicitly calls out `engineer` as
/// EXCLUDEd from the 28-agent import "(name-collides with tcode's own
/// default)" — this test is the regression pin for that exclusion, and a
/// general guard against any future roster addition silently shadowing an
/// existing dispatch name.
/// What: collects all 32 names, dedupes, asserts the length is unchanged;
/// separately asserts `"engineer"` appears exactly once.
/// Test: this test.
#[test]
fn no_name_collisions_across_the_32_agent_roster() {
    let names: Vec<&str> = DEFAULT_AGENTS.iter().map(|a| a.name()).collect();
    assert_eq!(names.len(), 32);

    let mut deduped = names.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        names.len(),
        "no name collisions across the 32-agent roster: {names:?}"
    );

    assert_eq!(
        names.iter().filter(|&&n| n == "engineer").count(),
        1,
        "tcode's own 'engineer' must be the only 'engineer' entry -- mpm's \
         'engineer' agent is excluded from the roster upstream precisely to \
         avoid this collision"
    );
}

/// The four reviewer-intent roster agents Bob designated for restrictive
/// read-only tooling (2026-07-18, Slice E3, #2958) — `qa`, `code-critic`,
/// `code-analyzer`, `web-qa` — compose to an `AgentConfig` carrying exactly
/// the read-only tool allowlist, mirroring tcode's own `code-reviewer`
/// default (no `write_file`/`edit`/`bash`).
///
/// Why: this is the acceptance test for the E3 tools-restriction decision —
/// a frontmatter typo or a dropped `tools:` line would silently leave these
/// "reviewer" agents with full read/write/bash access, defeating the whole
/// point of the restriction.
/// What: composes each of the four via `project_embedded_md_with_extends`
/// (exercising the SAME path `load_embedded_default_agents` uses) and
/// asserts `cfg.tools.allowed` equals the read-only list, with no
/// `write_file`, `edit`, or `bash` entry.
/// Test: this test.
#[test]
fn restricted_reviewer_agents_carry_read_only_tools() {
    let expected_read_only: Vec<String> = vec![
        "read_file".to_string(),
        "grep".to_string(),
        "glob".to_string(),
        "list_dir".to_string(),
        "search_code".to_string(),
        "use_skill".to_string(),
        "finish_task".to_string(),
    ];

    for name in ["qa", "code-critic", "code-analyzer", "web-qa"] {
        let cfg = project_embedded_md_with_extends(name)
            .unwrap_or_else(|e| panic!("failed to compose '{name}': {e}"));
        let allowed = cfg.tools.and_then(|t| t.allowed);
        assert_eq!(
            allowed,
            Some(expected_read_only.clone()),
            "'{name}' must carry the restrictive read-only tools: override"
        );
        assert!(
            !allowed
                .as_ref()
                .unwrap()
                .iter()
                .any(|t| t == "write_file" || t == "edit" || t == "bash"),
            "'{name}' must not carry any write/edit/bash-mutation tool"
        );
    }
}

/// `documentation` and `research` remain byte-identical to trusty-mpm's
/// source and unrestricted (`tools: None` — all tools allowed), per Bob's
/// explicit 2026-07-18 ruling that they build docs and research reports
/// rather than issue verdicts.
///
/// Why: distinguishes "no override was accidentally added" from "an override
/// was added but with the wrong value" — the previous test only pins the
/// four restricted agents; this one pins that the two agents Bob explicitly
/// exempted were NOT swept up by the same change.
/// What: composes both via `project_embedded_md_with_extends` and asserts
/// `cfg.tools.allowed` is `None`.
/// Test: this test.
#[test]
fn documentation_and_research_remain_unrestricted() {
    for name in ["documentation", "research"] {
        let cfg = project_embedded_md_with_extends(name)
            .unwrap_or_else(|e| panic!("failed to compose '{name}': {e}"));
        assert_eq!(
            cfg.tools.and_then(|t| t.allowed),
            None,
            "'{name}' must remain unrestricted (no tools: override)"
        );
    }
}

/// `crate::assets::DEFAULT_AGENTS`'s 28 `EmbeddedAgent::Composed` entries
/// every resolve to a real key in `EMBEDDED_TM_AGENT_SOURCES` — no typo'd
/// roster name that would silently degrade to a skipped agent at runtime.
///
/// Why: `load_embedded_default_agents` logs-and-skips a `Composed` entry
/// whose name isn't found in `EMBEDDED_TM_AGENT_SOURCES` rather than
/// panicking (see that function's doc) — a typo there would silently shrink
/// the roster below 31 with only a log line as evidence. This test fails
/// loudly in CI instead.
/// What: for every `Composed` entry, asserts its (lowercased) name matches
/// some `EMBEDDED_TM_AGENT_SOURCES` key with the `.md` suffix stripped.
/// Test: this test.
#[test]
fn every_composed_roster_name_resolves_in_embedded_tm_agent_sources() {
    let source_keys: Vec<String> = EMBEDDED_TM_AGENT_SOURCES
        .iter()
        .map(|(name, _)| name.trim_end_matches(".md").to_ascii_lowercase())
        .collect();

    for agent in DEFAULT_AGENTS {
        if let EmbeddedAgent::Composed { name } = agent {
            assert!(
                source_keys.iter().any(|k| k == name),
                "roster name '{name}' has no matching EMBEDDED_TM_AGENT_SOURCES key"
            );
        }
    }
}

/// `task::protocol::DEFAULT_TASK_RUN_AGENT_NAME` (the literal `task.run`
/// falls back to when `agent_name` is omitted) resolves against the embedded
/// roster via the SAME resolver the daemon uses (#3437's drift guard).
///
/// Why: #3437's root cause was exactly this pairing silently drifting apart —
/// `task/protocol.rs` defaulted an omitted `agent_name` to `"pm"` while
/// neither disk nor [`DEFAULT_AGENTS`] had a `pm` entry, so every
/// daemon-default (including every GUI-initiated, agent-name-omitting) run
/// failed agent resolution before a single turn executed. Referencing the
/// shared [`crate::task::protocol::DEFAULT_TASK_RUN_AGENT_NAME`] const from
/// both this test and the `task_run` call site (rather than two independent
/// `"pm"` string literals, one here and one there) means a future rename of
/// either side is a compile-time rename, not a silent divergence a test could
/// miss.
/// What: calls `crate::agents::resolve_agent` — the exact function
/// `task::executor::run_and_record`'s `pm_config` resolution uses — with a
/// nonexistent disk dir and `DEFAULT_TASK_RUN_AGENT_NAME`, and asserts it
/// resolves `Ok` to an `AgentConfig` named `"pm"`.
/// Test: this test.
#[test]
fn default_task_run_agent_resolves_against_default_agents() {
    let cfg = crate::agents::resolve_agent(
        std::path::Path::new("/nonexistent/agents/dir"),
        crate::task::protocol::DEFAULT_TASK_RUN_AGENT_NAME,
    )
    .unwrap_or_else(|e| {
        panic!(
            "task.run's default agent_name '{}' must resolve against the embedded \
             roster (disk-then-embedded, `agents::resolve_agent`) — it did not: {e}",
            crate::task::protocol::DEFAULT_TASK_RUN_AGENT_NAME
        )
    });
    assert_eq!(cfg.agent.name, "pm");
}

/// Every embedded default agent's resolved model slug (`agent.model`, when
/// set) is either a real concrete provider slug already, or one of the three
/// short Claude aliases (`opus`/`sonnet`/`haiku`) that
/// `provider::routing::resolve_model` now normalizes to a concrete slug
/// (#3438) — never a bare alias that would reach a provider unnormalized.
///
/// Why: #3438 traced `"API error 400: \"opus is not a valid model ID\""` to a
/// roster agent's bare `model: sonnet`/`opus`/`haiku` frontmatter value (the
/// `claude` CLI's own shorthand, composed for some roster agents from
/// `resource_tier` via `trusty_agents_common::agents::builder::tier_to_model`)
/// reaching `llm::client::OpenAiCompatClient`'s wire request unnormalized.
/// The runtime fix (`provider::routing::normalize_model_alias`, applied
/// inside `resolve_model`) makes every CALL SITE safe; this test is the
/// static acceptance check that every embedded agent's declared model is
/// something that fix actually knows how to handle — an agent with a model
/// string that ISN'T one of the three known aliases and doesn't look like a
/// real `vendor/model` slug would silently mean `normalize_model_alias`
/// leaves an invalid string untouched.
/// What: composes every [`DEFAULT_AGENTS`] entry (same dual-path projection
/// as `default_agents_parse_and_names_match`), and for every agent whose
/// `agent.model` is `Some`, asserts the value is one of the three known
/// aliases OR contains a `/` (the `vendor/model` shape every real slug in
/// this codebase uses — `anthropic/claude-sonnet-4-5`, `openai/gpt-4o-mini`,
/// `bedrock/us.anthropic.claude-sonnet-4-6`, etc.). Also asserts the new
/// `pm` agent specifically resolves through
/// `provider::routing::resolve_model` to a valid concrete slug (not a bare
/// alias) end-to-end, closing the loop #3438 asked for on the agent #3437
/// adds.
/// Test: this test.
#[test]
fn every_embedded_agent_model_normalizes_to_a_valid_slug() {
    const KNOWN_ALIASES: &[&str] = &["opus", "sonnet", "haiku"];

    for agent in DEFAULT_AGENTS {
        let cfg = match agent {
            EmbeddedAgent::Direct { name, md } => project_embedded_md(name, md),
            EmbeddedAgent::Composed { name } => project_embedded_md_with_extends(name)
                .unwrap_or_else(|e| panic!("roster agent '{name}' failed to compose: {e}")),
        };
        if let Some(model) = cfg.agent.model.as_deref() {
            let is_known_alias = KNOWN_ALIASES.contains(&model.to_ascii_lowercase().as_str());
            let looks_like_a_real_slug = model.contains('/');
            assert!(
                is_known_alias || looks_like_a_real_slug,
                "embedded agent '{}' has model '{model}', which is neither a known \
                 short alias ({KNOWN_ALIASES:?}) normalize_model_alias maps, nor a \
                 vendor/model-shaped slug — it would reach a provider unnormalized",
                agent.name()
            );
        }
    }

    // The new `pm` agent (#3437) specifically: its declared `model: sonnet`
    // must resolve, end-to-end through `resolve_model`, to a concrete slug —
    // not the bare alias — closing #3438 for the exact agent #3437 adds.
    let pm_cfg = project_embedded_md("pm", PM_MD);
    let resolved = crate::provider::resolve_model(&pm_cfg, None);
    assert!(
        resolved.contains('/'),
        "pm agent's model must resolve to a concrete vendor/model slug, got '{resolved}'"
    );
    assert_ne!(
        resolved, "sonnet",
        "pm's model must not resolve to the bare alias"
    );
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

/// `EMBEDDED_TM_AGENT_SOURCES` (Slice E2, #2958) has exactly 33 entries (5
/// `BASE-*` templates + 28 roster agents), every key is unique, and every
/// entry's raw content opens with a frontmatter fence.
///
/// Why: `agents::md_loader::project_embedded_md_with_extends` builds an
/// `InMemorySources` map from this table via `build_in_memory_source_map`
/// -- a duplicate key would silently shadow one agent's real content, and a
/// wrong count would mean a roster entry was dropped or double-copied
/// during the byte-for-byte copy from trusty-mpm's bundled assets.
/// What: asserts the length, dedupes the (case-folded, per
/// `InMemorySources::insert`'s own normalisation) keys, and checks every
/// content string opens with `---`.
/// Test: this test.
#[test]
fn embedded_tm_agent_sources_has_33_entries_and_unique_keys() {
    assert_eq!(EMBEDDED_TM_AGENT_SOURCES.len(), 33);
    let mut keys: Vec<String> = EMBEDDED_TM_AGENT_SOURCES
        .iter()
        .map(|(name, _)| name.to_lowercase())
        .collect();
    let before = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), before, "no duplicate embedded tm agent keys");
    for (name, content) in EMBEDDED_TM_AGENT_SOURCES {
        assert!(
            content.trim_start().starts_with("---"),
            "embedded tm agent source '{name}' must open with a frontmatter fence"
        );
    }
}
