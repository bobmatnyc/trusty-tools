//! Unit tests for the prompt-assembly layer (parity-spec §2, §4, §6).
//!
//! Why: The parity guarantee is only as strong as the assembler's determinism
//! and section discipline; these tests pin the stable order, idempotence,
//! separator collapse, and fallback-last placement the spec mandates.
//! What: Exercises [`assemble_system_prompt`] and [`PromptAssembler`] over the
//! full matrix of present/absent sections, plus shape checks on
//! [`BASE_PREAMBLE`] and [`BASE_PREAMBLE_VERSION`].
//! Test: this file.

use std::sync::Arc;

use crate::agents::AgentConfig;
use crate::mode::HarnessMode;
use crate::prompt::{
    BASE_PREAMBLE, BASE_PREAMBLE_VERSION, BATCH_WRITE_GUIDANCE, DISCOVERY_GUIDANCE,
    FILE_DISCOVERY_GUIDANCE, GATED_SECTIONS, PromptAssembler, assemble_system_prompt,
    assemble_system_prompt_for_mode,
};
use crate::tools::{
    EditTool, FinishTaskTool, GlobTool, GrepTool, ListDirTool, ReadFileTool, ToolRegistry,
    TrustySearchTool, WriteFileTool, WriteFilesTool,
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

/// A registry shaped like the interactive PM's — harness tools only (#4602).
///
/// Why: `task::executor`'s delegating path registers `delegate_to_agent`,
/// `finish_task` and the goal tools and NOTHING that touches the filesystem;
/// `finish_task` alone reproduces the property the prompt must respect.
/// What: a `ToolRegistry` holding only `finish_task`.
/// Test: `guidance_never_names_an_unregistered_tool`.
fn pm_like_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FinishTaskTool::new()));
    reg
}

/// A registry shaped like the delegated engineer's — every gated tool.
///
/// Why: the counterpart to [`pm_like_registry`]: the agent that DOES receive
/// `glob`/`grep`/`list_dir`/`search_code`/`write_file`/`write_files` must still
/// be told about them.
/// What: a `ToolRegistry` holding every tool named in [`GATED_SECTIONS`], plus
/// `finish_task`.
/// Test: `guidance_never_names_an_unregistered_tool`.
fn engineer_like_registry() -> ToolRegistry {
    let project = std::path::Path::new(".");
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(GlobTool::new(project)));
    reg.register(Arc::new(GrepTool::new(project)));
    reg.register(Arc::new(ListDirTool::new(project)));
    reg.register(Arc::new(TrustySearchTool::new(project)));
    reg.register(Arc::new(WriteFileTool::new(project)));
    reg.register(Arc::new(WriteFilesTool::new(project)));
    reg.register(Arc::new(FinishTaskTool::new()));
    reg
}

/// Every tool name any gated section instructs a call to.
///
/// Why: reading the set from [`GATED_SECTIONS`] rather than restating it means
/// a section added to the assembler cannot escape the tests below.
/// What: flattens each entry's declared name list.
/// Test: `guidance_never_names_an_unregistered_tool`.
fn gated_tool_names() -> Vec<&'static str> {
    GATED_SECTIONS
        .iter()
        .flat_map(|(_, names, _)| names.iter().copied())
        .collect()
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

/// `BASE_PREAMBLE` requires running the project's OWN test suite before
/// finishing, not just self-authored tests (bake-off L1 diagnosis: a
/// delegated engineer's self-reported "my tests pass" previously let a
/// missing feature ship unnoticed).
///
/// Why: This is the default finish-gate strengthening — it must apply to
/// every agent (PM and delegated sub-agents alike), not just a benchmark
/// harness, so it belongs in the model-agnostic BASE preamble every prompt
/// assembles.
/// What: Asserts the preamble instructs discovering/running the project's
/// existing suite and explicitly rejects self-authored tests as a substitute.
/// Test: this test.
#[test]
fn base_preamble_requires_running_project_test_suite_before_finishing() {
    assert!(
        BASE_PREAMBLE.contains("run it, then report the")
            || BASE_PREAMBLE.to_lowercase().contains("run it"),
        "must instruct running the project's own test suite before finishing"
    );
    assert!(
        BASE_PREAMBLE.contains("not a substitute for the"),
        "must reject self-authored tests as a substitute for the project's own suite"
    );
    assert!(
        BASE_PREAMBLE.contains("Never claim tests passed without"),
        "must forbid claiming unverified test results"
    );
}

