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

/// Every embedded agent `.md` parses (directly for the 8 tcode-native
/// `Direct` agents, via the in-memory extends composer for the 26 roster
/// agents), and its frontmatter `name:` matches the table key it is filed
/// under.
///
/// Why: A typo in either the `.md` frontmatter or the table entry would
/// silently break `agents::load_all_agents`'s embedded-fallback at runtime
/// instead of failing fast in CI. This is also the acceptance test for Slice
/// E3 (#2958): every one of the 34 entries must actually compose (a `Composed`
/// variant that panics here rather than resolving would otherwise only be
/// caught at runtime by `load_embedded_default_agents`'s log-and-skip path).
/// Test: this test.
#[test]
fn default_agents_parse_and_names_match() {
    assert_eq!(
        DEFAULT_AGENTS.len(),
        34,
        "4 originals (engineer, qa-agent, code-reviewer, pm) + 26 roster agents \
         + the 4 delivery-workflow agents (#8129, which absorbed #4027's ticketing)"
    );
    for agent in DEFAULT_AGENTS.iter() {
        let cfg = match agent {
            EmbeddedAgent::Direct { name, md } => project_embedded_md(name, md)
                .unwrap_or_else(|e| panic!("default agent '{name}' failed to load: {e}")),
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
    let engineer = project_embedded_md("engineer", ENGINEER_MD).expect("engineer loads");
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

    let qa_agent = project_embedded_md("qa-agent", QA_AGENT_MD).expect("qa-agent loads");
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

    let code_reviewer =
        project_embedded_md("code-reviewer", CODE_REVIEWER_MD).expect("code-reviewer loads");
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
/// empty, and yields the full 34-agent roster with the original 4 defaults
/// intact as the first four entries — proving Slice E3's roster expansion
/// (and #3437's `pm` addition) did not disturb the original fallback wiring
/// `.md` (#2897 Slice C) established.
///
/// Why: #2897 Slice C's non-breaking claim rests on this: a fresh project
/// with no `.claude/agents/` must still boot with `engineer`/`qa-agent`/
/// `code-reviewer`/`pm` available, exactly as it did when the defaults were
/// TOML — Slice E3 only ADDS the roster agents after them (28, plus
/// `ticketing` from #4027; #8129 made four of them tcode-native `Direct`
/// agents), never replaces or reorders the originals.
/// What: calls `crate::agents::load_all_agents` on a nonexistent directory;
/// asserts the returned names' first four entries are exactly
/// `["engineer", "qa-agent", "code-reviewer", "pm"]` and the full 34-name
/// list matches `crate::assets::DEFAULT_AGENTS`'s declared order with no
/// duplicates.
/// Test: this test.
#[test]
fn embedded_fallback_still_fires_and_yields_34_agents_with_original_4_intact() {
    let agents = crate::agents::load_all_agents(std::path::Path::new("/nonexistent/agents/dir"));
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();

    assert_eq!(
        names.len(),
        34,
        "34-agent roster: 4 originals + 26 roster + 4 delivery-workflow (#8129)"
    );
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
/// What: asserts none of the 34 `DEFAULT_AGENTS` names matches any of the 5
/// base template names (case-insensitive, since the source table keys them
/// `BASE-QA.md` while `extends:` references use `base-qa`).
/// Test: this test.
#[test]
fn base_templates_are_never_dispatchable() {
    for agent in DEFAULT_AGENTS.iter() {
        let lower = agent.name().to_ascii_lowercase();
        assert!(
            !BASE_AGENT_NAMES.contains(&lower.as_str()),
            "BASE template '{}' must never be dispatchable",
            agent.name()
        );
    }
}

/// No two entries in the 34-agent `DEFAULT_AGENTS` roster share a dispatch
/// name — in particular, trusty-mpm's own `engineer` agent (excluded from
/// the roster upstream specifically because it collides with tcode's
/// `engineer` default) does not sneak back in under any composed entry.
///
/// Why: the #2958 roster decision explicitly calls out `engineer` as
/// EXCLUDEd from the 28-agent import "(name-collides with tcode's own
/// default)" — this test is the regression pin for that exclusion, and a
/// general guard against any future roster addition silently shadowing an
/// existing dispatch name.
/// What: collects all 34 names, dedupes, asserts the length is unchanged;
/// separately asserts `"engineer"` appears exactly once.
/// Test: this test.
#[test]
fn no_name_collisions_across_the_34_agent_roster() {
    let names: Vec<&str> = DEFAULT_AGENTS.iter().map(|a| a.name()).collect();
    assert_eq!(names.len(), 34);

    let mut deduped = names.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        names.len(),
        "no name collisions across the 34-agent roster: {names:?}"
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

/// `research` is embedded straight from the shared asset crate and stays
/// unrestricted (`tools.allowed: None` — all tools allowed), per Bob's
/// explicit 2026-07-18 ruling that it builds research reports rather than
/// issuing verdicts.
///
/// Why: distinguishes "no override was accidentally added" from "an override
/// was added but with the wrong value" — the previous test only pins the
/// four restricted agents; this one pins that the agent Bob explicitly
/// exempted was NOT swept up by the same change. `documentation` shared this
/// test until #8129 replaced the shared body with a tcode-native one that
/// DOES declare `tcode_tools:`; its allowlist is pinned by
/// `delivery_workflow_agents_are_dispatchable_with_bash` instead, and that
/// is a change of agent SOURCE, not a reversal of the 2026-07-18 ruling —
/// the tcode-native body grants read/write/edit/bash, strictly more than the
/// reviewer-intent four get.
/// What: composes `research` via `project_embedded_md_with_extends` and
/// asserts `cfg.tools.allowed` is `None`.
/// Test: this test.
#[test]
fn research_remains_unrestricted() {
    let cfg = project_embedded_md_with_extends("research")
        .unwrap_or_else(|e| panic!("failed to compose 'research': {e}"));
    assert_eq!(
        cfg.tools.and_then(|t| t.allowed),
        None,
        "'research' must remain unrestricted (no tcode_tools: override)"
    );
}

/// The four #8129 delivery-workflow agents are dispatchable `Direct` roster
/// entries whose `tcode_tools:` allowlist grants `bash` and `finish_task`.
///
/// Why: this is the acceptance test for #8129. `bash` is the ONLY tool that
/// reaches `git`, `gh` and `cargo`, so an agent that lost it could not do
/// the job its body describes; `finish_task` is how any agent returns a
/// result at all. Before #8129 three of these four were `Composed` entries
/// carrying only Claude Code's `tools:` vocabulary, which #7683 makes this
/// runtime ignore — they projected to `None` (every tool allowed), the
/// opposite of an explicit grant, and `version-control` was absent entirely.
/// What: asserts each name is a `Direct` entry in `DEFAULT_AGENTS`, projects
/// it through the SAME path `load_embedded_default_agents` uses, and asserts
/// the projected allowlist is a non-empty `Some(_)` containing `bash` and
/// `finish_task`. Also pins each one's declared model, since the dispatch
/// cost of `documentation` riding on `haiku` is a deliberate choice.
/// Test: this test.
#[test]
fn delivery_workflow_agents_are_dispatchable_with_bash() {
    let expected_models = [
        ("ticketing", "sonnet"),
        ("version-control", "sonnet"),
        ("local-ops", "sonnet"),
        ("documentation", "haiku"),
    ];

    for (name, model) in expected_models {
        let md = DEFAULT_AGENTS
            .iter()
            .find_map(|a| match a {
                EmbeddedAgent::Direct { name: n, md } if *n == name => Some(*md),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("'{name}' must be a tcode-native Direct entry in DEFAULT_AGENTS")
            });

        let cfg = project_embedded_md(name, md)
            .unwrap_or_else(|e| panic!("delivery-workflow agent '{name}' failed to load: {e}"));

        assert_eq!(
            cfg.agent.model.as_deref(),
            Some(model),
            "'{name}' must declare model: {model}"
        );

        let allowed = cfg
            .tools
            .and_then(|t| t.allowed)
            .unwrap_or_else(|| panic!("'{name}' must declare an explicit tcode_tools allowlist"));
        assert!(
            allowed.iter().any(|t| t == "bash"),
            "'{name}' must be able to run git/gh/cargo: {allowed:?}"
        );
        assert!(
            allowed.iter().any(|t| t == "finish_task"),
            "'{name}' must be able to return a result: {allowed:?}"
        );
    }
}

/// `ticketing` and `version-control` each instruct the agent to report its
/// artifact URL on a line prefixed `ISSUE:` / `PR:`.
///
/// Why: the PM's #8129 delegation guidance tells it to carry those URLs
/// forward into the next brief. That contract lives in two places — the
/// producing agent's body and `pm.md`'s instruction to parse it — and a
/// silent edit to either half leaves the PM parsing for a prefix no agent
/// emits.
/// What: asserts the literal prefix appears in each producing agent's body
/// and that `pm.md` names both.
/// Test: this test.
#[test]
fn workflow_agents_declare_the_prefixes_the_pm_parses() {
    assert!(
        TICKETING_MD.contains("ISSUE:"),
        "ticketing must report its issue URL on an `ISSUE:` line"
    );
    assert!(
        VERSION_CONTROL_MD.contains("PR:"),
        "version-control must report its PR URL on a `PR:` line"
    );
    for token in ["ticketing", "version-control", "local-ops", "documentation"] {
        assert!(
            pm_card().contains(token),
            "pm.md's delegation guidance must name '{token}'"
        );
    }
    assert!(
        pm_card().contains("ISSUE:") && pm_card().contains("PR:"),
        "pm.md must name both parseable result prefixes"
    );
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

    for agent in DEFAULT_AGENTS.iter() {
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

    for agent in DEFAULT_AGENTS.iter() {
        let cfg = match agent {
            EmbeddedAgent::Direct { name, md } => project_embedded_md(name, md)
                .unwrap_or_else(|e| panic!("default agent '{name}' failed to load: {e}")),
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
    let pm_cfg = project_embedded_md("pm", pm_card()).expect("pm loads");
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

/// `EMBEDDED_TM_AGENT_SOURCES` (Slice E2, #2958) has exactly 31 entries (5
/// `BASE-*` templates + 26 roster agents — #8129 removed `ticketing.md`,
/// `local-ops.md` and `documentation.md`, whose dispatch names tcode-native
/// `Direct` agents took over), every key is unique, and every
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
fn embedded_tm_agent_sources_has_31_entries_and_unique_keys() {
    assert_eq!(EMBEDDED_TM_AGENT_SOURCES.len(), 31);
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

/// #4027 (epic #4021): the ported `ticketing` agent is a real, dispatchable
/// roster entry that resolves through the SAME resolver `tcode run-task
/// <AGENT>` uses — the reachability guarantee trusty-agents' widened
/// cross-product bridge (#4026) depends on.
///
/// Why: the bridge dispatches `run-task ticketing <task>`; if the name did not
/// resolve here, that call would fail at the far end with an unresolved-agent
/// error the bridge cannot distinguish from a real task failure. Asserting
/// resolution through `resolve_agent` (not just table membership) is what makes
/// this an end-to-end reachability pin rather than a table-shape assertion.
/// What: resolves `ticketing` against an EMPTY disk dir so the embedded tier is
/// exercised, and asserts the composed config carries the ticketing persona's
/// own identity (not a fallback to `pm` and not a `NotFound`).
/// Test: this test.
#[test]
fn ticketing_is_dispatchable_for_cross_product_delegation() {
    assert!(
        DEFAULT_AGENTS.iter().any(|a| a.name() == "ticketing"),
        "ticketing must be a dispatchable roster entry"
    );

    let cfg =
        crate::agents::resolve_agent(std::path::Path::new("/nonexistent/agents/dir"), "ticketing")
            .expect("ticketing must resolve via the embedded roster");
    assert_eq!(cfg.agent.name, "ticketing");
    assert_ne!(cfg.agent.name, "pm", "must not silently fall back to pm");
}

/// #8129: no delivery-workflow dispatch name is ALSO resolvable from
/// [`EMBEDDED_TM_AGENT_SOURCES`], so there is exactly one body behind each.
///
/// Why: #8129 replaced three shared bodies with tcode-native ones under the
/// same dispatch names. Leaving the shared entry in the source table would
/// make `project_embedded_md_with_extends("ticketing")` and
/// `project_embedded_md("ticketing", TICKETING_MD)` return DIFFERENT prompts
/// and different tool grants for the same name — which one a caller got
/// would depend on which projection path it happened to take. This replaces
/// #4027's `ticketing_copy_carries_no_tcode_only_tools_restriction`, whose
/// premise (ticketing is a byte-parity copy of the shared asset) #8129
/// retired.
/// What: asserts each of the four names is absent from the source table and
/// present as an `EmbeddedAgent::Direct` entry.
/// Test: this test.
#[test]
fn delivery_workflow_names_resolve_to_exactly_one_body() {
    for name in ["ticketing", "version-control", "local-ops", "documentation"] {
        let key = format!("{name}.md");
        assert!(
            !EMBEDDED_TM_AGENT_SOURCES
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case(&key)),
            "'{key}' must not stay in the shared source table — a tcode-native \
             Direct agent owns that dispatch name"
        );
        assert!(
            DEFAULT_AGENTS
                .iter()
                .any(|a| matches!(a, EmbeddedAgent::Direct { name: n, .. } if *n == name)),
            "'{name}' must be a Direct roster entry"
        );
    }
}

// -- #8287 / #8227: PM routing table and supporting-agent delegability. --

/// A directory path no `.claude/agents/` tier occupies, so
/// [`crate::agents::resolve_agent`] always falls through to the embedded roster.
///
/// Why: `resolve_agent` is disk-first. Pointing it at a real directory would
/// make these tests depend on whatever the developer's checkout happens to hold;
/// pointing it at a path that cannot exist exercises exactly the tier
/// `delegate_to_agent` reaches for a stock session.
/// What: an absolute path under a name no filesystem root uses.
/// Test: used by the tests below.
const NO_DISK_AGENTS_DIR: &str = "/tcode-tests-no-such-agents-dir";

/// Resolve `name` through the same path `delegate_to_agent` uses, or panic.
///
/// Why: a routing target that `resolve_agent` cannot return is not delegable,
/// whatever `DEFAULT_AGENTS` says — the roster table and the resolver are two
/// steps, and only the second one a delegation actually takes.
/// What: calls [`crate::agents::resolve_agent`] against [`NO_DISK_AGENTS_DIR`].
/// Test: used by the tests below.
fn resolve_embedded(name: &str) -> crate::agents::AgentConfig {
    crate::agents::resolve_agent(std::path::Path::new(NO_DISK_AGENTS_DIR), name)
        .unwrap_or_else(|e| panic!("'{name}' is not delegable — resolve_agent failed: {e}"))
}

/// Every backtick-quoted agent-shaped token in `text`, in order.
///
/// Why: the routing block's whole content is agent names, and the test has to
/// range over what the CARD actually says rather than a list transcribed beside
/// it — a name added to the table but not to the roster must fail here.
/// What: keeps the odd-indexed (inside-backtick) spans whose every character is
/// a lowercase ASCII letter or `-`. No roster name carries an underscore, so a
/// tool name (`finish_task`) is excluded by shape, and `ISSUE:`/`PR:` by case.
/// Panics on unbalanced backticks, which would invert inside and outside.
/// Test: `pm_routing_block_names_only_delegable_roster_agents`.
fn backticked_agent_tokens(text: &str) -> Vec<&str> {
    let spans: Vec<&str> = text.split('`').collect();
    assert!(spans.len() % 2 == 1, "unbalanced backticks in: {text:?}");
    spans
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .filter(|token| {
            !token.is_empty() && token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        })
        .collect()
}

/// The raw `.md` source behind a roster dispatch name.
///
/// Why: the #8227 test compares each agent's PROJECTED grant against what its
/// own card declares, so it needs the card bytes — which live in two different
/// tables depending on whether the agent is `Direct` or `Composed`.
/// What: the `Direct` entry's `md` when one exists, else the
/// [`EMBEDDED_TM_AGENT_SOURCES`] entry keyed `<name>.md`.
/// Test: `supporting_agents_resolve_with_their_declared_tool_grant`.
fn card_source(name: &str) -> &'static str {
    DEFAULT_AGENTS
        .iter()
        .find_map(|a| match a {
            EmbeddedAgent::Direct { name: n, md } if *n == name => Some(*md),
            _ => None,
        })
        .or_else(|| {
            let key = format!("{name}.md");
            EMBEDDED_TM_AGENT_SOURCES
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                .map(|(_, md)| *md)
        })
        .unwrap_or_else(|| panic!("no embedded card source for '{name}'"))
}

/// A scalar frontmatter value from a raw agent card, trimmed.
///
/// Why/What: scans the card's leading `---` fence for `<key>:` and returns the
/// remainder of that line. `None` when the key is absent, which for
/// `tcode_tools:` is itself the declared grant (every tool allowed).
/// Test: `supporting_agents_resolve_with_their_declared_tool_grant`.
fn frontmatter_value(md: &str, key: &str) -> Option<String> {
    let fenced = md.strip_prefix("---\n")?.split_once("\n---")?.0;
    fenced
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}:")))
        .map(|value| value.trim().to_string())
}

/// The tool allowlist a card declares, as [`crate::agents::AgentConfig`] would
/// hold it.
///
/// Why/What: parses `tcode_tools: [a, b, c]` into the same `Option<Vec<String>>`
/// shape `cfg.tools.allowed` carries — `None` when the card declares no
/// `tcode_tools:` line at all.
/// Test: `supporting_agents_resolve_with_their_declared_tool_grant`.
fn declared_tool_grant(md: &str) -> Option<Vec<String>> {
    let raw = frontmatter_value(md, "tcode_tools")?;
    let inner = raw.trim().trim_start_matches('[').trim_end_matches(']');
    Some(
        inner
            .split(',')
            .map(|tool| tool.trim().to_string())
            .filter(|tool| !tool.is_empty())
            .collect(),
    )
}

/// `pm.md`'s routing block names `research` -> `engineer` -> `qa-agent` in that
/// order, and every agent it names is delegable (#8287).
///
/// Why: this is #8287's acceptance test. A card that merely CONTAINS the word
/// "research" proves nothing — the defect #8287 describes is that the PM has no
/// basis to dispatch an agent, which needs the name to be (a) present as a
/// routing target inside the routing block, (b) in the order DOC-75 §1 fixes,
/// and (c) resolvable by the same `resolve_agent` call `delegate_to_agent`
/// makes, with a real prompt and a way to report back. Each of those three is a
/// separate way to be wrong, so each is asserted separately. The `bash`
/// assertion on the verification target is what pins `qa-agent` over `qa`:
/// DOC-75 §6 requires real test output in the transcript, and tcode's `qa` fork
/// carries no `bash`.
/// What: extracts the block from the RENDERED card ([`pm_card`]) via
/// [`pm_routing_block`], asserts every
/// backticked agent-shaped token in it resolves and is delegable, asserts
/// [`PM_ROUTING_ORDER`]'s three names appear in that relative order, and asserts
/// the third one can run commands.
/// Test: this test.
#[test]
fn pm_routing_block_names_only_delegable_roster_agents() {
    let block = pm_routing_block(pm_card()).unwrap_or_else(|| {
        panic!(
            "pm.md carries no routing block — #8287 requires one delimited by \
             {PM_ROUTING_BLOCK_BEGIN:?} .. {PM_ROUTING_BLOCK_END:?}"
        )
    });

    let named = backticked_agent_tokens(block);
    assert!(
        named.len() >= PM_ROUTING_ORDER.len(),
        "the routing block names too few agents to route a coding task: {named:?}"
    );
    for name in &named {
        assert!(
            DEFAULT_AGENTS.iter().any(|a| a.name() == *name),
            "the routing block names '{name}', which is not in DEFAULT_AGENTS — \
             the PM would emit a delegation that fails agent resolution"
        );
        let cfg = resolve_embedded(name);
        assert!(
            !cfg.system_prompt.content.trim().is_empty(),
            "'{name}' resolves to an empty prompt, so delegating to it is a no-op"
        );
        let allowed = cfg.tools.and_then(|t| t.allowed);
        assert!(
            allowed.as_ref().is_none_or(|tools| tools
                .iter()
                .any(|t| t == crate::tools::FINISH_TASK_TOOL_NAME)),
            "'{name}' cannot call finish_task, so it can never report a result back"
        );
    }

    let position = |target: &str| {
        named
            .iter()
            .position(|name| *name == target)
            .unwrap_or_else(|| {
                panic!("the routing block names no '{target}' delegation target: {named:?}")
            })
    };
    let mut previous = None;
    for target in PM_ROUTING_ORDER {
        let at = position(target);
        if let Some((earlier_name, earlier_at)) = previous {
            assert!(
                earlier_at < at,
                "the routing block must name '{earlier_name}' before '{target}' — \
                 DOC-75 §1 fixes the order research -> engineer -> qa"
            );
        }
        previous = Some((*target, at));
    }

    let verifier = PM_ROUTING_ORDER
        .last()
        .expect("PM_ROUTING_ORDER is never empty");
    let verifier_tools = resolve_embedded(verifier)
        .tools
        .and_then(|t| t.allowed)
        .unwrap_or_else(|| panic!("'{verifier}' declares no explicit tool grant"));
    assert!(
        verifier_tools
            .iter()
            .any(|t| t == crate::tools::BASH_TOOL_NAME),
        "the verification target must be able to RUN the tests DOC-75 §6 wants \
         quoted, so it needs bash: {verifier_tools:?}"
    );
}

/// The card's routing block is RENDERED from the shared routing rows (#8293).
///
/// Why: this is #8293's drift check for trusty-code. A card that went back to
/// authoring its own table would still pass every #8287 test — those only read
/// what the card says — so the property that actually has to hold is that the
/// delivered text is the shared rows' output. Asserting the template still
/// carries the placeholders is the other half: without them `fill` is a no-op
/// and the block would silently ship empty.
/// What: asserts [`PM_CARD_TEMPLATE`] carries both placeholders, that the
/// rendered card carries neither, and that the routing block of the rendered
/// card contains exactly what `pm_routing::render_table`/`render_pipeline`
/// produce for [`Consumer::Tcode`].
/// Test: this test.
#[test]
fn pm_card_routing_block_is_rendered_from_the_shared_rows() {
    use trusty_agents_common::pm_routing::{
        Consumer, PIPELINE_PLACEHOLDER, TABLE_PLACEHOLDER, render_pipeline, render_table,
    };

    for placeholder in [TABLE_PLACEHOLDER, PIPELINE_PLACEHOLDER] {
        assert!(
            PM_CARD_TEMPLATE.contains(placeholder),
            "pm.md must carry {placeholder:?} — without it the routing block is \
             authored here again instead of rendered from the shared rows (#8293)"
        );
        assert!(
            !pm_card().contains(placeholder),
            "the rendered card still carries {placeholder:?}, so `fill` did not run"
        );
    }

    let block = pm_routing_block(pm_card())
        .unwrap_or_else(|| panic!("the rendered card carries no routing block"));
    let table = render_table(Consumer::Tcode);
    assert!(
        block.contains(&table),
        "the card's routing block has drifted from the shared rows.\n\
         expected to contain:\n{table}\n\nblock:\n{block}"
    );
    let chain = render_pipeline(Consumer::Tcode);
    assert!(
        block.contains(&chain),
        "the card's pipeline sentence has drifted from the shared rows: \
         expected {chain:?} in:\n{block}"
    );
}

/// Every agent the shared rows route trusty-code's work to is delegable (#8293).
///
/// Why: the rows are edited in a crate that cannot see tcode's roster, so a row
/// naming an agent trusty-code does not bundle would ship a PM that emits a
/// delegation failing agent resolution (#4594). This check is what makes the
/// shared source safe to edit from the other product's side.
/// What: for every name in `pm_routing::agents(Consumer::Tcode)`, asserts a
/// [`DEFAULT_AGENTS`] entry exists and [`crate::agents::resolve_agent`] returns
/// it with a non-empty prompt.
/// Test: this test.
#[test]
fn shared_routing_rows_name_only_delegable_tcode_agents() {
    let named =
        trusty_agents_common::pm_routing::agents(trusty_agents_common::pm_routing::Consumer::Tcode);
    assert!(
        named.len() >= PM_ROUTING_ORDER.len(),
        "the shared rows route too few agents to run a coding task: {named:?}"
    );
    for name in named {
        assert!(
            DEFAULT_AGENTS.iter().any(|a| a.name() == name),
            "the shared routing rows name '{name}', which is not in DEFAULT_AGENTS"
        );
        assert!(
            !resolve_embedded(name)
                .system_prompt
                .content
                .trim()
                .is_empty(),
            "'{name}' resolves to an empty prompt, so delegating to it is a no-op"
        );
    }
}

/// The PM card stays under DOC-75 §4b's 2x token-budget cap (#8287).
///
/// Why: token budget is a first-class axis for tcode (vision spec), and this
/// card is resident in every delegate-mode turn. #8293 and #8294 both add to it
/// next, so the cap needs a mechanical floor now rather than after the third
/// edit.
/// What: asserts the RENDERED card's length — what a session actually
/// receives, never the shorter authored template — is at most twice
/// [`PM_CARD_BASELINE_BYTES`].
/// Test: this test.
#[test]
fn pm_card_stays_within_the_doc_75_size_cap() {
    let cap = PM_CARD_BASELINE_BYTES * 2;
    assert!(
        pm_card().len() <= cap,
        "pm.md is {} bytes, over DOC-75 §4b's cap of {cap} (2x the \
         {PM_CARD_BASELINE_BYTES}-byte 2026-09-19 baseline)",
        pm_card().len()
    );
}

/// The PM card instructs no tool a delegate-mode PM registry lacks, and names
/// no harness tcode does not ship (#8287).
///
/// Why: the #4602 class of defect — a prompt naming a tool the run's registry
/// never registered, so the model emits a call that fails validation. #4602
/// closed it for the assembler's own gated sections; the PM card itself was
/// never checked, and the routing block is new prose that could reintroduce it.
/// The `tmux`/`Skill(` checks encode DOC-75 §4b's other half: tcode hosts
/// neither, so wording borrowed from trusty-mpm's PM instructions is a defect
/// here even though it is correct there.
/// What: asserts every backticked `[a-z_]+` token in the card BODY is one of the
/// six tools `task::executor`'s delegating path registers
/// (`executor.rs`'s `pm_registry`, where `pm_prompt_tools` returns `None`), and
/// that the card contains neither `tmux` nor a `Skill(` invocation.
/// Test: this test.
#[test]
fn pm_card_names_no_tool_the_delegate_mode_pm_lacks() {
    let body = resolve_embedded("pm").system_prompt.content;
    let delegate_mode_tools = [
        crate::tools::DELEGATE_TO_AGENT_TOOL_NAME,
        crate::tools::FINISH_TASK_TOOL_NAME,
        crate::tools::SET_GOAL_TOOL_NAME,
        crate::tools::CLEAR_GOAL_TOOL_NAME,
        crate::tools::USE_SKILL_TOOL_NAME,
        crate::tools::RECALL_SESSION_TOOL_NAME,
    ];

    let spans: Vec<&str> = body.split('`').collect();
    assert!(spans.len() % 2 == 1, "unbalanced backticks in pm.md");
    let mut checked = 0usize;
    for token in spans.iter().skip(1).step_by(2) {
        if token.is_empty() || !token.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        if !token.contains('_') {
            continue; // an agent name, checked by the routing test above
        }
        checked += 1;
        assert!(
            delegate_mode_tools.contains(token),
            "pm.md names `{token}`, which a delegate-mode PM registry does not \
             hold — the model would emit a call that fails validation (#4602)"
        );
    }
    assert!(
        checked > 0,
        "pm.md names no tool at all, so this check proves nothing"
    );
    for absent in ["tmux", "Skill("] {
        assert!(
            !body.contains(absent),
            "pm.md names {absent:?}, which tcode does not ship (DOC-75 §4b)"
        );
    }
}

/// Each of #8227's four supporting agents resolves, carries a real prompt with
/// its card's role, and projects exactly the tool grant its card declares.
///
/// Why: this is #8227's deterministic half. #8129 verified roster PRESENCE only;
/// what the PM's routing table now depends on is that each name resolves through
/// `resolve_agent` and arrives with the grant its author wrote — the #8199 class
/// of defect is precisely a frontmatter key silently dropped on the way through,
/// leaving an agent with a grant nobody chose. Comparing against the card's OWN
/// frontmatter rather than a transcribed list is what makes an edit to either
/// side fail here.
/// What: for `research`, `documentation`, `ticketing` and `version-control`:
/// resolves each, asserts a non-empty prompt, asserts `cfg.agent.role` equals
/// the card's `role:` line, and asserts `cfg.tools.allowed` equals the card's
/// `tcode_tools:` list (or `None` where the card declares none — `research`,
/// whose unrestricted grant is the 2026-07-18 owner ruling).
/// Test: this test.
#[test]
fn supporting_agents_resolve_with_their_declared_tool_grant() {
    for name in ["research", "documentation", "ticketing", "version-control"] {
        let card = card_source(name);
        let cfg = resolve_embedded(name);

        assert!(
            !cfg.system_prompt.content.trim().is_empty(),
            "'{name}' resolves to an empty prompt"
        );
        assert_eq!(
            cfg.agent.role,
            frontmatter_value(card, "role"),
            "'{name}' must arrive carrying the role its card declares"
        );
        assert_eq!(
            cfg.tools.and_then(|t| t.allowed),
            declared_tool_grant(card),
            "'{name}' projects a tool grant its card did not declare (#8199)"
        );
    }
}
