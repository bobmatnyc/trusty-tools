//! Skill-port batch 1 (issue #2903) — debugging & testing universal skills.
//!
//! Why: `docs/specs/` epic #2902 ports the 26-skill upstream `universal/`
//! catalog (`bobmatnyc/claude-mpm-skills`) into bundled trusty-mpm skills so
//! agents that declare them in `skills:` frontmatter (DOC-42, #2889) resolve
//! against real content instead of a dangling reference. This module holds
//! the debugging/testing group (systematic-debugging, verification-before-
//! completion, root-cause-tracing, test-driven-development, condition-based-
//! waiting, test-quality-inspector, testing-anti-patterns, webapp-testing) —
//! split from the other batch-1 groups to keep every file under the 500-SLOC
//! production cap despite the const count (comments do not count toward SLOC,
//! but many `pub const` lines still add up across 25 skills + references).
//! What: `pub const` strings for each skill's `SKILL.md` entry point and, for
//! multi-file skills, each `references/*.md` file, embedded at compile time
//! via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `skill_port_batch1_skills_are_in_bundle`.

/// `systematic-debugging` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/systematic-debugging/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/systematic-debugging.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const SYSTEMATIC_DEBUGGING: &str = include_str!("../assets/skills/systematic-debugging.md");

/// `systematic-debugging` reference file `references/anti-patterns.md` (issue #2903).
///
/// Why: upstream `systematic-debugging` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `systematic-debugging`'s
/// `SKILL.md` at `skills/systematic-debugging/references/anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SYSTEMATIC_DEBUGGING_ANTI_PATTERNS: &str =
    include_str!("../assets/skills/systematic-debugging/references/anti-patterns.md");

/// `systematic-debugging` reference file `references/examples.md` (issue #2903).
///
/// Why: upstream `systematic-debugging` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `systematic-debugging`'s
/// `SKILL.md` at `skills/systematic-debugging/references/examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SYSTEMATIC_DEBUGGING_EXAMPLES: &str =
    include_str!("../assets/skills/systematic-debugging/references/examples.md");

/// `systematic-debugging` reference file `references/troubleshooting.md` (issue #2903).
///
/// Why: upstream `systematic-debugging` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `systematic-debugging`'s
/// `SKILL.md` at `skills/systematic-debugging/references/troubleshooting.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SYSTEMATIC_DEBUGGING_TROUBLESHOOTING: &str =
    include_str!("../assets/skills/systematic-debugging/references/troubleshooting.md");

/// `systematic-debugging` reference file `references/workflow.md` (issue #2903).
///
/// Why: upstream `systematic-debugging` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `systematic-debugging`'s
/// `SKILL.md` at `skills/systematic-debugging/references/workflow.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SYSTEMATIC_DEBUGGING_WORKFLOW: &str =
    include_str!("../assets/skills/systematic-debugging/references/workflow.md");

/// `verification-before-completion` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/verification-before-completion/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/verification-before-completion.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const VERIFICATION_BEFORE_COMPLETION: &str =
    include_str!("../assets/skills/verification-before-completion.md");

/// `verification-before-completion` reference file `references/gate-function.md` (issue #2903).
///
/// Why: upstream `verification-before-completion` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `verification-before-completion`'s
/// `SKILL.md` at `skills/verification-before-completion/references/gate-function.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const VERIFICATION_BEFORE_COMPLETION_GATE_FUNCTION: &str =
    include_str!("../assets/skills/verification-before-completion/references/gate-function.md");

/// `verification-before-completion` reference file `references/integration-and-workflows.md` (issue #2903).
///
/// Why: upstream `verification-before-completion` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `verification-before-completion`'s
/// `SKILL.md` at `skills/verification-before-completion/references/integration-and-workflows.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const VERIFICATION_BEFORE_COMPLETION_INTEGRATION_AND_WORKFLOWS: &str = include_str!(
    "../assets/skills/verification-before-completion/references/integration-and-workflows.md"
);

/// `verification-before-completion` reference file `references/red-flags-and-failures.md` (issue #2903).
///
/// Why: upstream `verification-before-completion` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `verification-before-completion`'s
/// `SKILL.md` at `skills/verification-before-completion/references/red-flags-and-failures.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const VERIFICATION_BEFORE_COMPLETION_RED_FLAGS_AND_FAILURES: &str = include_str!(
    "../assets/skills/verification-before-completion/references/red-flags-and-failures.md"
);