/// `BASE_PREAMBLE` tells the model that ONE clean run after the last code
/// change is sufficient and to NOT repeat identical confirmation runs over
/// unchanged code (#2682 bake-off L1 diagnosis: the engineer re-ran the full
/// suite 7-14x per run as repeated "one final run to confirm" passes,
/// inflating turn counts far above claude-code's).
///
/// Why: This is the prompt-side half of #2682 (the structural half is
/// `redundant_run`) — a general turn-efficiency nudge that applies to every
/// agent's assembled prompt, so it belongs in the model-agnostic BASE preamble
/// beside the neighbouring verification instructions.
/// What: Asserts the preamble states one clean run is sufficient and forbids a
/// redundant re-run over unchanged code.
/// Test: this test.
#[test]
fn base_preamble_discourages_redundant_test_reruns() {
    assert!(
        BASE_PREAMBLE.contains(
            "ONE clean run of the suite after your LAST code change is \
sufficient"
        ),
        "must state one clean post-change run is sufficient verification"
    );
    assert!(
        BASE_PREAMBLE.contains("do NOT re-run the same suite again"),
        "must forbid repeating an identical confirmation run over unchanged code"
    );
}

/// `BASE_PREAMBLE` nudges toward pure, unit-testable core functions with I/O
/// kept in thin wrappers (#2279 improvement 2, bake-off L2 diagnosis: tcode
/// fused `git` invocation directly into `parse_git_log`, making it untestable
/// against the visible text-fixture suite the challenge instructed it to
/// run).
///
/// Why: This is a general product-quality nudge, not a benchmark-specific
/// patch — it must apply to every agent's assembled prompt, so it belongs in
/// the model-agnostic BASE preamble like the neighbouring verification block.
/// What: Asserts the preamble names both halves of the principle: pure core
/// functions, and I/O kept separate in thin wrappers.
/// Test: this test.
#[test]
fn base_preamble_nudges_testable_design() {
    assert!(
        BASE_PREAMBLE.contains("pure, unit-testable core functions"),
        "must nudge toward pure, unit-testable core functions"
    );
    assert!(
        BASE_PREAMBLE.contains("thin wrapper functions"),
        "must instruct keeping I/O in thin wrapper functions separate from core logic"
    );
}

/// `BASE_PREAMBLE` makes eager schema initialization at application
/// construction the PRIMARY, required mechanism, and explicitly warns that a
/// startup/lifecycle hook alone is insufficient because an in-process test
/// client may construct the app without firing those hooks (#2622 bake-off L3
/// diagnosis, strengthened: the first 1.3.0 fix offered eager init only as an
/// "OR" alternative, so a generated service picked the hook-only branch and the
/// first CRUD request still hit a missing table under the bare test client).
///
/// Why: This is a general product-quality nudge for any service backed by a
/// database or persistent store, not a benchmark- or provider-specific patch —
/// it must apply to every agent's assembled prompt, so it belongs in the
/// model-agnostic BASE preamble alongside the neighbouring design nudges. The
/// eager-at-construction directive must be unambiguously primary so the model
/// cannot satisfy it with a fragile hook-only initialization.
/// What: Asserts the preamble (a) requires creating the schema as part of
/// constructing the application object before the first request, names that the
/// required/primary mechanism, and (b) warns that a startup/lifecycle hook alone
/// is not sufficient because an in-process test client may construct the app
/// without triggering those hooks.
/// Test: this test.
#[test]
fn base_preamble_requires_persistent_store_init() {
    assert!(
        BASE_PREAMBLE.contains("persistent store"),
        "must address services backed by a database or other persistent store"
    );
    assert!(
        BASE_PREAMBLE.contains("BEFORE the service handles its first request"),
        "must instruct initializing the schema before the first request"
    );
    // (a) Eager init at application construction is the PRIMARY, required
    // mechanism — not merely one alternative among several.
    assert!(
        BASE_PREAMBLE.contains("as part of constructing the application object"),
        "must require creating the schema as part of constructing the application object"
    );
    assert!(
        BASE_PREAMBLE.contains("required, primary mechanism"),
        "must name eager construction-time initialization as the required, primary mechanism"
    );
    // (b) A startup/lifecycle hook alone is insufficient because an in-process
    // test client may construct the app without firing those hooks.
    assert!(
        BASE_PREAMBLE.contains("lifecycle event hook alone is NOT sufficient"),
        "must warn that a startup or lifecycle hook alone is not sufficient"
    );
    assert!(
        BASE_PREAMBLE
            .contains("in-process test client may construct the application WITHOUT triggering"),
        "must warn that an in-process test client may construct the app without firing its hooks"
    );
}

