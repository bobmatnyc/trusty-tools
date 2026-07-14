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
use crate::mode::HarnessMode;
use crate::prompt::{
    BASE_PREAMBLE, BASE_PREAMBLE_VERSION, PromptAssembler, assemble_system_prompt,
    assemble_system_prompt_for_mode,
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

/// `BASE_PREAMBLE` instructs initializing a persistent store's schema at
/// startup, before serving, and in a way that also runs under a test client
/// (#2622 bake-off L3 diagnosis: generated DB-backed services intermittently
/// omitted `create_all`/migrations, so the first CRUD request hit a missing
/// table).
///
/// Why: This is a general product-quality nudge for any service backed by a
/// database or persistent store, not a benchmark- or provider-specific patch —
/// it must apply to every agent's assembled prompt, so it belongs in the
/// model-agnostic BASE preamble alongside the neighbouring design nudges.
/// What: Asserts the preamble names both halves of the principle: initialize
/// the schema before serving the first request, and do it so it also runs
/// under a test client.
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
    assert!(
        BASE_PREAMBLE.contains("in-process test client"),
        "must ensure initialization also runs under a test client"
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

/// Expected FNV-1a-style fold of [`BASE_PREAMBLE`]'s bytes, folded with its
/// byte length. Regenerate this whenever the preamble legitimately changes
/// (see [`base_preamble_hash_tripwire`] for the contributor instructions).
const EXPECTED_PREAMBLE_HASH: u64 = 0x83d8_0652_3fc6_0053;

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
/// What: asserts `assemble_system_prompt_for_mode` with `skills_catalog:
/// None` returns the exact same string as `assemble_system_prompt` for both
/// `HarnessMode` variants.
/// Test: this test.
#[test]
fn assemble_system_prompt_for_mode_identical_when_no_skills() {
    let cfg = config_with_prompt("AGENT");
    let baseline = assemble_system_prompt(&cfg, Some("PROJECT"), Some("FALLBACK"));

    for mode in [HarnessMode::DailyDriver, HarnessMode::Parity] {
        let via_mode =
            assemble_system_prompt_for_mode(mode, &cfg, Some("PROJECT"), Some("FALLBACK"), None);
        assert_eq!(
            via_mode, baseline,
            "mode {mode:?} must match baseline when there is no skills catalog"
        );
    }
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
    );

    assert_eq!(out, baseline);
    assert!(!out.contains("demo-skill"));
}