/// `verification-before-completion` reference file `references/verification-patterns.md` (issue #2903).
///
/// Why: upstream `verification-before-completion` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `verification-before-completion`'s
/// `SKILL.md` at `skills/verification-before-completion/references/verification-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const VERIFICATION_BEFORE_COMPLETION_VERIFICATION_PATTERNS: &str = include_str!(
    "../assets/skills/verification-before-completion/references/verification-patterns.md"
);

/// `root-cause-tracing` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/root-cause-tracing/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/root-cause-tracing.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const ROOT_CAUSE_TRACING: &str = include_str!("../assets/skills/root-cause-tracing.md");

/// `root-cause-tracing` reference file `references/advanced-techniques.md` (issue #2903).
///
/// Why: upstream `root-cause-tracing` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `root-cause-tracing`'s
/// `SKILL.md` at `skills/root-cause-tracing/references/advanced-techniques.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ROOT_CAUSE_TRACING_ADVANCED_TECHNIQUES: &str =
    include_str!("../assets/skills/root-cause-tracing/references/advanced-techniques.md");

/// `root-cause-tracing` reference file `references/examples.md` (issue #2903).
///
/// Why: upstream `root-cause-tracing` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `root-cause-tracing`'s
/// `SKILL.md` at `skills/root-cause-tracing/references/examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ROOT_CAUSE_TRACING_EXAMPLES: &str =
    include_str!("../assets/skills/root-cause-tracing/references/examples.md");

/// `root-cause-tracing` reference file `references/integration.md` (issue #2903).
///
/// Why: upstream `root-cause-tracing` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `root-cause-tracing`'s
/// `SKILL.md` at `skills/root-cause-tracing/references/integration.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ROOT_CAUSE_TRACING_INTEGRATION: &str =
    include_str!("../assets/skills/root-cause-tracing/references/integration.md");

/// `root-cause-tracing` reference file `references/tracing-techniques.md` (issue #2903).
///
/// Why: upstream `root-cause-tracing` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `root-cause-tracing`'s
/// `SKILL.md` at `skills/root-cause-tracing/references/tracing-techniques.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ROOT_CAUSE_TRACING_TRACING_TECHNIQUES: &str =
    include_str!("../assets/skills/root-cause-tracing/references/tracing-techniques.md");

/// `test-driven-development` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/test-driven-development/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/test-driven-development.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const TEST_DRIVEN_DEVELOPMENT: &str =
    include_str!("../assets/skills/test-driven-development.md");

/// `test-driven-development` reference file `references/anti-patterns.md` (issue #2903).
///
/// Why: upstream `test-driven-development` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-driven-development`'s
/// `SKILL.md` at `skills/test-driven-development/references/anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_DRIVEN_DEVELOPMENT_ANTI_PATTERNS: &str =
    include_str!("../assets/skills/test-driven-development/references/anti-patterns.md");

/// `test-driven-development` reference file `references/examples.md` (issue #2903).
///
/// Why: upstream `test-driven-development` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-driven-development`'s
/// `SKILL.md` at `skills/test-driven-development/references/examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_DRIVEN_DEVELOPMENT_EXAMPLES: &str =
    include_str!("../assets/skills/test-driven-development/references/examples.md");

/// `test-driven-development` reference file `references/integration.md` (issue #2903).
///
/// Why: upstream `test-driven-development` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-driven-development`'s
/// `SKILL.md` at `skills/test-driven-development/references/integration.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_DRIVEN_DEVELOPMENT_INTEGRATION: &str =
    include_str!("../assets/skills/test-driven-development/references/integration.md");

/// `test-driven-development` reference file `references/philosophy.md` (issue #2903).
///
/// Why: upstream `test-driven-development` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-driven-development`'s
/// `SKILL.md` at `skills/test-driven-development/references/philosophy.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_DRIVEN_DEVELOPMENT_PHILOSOPHY: &str =
    include_str!("../assets/skills/test-driven-development/references/philosophy.md");

/// `test-driven-development` reference file `references/workflow.md` (issue #2903).
///
/// Why: upstream `test-driven-development` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-driven-development`'s
/// `SKILL.md` at `skills/test-driven-development/references/workflow.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_DRIVEN_DEVELOPMENT_WORKFLOW: &str =
    include_str!("../assets/skills/test-driven-development/references/workflow.md");

/// `condition-based-waiting` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/condition-based-waiting/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/condition-based-waiting.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const CONDITION_BASED_WAITING: &str =
    include_str!("../assets/skills/condition-based-waiting.md");