/// `BASE_PREAMBLE` makes the task's required-artifact SET a tracked deliverable
/// rather than an epilogue (#2824).
///
/// Why: On the bake-off L4 task the engineer's completion was nondeterministic:
/// with an identical prompt and model, run-2's engineer consumed its full
/// 40-turn `InProcessRunnerConfig` budget (transcript turns 9..=48) and its
/// FINAL turn opened with "Now let me create the README and ARCHITECTURE
/// files" — so `ARCHITECTURE.md` was never written, while run-3 finished the
/// same task in 29 turns. Both runs shipped ~130 self-authored tests; the only
/// difference was that run-2 had less turn margin left when it reached the docs
/// it had queued LAST. Documentation being last in the queue makes it the only
/// deliverable exposed to turn-budget variance, so the preamble must (a) order
/// required docs ahead of discretionary self-authored tests and (b) make the
/// required set explicitly tracked.
/// What: Asserts the load-bearing phrases of the deliverable-completeness block
/// are present — the up-front checklist, the docs-are-deliverables rule, the
/// required-before-discretionary ordering, and the final checklist sweep.
/// Test: this test.
#[test]
fn base_preamble_requires_tracking_the_deliverable_set() {
    assert!(
        BASE_PREAMBLE.contains("## Deliverable completeness"),
        "BASE_PREAMBLE must carry the deliverable-completeness block (#2824)"
    );
    assert!(
        BASE_PREAMBLE.contains("enumerate that set"),
        "must instruct enumerating the required-artifact set up front"
    );
    assert!(
        BASE_PREAMBLE.contains("which items remain outstanding"),
        "must instruct tracking the outstanding items as work proceeds"
    );
    assert!(
        BASE_PREAMBLE.contains("it is not an epilogue"),
        "must state required documentation is not an epilogue"
    );
    assert!(
        BASE_PREAMBLE.contains("the design is settled: write the required documents THEN"),
        "must order required docs at the point the design settles"
    );
    assert!(
        BASE_PREAMBLE.contains("Do the REQUIRED work before any discretionary work"),
        "must order required work ahead of discretionary work"
    );
    assert!(
        BASE_PREAMBLE.contains("while a required artifact is still \nmissing from disk")
            || BASE_PREAMBLE.contains("while a required artifact is still missing from disk"),
        "must forbid starting discretionary work while a required artifact is missing"
    );
    assert!(
        BASE_PREAMBLE.contains("Scale the tests you author yourself to the requirements"),
        "must scale self-authored tests to the requirements (run-2 shipped ~130)"
    );
    assert!(
        BASE_PREAMBLE.contains("walk your checklist one final time"),
        "must require a final completeness sweep before finishing"
    );
}

