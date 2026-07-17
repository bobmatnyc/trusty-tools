//! Skill-port batch 1 (issue #2903) — collaboration, data & architecture skills.
//!
//! Why: see `bundle_skills_batch1_debugging.rs` module doc — same epic (#2902),
//! same DOC-42 co-deploy motivation. This module holds the collaboration/data/
//! architecture group (git-workflow, requesting-code-review, writing-plans,
//! brainstorming, json-data-handling, database-migration, software-patterns).
//! What: `pub const` strings for each skill's `SKILL.md` entry point and any
//! `references/*.md` files, embedded via `include_str!`. Re-exported by
//! `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `skill_port_batch1_skills_are_in_bundle`.

/// `git-workflow` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/git-workflow/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/git-workflow.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const GIT_WORKFLOW: &str = include_str!("../assets/skills/git-workflow.md");

/// `requesting-code-review` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/requesting-code-review/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/requesting-code-review.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const REQUESTING_CODE_REVIEW: &str = include_str!("../assets/skills/requesting-code-review.md");

/// `requesting-code-review` reference file `references/code-reviewer-template.md` (issue #2903).
///
/// Why: upstream `requesting-code-review` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `requesting-code-review`'s
/// `SKILL.md` at `skills/requesting-code-review/references/code-reviewer-template.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const REQUESTING_CODE_REVIEW_CODE_REVIEWER_TEMPLATE: &str =
    include_str!("../assets/skills/requesting-code-review/references/code-reviewer-template.md");

/// `requesting-code-review` reference file `references/review-examples.md` (issue #2903).
///
/// Why: upstream `requesting-code-review` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `requesting-code-review`'s
/// `SKILL.md` at `skills/requesting-code-review/references/review-examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const REQUESTING_CODE_REVIEW_REVIEW_EXAMPLES: &str =
    include_str!("../assets/skills/requesting-code-review/references/review-examples.md");

/// `writing-plans` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/writing-plans/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/writing-plans.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const WRITING_PLANS: &str = include_str!("../assets/skills/writing-plans.md");

/// `writing-plans` reference file `references/best-practices.md` (issue #2903).
///
/// Why: upstream `writing-plans` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `writing-plans`'s
/// `SKILL.md` at `skills/writing-plans/references/best-practices.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WRITING_PLANS_BEST_PRACTICES: &str =
    include_str!("../assets/skills/writing-plans/references/best-practices.md");

/// `writing-plans` reference file `references/plan-structure-templates.md` (issue #2903).
///
/// Why: upstream `writing-plans` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `writing-plans`'s
/// `SKILL.md` at `skills/writing-plans/references/plan-structure-templates.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WRITING_PLANS_PLAN_STRUCTURE_TEMPLATES: &str =
    include_str!("../assets/skills/writing-plans/references/plan-structure-templates.md");

/// `brainstorming` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/brainstorming/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/brainstorming.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const BRAINSTORMING: &str = include_str!("../assets/skills/brainstorming.md");

/// `json-data-handling` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/json-data-handling/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/json-data-handling.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const JSON_DATA_HANDLING: &str = include_str!("../assets/skills/json-data-handling.md");

/// `database-migration` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/database-migration/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/database-migration.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const DATABASE_MIGRATION: &str = include_str!("../assets/skills/database-migration.md");

/// `database-migration` reference file `references/decision-trees.md` (issue #2903).
///
/// Why: upstream `database-migration` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `database-migration`'s
/// `SKILL.md` at `skills/database-migration/references/decision-trees.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const DATABASE_MIGRATION_DECISION_TREES: &str =
    include_str!("../assets/skills/database-migration/references/decision-trees.md");

/// `database-migration` reference file `references/troubleshooting.md` (issue #2903).
///
/// Why: upstream `database-migration` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `database-migration`'s
/// `SKILL.md` at `skills/database-migration/references/troubleshooting.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const DATABASE_MIGRATION_TROUBLESHOOTING: &str =
    include_str!("../assets/skills/database-migration/references/troubleshooting.md");

/// `software-patterns` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/software-patterns/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/software-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const SOFTWARE_PATTERNS: &str = include_str!("../assets/skills/software-patterns.md");

/// `software-patterns` reference file `references/anti-patterns.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/anti-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_ANTI_PATTERNS: &str =
    include_str!("../assets/skills/software-patterns/references/anti-patterns.md");

/// `software-patterns` reference file `references/code-smell-signals.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/code-smell-signals.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_CODE_SMELL_SIGNALS: &str =
    include_str!("../assets/skills/software-patterns/references/code-smell-signals.md");

/// `software-patterns` reference file `references/decision-trees.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/decision-trees.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_DECISION_TREES: &str =
    include_str!("../assets/skills/software-patterns/references/decision-trees.md");

/// `software-patterns` reference file `references/examples.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/examples.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_EXAMPLES: &str =
    include_str!("../assets/skills/software-patterns/references/examples.md");

/// `software-patterns` reference file `references/foundational-patterns.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/foundational-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_FOUNDATIONAL_PATTERNS: &str =
    include_str!("../assets/skills/software-patterns/references/foundational-patterns.md");

/// `software-patterns` reference file `references/situational-patterns.md` (issue #2903).
///
/// Why: upstream `software-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `software-patterns`'s
/// `SKILL.md` at `skills/software-patterns/references/situational-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SOFTWARE_PATTERNS_SITUATIONAL_PATTERNS: &str =
    include_str!("../assets/skills/software-patterns/references/situational-patterns.md");