/// `condition-based-waiting` reference file `references/patterns-and-implementation.md` (issue #2903).
///
/// Why: upstream `condition-based-waiting` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `condition-based-waiting`'s
/// `SKILL.md` at `skills/condition-based-waiting/references/patterns-and-implementation.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CONDITION_BASED_WAITING_PATTERNS_AND_IMPLEMENTATION: &str = include_str!(
    "../assets/skills/condition-based-waiting/references/patterns-and-implementation.md"
);

/// `test-quality-inspector` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/test-quality-inspector/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/test-quality-inspector.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const TEST_QUALITY_INSPECTOR: &str = include_str!("../assets/skills/test-quality-inspector.md");

/// `test-quality-inspector` reference file `references/assertion-quality.md` (issue #2903).
///
/// Why: upstream `test-quality-inspector` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-quality-inspector`'s
/// `SKILL.md` at `skills/test-quality-inspector/references/assertion-quality.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_QUALITY_INSPECTOR_ASSERTION_QUALITY: &str =
    include_str!("../assets/skills/test-quality-inspector/references/assertion-quality.md");

/// `test-quality-inspector` reference file `references/inspection-checklist.md` (issue #2903).
///
/// Why: upstream `test-quality-inspector` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-quality-inspector`'s
/// `SKILL.md` at `skills/test-quality-inspector/references/inspection-checklist.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_QUALITY_INSPECTOR_INSPECTION_CHECKLIST: &str =
    include_str!("../assets/skills/test-quality-inspector/references/inspection-checklist.md");

/// `test-quality-inspector` reference file `references/red-flags.md` (issue #2903).
///
/// Why: upstream `test-quality-inspector` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `test-quality-inspector`'s
/// `SKILL.md` at `skills/test-quality-inspector/references/red-flags.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TEST_QUALITY_INSPECTOR_RED_FLAGS: &str =
    include_str!("../assets/skills/test-quality-inspector/references/red-flags.md");

/// `testing-anti-patterns` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/testing-anti-patterns/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/testing-anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const TESTING_ANTI_PATTERNS: &str = include_str!("../assets/skills/testing-anti-patterns.md");

/// `testing-anti-patterns` reference file `references/completeness-anti-patterns.md` (issue #2903).
///
/// Why: upstream `testing-anti-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `testing-anti-patterns`'s
/// `SKILL.md` at `skills/testing-anti-patterns/references/completeness-anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TESTING_ANTI_PATTERNS_COMPLETENESS_ANTI_PATTERNS: &str =
    include_str!("../assets/skills/testing-anti-patterns/references/completeness-anti-patterns.md");

/// `testing-anti-patterns` reference file `references/core-anti-patterns.md` (issue #2903).
///
/// Why: upstream `testing-anti-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `testing-anti-patterns`'s
/// `SKILL.md` at `skills/testing-anti-patterns/references/core-anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TESTING_ANTI_PATTERNS_CORE_ANTI_PATTERNS: &str =
    include_str!("../assets/skills/testing-anti-patterns/references/core-anti-patterns.md");

/// `testing-anti-patterns` reference file `references/detection-guide.md` (issue #2903).
///
/// Why: upstream `testing-anti-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `testing-anti-patterns`'s
/// `SKILL.md` at `skills/testing-anti-patterns/references/detection-guide.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TESTING_ANTI_PATTERNS_DETECTION_GUIDE: &str =
    include_str!("../assets/skills/testing-anti-patterns/references/detection-guide.md");

/// `testing-anti-patterns` reference file `references/python-examples.md` (issue #2903).
///
/// Why: upstream `testing-anti-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `testing-anti-patterns`'s
/// `SKILL.md` at `skills/testing-anti-patterns/references/python-examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TESTING_ANTI_PATTERNS_PYTHON_EXAMPLES: &str =
    include_str!("../assets/skills/testing-anti-patterns/references/python-examples.md");

/// `testing-anti-patterns` reference file `references/tdd-connection.md` (issue #2903).
///
/// Why: upstream `testing-anti-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `testing-anti-patterns`'s
/// `SKILL.md` at `skills/testing-anti-patterns/references/tdd-connection.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const TESTING_ANTI_PATTERNS_TDD_CONNECTION: &str =
    include_str!("../assets/skills/testing-anti-patterns/references/tdd-connection.md");

/// `webapp-testing` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/webapp-testing/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/webapp-testing.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const WEBAPP_TESTING: &str = include_str!("../assets/skills/webapp-testing.md");