/// The deliverable-completeness block points at the batch-write tool so a
/// multi-document tail costs one turn, not one per file (#2824).
///
/// Why: Run-2's engineer spent 19 of its 40 turns on single-file `write_file`
/// calls and used the `write_files` batch tool exactly ONCE, despite the
/// tool-use block already recommending batching. Repeating the pointer at the
/// point of use — the doc tail, which is where the budget actually ran out —
/// targets the specific waste the transcript shows.
/// What: Asserts the guidance names `write_files` and the one-call framing.
///
/// #4602: the bullet moved out of BASE's deliverable block into the
/// registry-gated [`BATCH_WRITE_GUIDANCE`], because a PM that holds no write
/// tool cannot obey it; the wording itself is unchanged, so this test now reads
/// the constant instead of splitting BASE.
/// Test: this test.
#[test]
fn base_preamble_batches_remaining_deliverables() {
    assert!(
        BATCH_WRITE_GUIDANCE.contains("ONE `write_files` call"),
        "the deliverable pointer must point at the batch-write tool"
    );
    assert!(
        BATCH_WRITE_GUIDANCE.contains("rather than one per turn"),
        "must contrast batching against one-file-per-turn writes"
    );
    assert!(
        BATCH_WRITE_GUIDANCE.contains("an N-file scaffold should cost ONE turn, not N"),
        "the tool-use protocol's scaffolding bullet (#2681) must survive the move"
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

/// The only spans of [`BASE_PREAMBLE`] permitted to name a tcode tool (#4602,
/// critic round 2).
///
/// Why: BASE reaches EVERY agent, so a tool name in it is only safe when the
/// sentence around it illustrates a PROTOCOL rule rather than instructing a
/// call. Pinning the exact spans — not the bare names — is what makes a NEW
/// imperative sentence naming an already-listed tool fail: the allowlist
/// excuses these two sentences, nothing else.
/// What: verbatim substrings of `BASE_PREAMBLE`; both are `e.g.` illustrations
/// of what batching means, inside the tool-use protocol.
/// Test: `base_preamble_instructs_no_registry_specific_tool`.
const ILLUSTRATION_MENTIONS: &[&str] = &[
    "(e.g. `write_file` then a `bash` that runs or verifies it",
    "e.g. you cannot `write_file` content derived from a `read_file`",
];

/// Every tool name the tcode harness can register, read from the tools
/// themselves (#4602, critic round 2).
///
/// Why: `base_preamble_instructs_no_registry_specific_tool` must close the
/// CLASS — any tool BASE could name — not just today's gated three. Deriving
/// the candidate set from each tool's own `name()` and from the exported
/// `*_TOOL_NAME` constants means a tool renamed or added in `src/tools` is
/// carried into the check without anyone editing this list.
/// What: `ToolExecutor::name()` for every cheaply-constructible tool, plus the
/// name constants of those needing a runner, resolver or session to build.
/// Test: `base_preamble_instructs_no_registry_specific_tool`.
fn known_tool_names() -> Vec<String> {
    let project = std::path::Path::new(".");
    let constructible: Vec<Box<dyn crate::tools::ToolExecutor>> = vec![
        Box::new(ReadFileTool::new(project)),
        Box::new(WriteFileTool::new(project)),
        Box::new(WriteFilesTool::new(project)),
        Box::new(EditTool::new(project)),
        Box::new(GlobTool::new(project)),
        Box::new(GrepTool::new(project)),
        Box::new(ListDirTool::new(project)),
        Box::new(TrustySearchTool::new(project)),
        Box::new(FinishTaskTool::new()),
    ];
    let mut names: Vec<String> = constructible
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    // Tools whose constructors need a runner, resolver or live session expose
    // their name as a constant instead.
    names.extend(
        [
            crate::tools::BASH_TOOL_NAME,
            crate::tools::DELEGATE_TO_AGENT_TOOL_NAME,
            crate::tools::USE_SKILL_TOOL_NAME,
            crate::tools::RECALL_SESSION_TOOL_NAME,
            crate::tools::SET_GOAL_TOOL_NAME,
            crate::tools::CLEAR_GOAL_TOOL_NAME,
            // #8235: needs a live session's checklist store to construct.
            crate::tools::TODO_WRITE_TOOL_NAME,
        ]
        .iter()
        .map(|name| (*name).to_string()),
    );
    names
}

/// Every backtick-quoted `[a-z_]+` span in `text`, in order.
///
/// Why: the two checks below both need "which tools does this prose name", and
/// a shared extractor keeps them agreeing on what counts. Non-identifier spans
/// (`**/*.py`, `src/*.rs`, `## Summary`) are not tool names and are dropped.
/// What: splits on the backtick, keeps the odd-indexed (inside) spans, filters
/// to non-empty lowercase-and-underscore tokens. Panics on unbalanced
/// backticks, which would silently invert inside and outside.
/// Test: `base_preamble_instructs_no_registry_specific_tool`,
/// `every_gated_section_declares_the_tools_it_names`.
fn backticked_identifiers(text: &str) -> Vec<&str> {
    let spans: Vec<&str> = text.split('`').collect();
    assert!(spans.len() % 2 == 1, "unbalanced backticks in: {text:?}");
    spans
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .filter(|token| {
            !token.is_empty() && token.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        })
        .collect()
}

/// `BASE_PREAMBLE` instructs no tcode tool call at all (#4602, critic round 2).
///
/// Why: BASE reaches EVERY agent, including the interactive PM whose registry
/// holds harness tools only. An INSTRUCTION to call a named tool here is one
/// some agent cannot obey — the `## File discovery` block and the two
/// batch-write bullets were exactly that, and now live in
/// [`FILE_DISCOVERY_GUIDANCE`] and [`BATCH_WRITE_GUIDANCE`], gated on the run's
/// registry. An allowlist closes the whole class: a future BASE edit naming
/// ANY tool — `edit`, `bash`, `use_skill`, `delegate_to_agent` — fails here,
/// not just the three tools gated today.
/// What: excises the [`ILLUSTRATION_MENTIONS`] spans, then asserts no
/// backtick-quoted name in what remains is a tool from [`known_tool_names`].
/// Guards against vacuity at both ends: each allowlisted span must still be
/// present and must itself name a tool.
/// Test: this test.
#[test]
fn base_preamble_instructs_no_registry_specific_tool() {
    let known = known_tool_names();
    let names_a_tool = |text: &str| {
        backticked_identifiers(text)
            .iter()
            .any(|span| known.iter().any(|name| name == span))
    };

    let mut remainder = BASE_PREAMBLE.to_string();
    for span in ILLUSTRATION_MENTIONS {
        assert!(
            remainder.contains(span),
            "allowlisted illustration span is no longer in BASE_PREAMBLE: {span:?}"
        );
        assert!(
            names_a_tool(span),
            "allowlisted span names no tool, so it excuses nothing: {span:?}"
        );
        remainder = remainder.replace(span, "");
    }

    for span in backticked_identifiers(&remainder) {
        assert!(
            !known.iter().any(|name| name == span),
            "BASE_PREAMBLE names the tool `{span}` outside ILLUSTRATION_MENTIONS. \
             Every agent reads BASE, so either move the sentence into a \
             registry-gated section (#4602) or, if it only illustrates the \
             protocol, add its exact span to ILLUSTRATION_MENTIONS"
        );
    }
}

/// Each gated section's hand-maintained name list matches its own prose
/// (#4602, critic round 1).
///
/// Why: the lists in `assembler.rs` are the ONLY input to the gate and to
/// `guidance_never_names_an_unregistered_tool`, so a tool name added to a
/// section's text but not to its list would escape both silently.
/// What: extracts every backtick-quoted `[a-z_]+` token from each gated
/// section and asserts the extracted set equals the declared list, both ways.
/// Non-identifier backtick spans (`**/*.py`, `src/*.rs`) are skipped.
/// Test: this test.
#[test]
fn every_gated_section_declares_the_tools_it_names() {
    for (section, declared, _) in GATED_SECTIONS {
        let named = backticked_identifiers(section);
        assert!(
            !named.is_empty(),
            "gated section names no tool: {section:?}"
        );

        for name in &named {
            assert!(
                declared.contains(name),
                "section names `{name}` but its list does not declare it"
            );
        }
        for name in *declared {
            assert!(
                named.contains(name),
                "list declares `{name}` but the section text never names it"
            );
        }
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

/// Expected FNV-1a-style fold of [`BASE_PREAMBLE`]'s bytes, folded with its
/// byte length. Regenerate this whenever the preamble legitimately changes
/// (see [`base_preamble_hash_tripwire`] for the contributor instructions).
// #4602: refreshed twice — the `## File discovery` block left BASE for
// `FILE_DISCOVERY_GUIDANCE`, then (critic round 1) the two batch-write bullets
// left it for `BATCH_WRITE_GUIDANCE`. `BASE_PREAMBLE_VERSION` 1.8.0 → 1.10.0
// across the same two changes.
const EXPECTED_PREAMBLE_HASH: u64 = 0xb1d0_41f0_a152_88ff;

/// Test-time guard coupling `BASE_PREAMBLE` *content* to its version constant.
///
/// Why: `BASE_PREAMBLE_VERSION` (in `prompt/version.rs`) is hand-maintained and
/// otherwise decoupled from the preamble text — a contributor can edit the
/// preamble and forget to bump the version, silently making the parity report's
/// version token (spec D5 disclosure) stale. This tripwire fails the build the
/// moment the preamble bytes change without the expected hash being refreshed,
/// forcing the version bump into the same review.
///
/// What: Folds `BASE_PREAMBLE`'s bytes with an inline FNV-1a-style hash (mixed
/// with the byte length) and asserts it equals [`EXPECTED_PREAMBLE_HASH`].
///
/// Note: This is a *tripwire*, not a security or cryptographic hash — collision
/// resistance is irrelevant; we only need a deterministic fingerprint that
/// trips on edits. We deliberately compute the fold inline (FNV-1a over the
/// bytes plus a length mix) rather than using `std::collections::hash_map::
/// DefaultHasher`, whose output is only guaranteed stable *within* a Rust
/// release; the inline fold is byte-stable across every toolchain, so the
/// stored constant never needs a cross-compiler caveat.
///
/// Test: this test (self-checking against the stored constant).
#[test]
fn base_preamble_hash_tripwire() {
    // FNV-1a 64-bit constants (public-domain reference values).
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for &byte in BASE_PREAMBLE.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // Mix the byte length so a pure-truncation edit can't preserve the hash.
    hash ^= BASE_PREAMBLE.len() as u64;
    hash = hash.wrapping_mul(FNV_PRIME);

    assert_eq!(
        hash, EXPECTED_PREAMBLE_HASH,
        "BASE_PREAMBLE changed — update EXPECTED_PREAMBLE_HASH and bump \
         BASE_PREAMBLE_VERSION in prompt/version.rs (actual hash: {hash:#018x})"
    );
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

/// With no skills catalog, #2059's mode-aware branch point must produce
/// IDENTICAL output for both modes — this is the pre-#2069 M1 contract,
/// preserved for any project with no `.claude/skills/` catalog (or when the
/// catalog happens to be empty).
///
/// Why: pins the "byte-identical when there is nothing to progressively
/// disclose" contract so a future change to the `DailyDriver` arm cannot
/// silently regress projects with no skills.
/// What: asserts `Parity` with `skills_catalog: None` returns the exact same
/// string as `assemble_system_prompt`, and `DailyDriver` differs only by the
/// [`DISCOVERY_GUIDANCE`] trailing section (PR A, #2689) the engineer's
/// registry earns (#4602) on top of the two mode-independent gated sections.
/// Test: this test.
#[test]
fn assemble_system_prompt_for_mode_identical_when_no_skills() {
    let cfg = config_with_prompt("AGENT");
    let baseline = assemble_system_prompt(&cfg, Some("PROJECT"), Some("FALLBACK"));
    // #4602: the tool-instructing sections are registry-gated, so this baseline
    // comparison uses a registry that earns them.
    let tools = engineer_like_registry();

    let parity = assemble_system_prompt_for_mode(
        HarnessMode::Parity,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        None,
        Some(&tools),
    );
    let mode_independent =
        format!("{baseline}\n\n---\n\n{FILE_DISCOVERY_GUIDANCE}\n\n---\n\n{BATCH_WRITE_GUIDANCE}");
    assert_eq!(
        parity, mode_independent,
        "Parity = baseline + the two mode-independent gated sections"
    );

    let daily = assemble_system_prompt_for_mode(
        HarnessMode::DailyDriver,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        None,
        Some(&tools),
    );
    assert_eq!(
        daily,
        format!("{mode_independent}\n\n---\n\n{DISCOVERY_GUIDANCE}"),
        "DailyDriver with no skills = Parity + the code-discovery section"
    );
}

/// A prompt never names a gated tool the run's registry does not carry — the
/// #4602 regression: the interactive PM was told to call `list_dir`/`glob` and
/// to batch into `write_files` while its registry held harness tools only, so
/// every such call came back as `ToolCallExtractError::UnknownTool`.
///
/// Why: this is the contract the fix exists to hold, checked across every
/// prompt the assembler can produce for a given registry — both modes, both
/// registry shapes — rather than at one call site.
/// What: for each (registry, mode) pair, asserts that any backtick-quoted gated
/// tool name appearing in the assembled prompt is registered in that registry.
/// `BASE_PREAMBLE` is excised first: it names `write_file` twice as an `e.g.`
/// illustration of batching rather than an instruction to call it, and it has
/// its own check in `base_preamble_instructs_no_registry_specific_tool`. The
/// final assertion rejects a vacuous pass: the engineer registry must still see
/// its tools named.
/// Test: this test.
#[test]
fn guidance_never_names_an_unregistered_tool() {
    let cfg = config_with_prompt("AGENT");
    let registries = [
        ("pm (harness tools only)", pm_like_registry()),
        ("engineer (project tools)", engineer_like_registry()),
    ];
    let mut mentions = 0usize;

    for (label, registry) in &registries {
        for mode in [HarnessMode::Parity, HarnessMode::DailyDriver] {
            let out = assemble_system_prompt_for_mode(
                mode,
                &cfg,
                Some("PROJECT"),
                Some("FALLBACK"),
                Some("Available skills:\n- demo-skill: Does demo things"),
                Some(registry),
            );
            let appended = out.replace(BASE_PREAMBLE, "");
            for name in gated_tool_names() {
                if !appended.contains(&format!("`{name}`")) {
                    continue;
                }
                mentions += 1;
                assert!(
                    registry.contains(name),
                    "{label} / {mode:?}: prompt tells the model to call `{name}`, \
                     but that tool is not in its registry"
                );
            }
        }
    }

    assert!(
        mentions > 0,
        "no prompt named any gated tool — the engineer must still be told \
         about the tools it does carry"
    );
}

/// The mode-independent gated sections follow the registry, not the mode
/// (#4602).
///
/// Why: the engineer needs file discovery AND the batch-write instructions in
/// BOTH modes (both long predate the DailyDriver split, having lived inside
/// `BASE_PREAMBLE`), and the PM needs neither.
/// What: asserts the engineer registry earns both sections under `Parity` and
/// `DailyDriver`, and the PM-shaped registry earns neither under either.
/// Test: this test.
#[test]
fn file_discovery_guidance_follows_the_registry() {
    let cfg = config_with_prompt("AGENT");
    let engineer = engineer_like_registry();
    let pm = pm_like_registry();

    for mode in [HarnessMode::Parity, HarnessMode::DailyDriver] {
        let with_tools =
            assemble_system_prompt_for_mode(mode, &cfg, None, None, None, Some(&engineer));
        let without_tools =
            assemble_system_prompt_for_mode(mode, &cfg, None, None, None, Some(&pm));

        for section in [FILE_DISCOVERY_GUIDANCE, BATCH_WRITE_GUIDANCE] {
            assert!(
                with_tools.contains(section),
                "{mode:?}: an agent holding the tools must be told about them"
            );
            assert!(
                !without_tools.contains(section),
                "{mode:?}: an agent without those tools must not be told to use them"
            );
        }
    }
}

/// The batch-write section follows the registry (#4602, critic round 1).
///
/// Why: this is the HIGH the critic found — BASE instructed every agent to
/// "use the `write_files` tool", including the delegating PM, which holds
/// neither `write_file` nor `write_files`.
/// What: asserts a PM-shaped registry earns neither the section nor the
/// `write_files` name, and that a registry holding both write tools does.
/// Test: this test.
#[test]
fn batch_write_guidance_follows_the_registry() {
    let cfg = config_with_prompt("AGENT");

    let pm = assemble_system_prompt_for_mode(
        HarnessMode::DailyDriver,
        &cfg,
        None,
        None,
        None,
        Some(&pm_like_registry()),
    );
    assert!(!pm.contains("## Batching file writes"));
    assert!(
        !pm.contains("`write_files`"),
        "the delegating PM must never be told to call `write_files`"
    );

    let engineer = assemble_system_prompt_for_mode(
        HarnessMode::DailyDriver,
        &cfg,
        None,
        None,
        None,
        Some(&engineer_like_registry()),
    );
    assert!(engineer.contains(BATCH_WRITE_GUIDANCE));
    assert!(
        engineer.contains("ONE `write_files` call"),
        "the bake-off-tuned deliverable-tail pointer (#2824) must survive the move"
    );
}

/// An unknown registry (`tools: None`) emits no gated section (#4602).
///
/// Why: "I do not know what this agent can call" must fail closed — naming a
/// tool on a guess is exactly the failure #4602 reports.
/// What: asserts no gated section appears when `tools` is `None`.
/// Test: this test.
#[test]
fn unknown_registry_omits_every_discovery_section() {
    let cfg = config_with_prompt("AGENT");
    let out =
        assemble_system_prompt_for_mode(HarnessMode::DailyDriver, &cfg, None, None, None, None);
    for (section, _, _) in GATED_SECTIONS {
        assert!(!out.contains(section));
    }
    assert!(!out.contains("## File discovery"));
    assert!(!out.contains("## Code discovery"));
}

/// `HarnessMode::DailyDriver` always appends [`DISCOVERY_GUIDANCE`] as a
/// trailing section, before any skills catalog (PR A, #2689).
///
/// Why: the engineer's `search_code` tool needs the model steered toward it;
/// the guidance is static (not project-dependent) so it is present whenever the
/// registry carries the tools it names (#4602).
/// What: asserts the guidance appears after FALLBACK and, when a catalog is
/// present, before the catalog.
/// Test: this test.
#[test]
fn daily_driver_appends_discovery_guidance() {
    let cfg = config_with_prompt("AGENT");
    let catalog = "Available skills:\n- demo-skill: Does demo things";
    let tools = engineer_like_registry();
    let out = assemble_system_prompt_for_mode(
        HarnessMode::DailyDriver,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        Some(catalog),
        Some(&tools),
    );
    assert!(out.contains(DISCOVERY_GUIDANCE));
    assert!(out.contains("search_code"));
    let guidance_at = out.find("## Code discovery").expect("guidance present");
    assert!(
        out.find("FALLBACK").unwrap() < guidance_at,
        "guidance after fallback"
    );
    assert!(
        guidance_at < out.find(catalog).unwrap(),
        "guidance before catalog"
    );
}

/// `HarnessMode::Parity` never appends the discovery guidance — the parity
/// spec forbids MCP/tool catalogs in the shared prompt.
///
/// Why: benchmark fairness (D2) requires a byte-identical prompt across models.
/// What: asserts Parity output equals the plain baseline and omits the guidance.
/// Test: this test.
#[test]
fn parity_omits_discovery_guidance() {
    let cfg = config_with_prompt("AGENT");
    let baseline = assemble_system_prompt(&cfg, Some("PROJECT"), Some("FALLBACK"));
    // #4602: a PM-shaped registry earns neither discovery section, so Parity
    // still matches the plain baseline byte-for-byte here.
    let tools = pm_like_registry();
    let out = assemble_system_prompt_for_mode(
        HarnessMode::Parity,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        Some("Available skills:\n- demo"),
        Some(&tools),
    );
    assert_eq!(out, baseline);
    assert!(!out.contains("## Code discovery"));
}

/// `HarnessMode::DailyDriver` appends a non-empty skills catalog as a
/// trailing section (#2069).
///
/// Why: This is the actual token-efficiency payoff — the DailyDriver prompt
/// gains the cheap metadata-only catalog, never a skill's full body.
/// What: Asserts the DailyDriver output contains both the baseline (BASE +
/// agent + project + fallback) and the catalog text, with the catalog
/// appearing after the baseline.
/// Test: this test.
#[test]
fn daily_driver_appends_skills_catalog() {
    let cfg = config_with_prompt("AGENT");
    let catalog = "Available skills:\n- demo-skill: Does demo things";

    let out = assemble_system_prompt_for_mode(
        HarnessMode::DailyDriver,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        Some(catalog),
        Some(&engineer_like_registry()),
    );

    assert!(out.contains("AGENT"));
    assert!(out.contains(catalog));
    assert!(
        out.find("FALLBACK").unwrap() < out.find(catalog).unwrap(),
        "the skills catalog must be the trailing section"
    );
}

/// `HarnessMode::Parity` ignores `skills_catalog` entirely, even when given
/// one (#2069's scope note: "Parity mode should NOT progressively
/// disclose").
///
/// Why: The parity spec's byte-identical-schema guarantee (D2) must never
/// depend on which project's `.claude/skills/` catalog happens to be
/// present.
/// What: Asserts `Parity` output with `Some(catalog)` equals the plain
/// `assemble_system_prompt` baseline and does not contain the catalog text.
/// Test: this test.
#[test]
fn parity_ignores_skills_catalog() {
    let cfg = config_with_prompt("AGENT");
    let baseline = assemble_system_prompt(&cfg, Some("PROJECT"), Some("FALLBACK"));
    let catalog = "Available skills:\n- demo-skill: Does demo things";

    let out = assemble_system_prompt_for_mode(
        HarnessMode::Parity,
        &cfg,
        Some("PROJECT"),
        Some("FALLBACK"),
        Some(catalog),
        // #4602: a PM-shaped registry keeps Parity equal to the plain baseline.
        Some(&pm_like_registry()),
    );

    assert_eq!(out, baseline);
    assert!(!out.contains("demo-skill"));
}
