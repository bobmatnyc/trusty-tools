//! Skill-port batch 1 (issue #2903) — process, general & data-format skills.
//!
//! Why: see `bundle_skills_batch1_debugging.rs` module doc — same epic (#2902),
//! same DOC-42 co-deploy motivation. This module holds the remaining batch-1
//! group (code-production-process, internal-comms, artifacts-builder,
//! model-context-builder, xlsx).
//! What: `pub const` strings for each skill's `SKILL.md` entry point and any
//! `references/*.md` files, embedded via `include_str!`. Re-exported by
//! `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `skill_port_batch1_skills_are_in_bundle`.

/// `code-production-process` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/code-production-process/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/code-production-process.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const CODE_PRODUCTION_PROCESS: &str =
    include_str!("../assets/skills/code-production-process.md");

/// `code-production-process` reference file `references/critic-isolation.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/critic-isolation.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_CRITIC_ISOLATION: &str =
    include_str!("../assets/skills/code-production-process/references/critic-isolation.md");

/// `code-production-process` reference file `references/skip-rules.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/skip-rules.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_SKIP_RULES: &str =
    include_str!("../assets/skills/code-production-process/references/skip-rules.md");

/// `code-production-process` reference file `references/stage-architect.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-architect.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_ARCHITECT: &str =
    include_str!("../assets/skills/code-production-process/references/stage-architect.md");

/// `code-production-process` reference file `references/stage-critic.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-critic.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_CRITIC: &str =
    include_str!("../assets/skills/code-production-process/references/stage-critic.md");

/// `code-production-process` reference file `references/stage-implement.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-implement.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_IMPLEMENT: &str =
    include_str!("../assets/skills/code-production-process/references/stage-implement.md");

/// `code-production-process` reference file `references/stage-research.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-research.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_RESEARCH: &str =
    include_str!("../assets/skills/code-production-process/references/stage-research.md");

/// `code-production-process` reference file `references/stage-security.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-security.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_SECURITY: &str =
    include_str!("../assets/skills/code-production-process/references/stage-security.md");

/// `code-production-process` reference file `references/stage-tests.md` (issue #2903).
///
/// Why: upstream `code-production-process` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `code-production-process`'s
/// `SKILL.md` at `skills/code-production-process/references/stage-tests.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const CODE_PRODUCTION_PROCESS_STAGE_TESTS: &str =
    include_str!("../assets/skills/code-production-process/references/stage-tests.md");

/// `internal-comms` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/internal-comms/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/internal-comms.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const INTERNAL_COMMS: &str = include_str!("../assets/skills/internal-comms.md");

/// `artifacts-builder` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/artifacts-builder/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/artifacts-builder.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const ARTIFACTS_BUILDER: &str = include_str!("../assets/skills/artifacts-builder.md");

/// `model-context-builder` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/model-context-builder/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/model-context-builder.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const MODEL_CONTEXT_BUILDER: &str = include_str!("../assets/skills/model-context-builder.md");

/// `xlsx` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/xlsx/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/xlsx.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const XLSX: &str = include_str!("../assets/skills/xlsx.md");
