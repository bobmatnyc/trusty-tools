//! Skill-port batch 1 (issue #2903) — security, web & infrastructure skills.
//!
//! Why: see `bundle_skills_batch1_debugging.rs` module doc — same epic (#2902),
//! same DOC-42 co-deploy motivation. This module holds the security/web/infra
//! group (security-scanning, api-design-patterns, web-performance-optimization,
//! api-documentation, env-manager). NOTE: `env-manager` is framework-specific
//! (Next.js/Vite/React/Node.js) despite living under upstream `universal/` —
//! ported verbatim per the stack-neutrality exception-flagging policy (#2005)
//! rather than rewritten; see the batch-1 PR description for the full flag.
//! What: `pub const` strings for each skill's `SKILL.md` entry point and any
//! `references/*.md` files, embedded via `include_str!`. Re-exported by
//! `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `skill_port_batch1_skills_are_in_bundle`.

/// `security-scanning` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/security-scanning/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/security-scanning.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const SECURITY_SCANNING: &str = include_str!("../assets/skills/security-scanning.md");

/// `security-scanning` reference file `references/ci-workflows.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/ci-workflows.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_CI_WORKFLOWS: &str =
    include_str!("../assets/skills/security-scanning/references/ci-workflows.md");

/// `security-scanning` reference file `references/common-findings-and-fixes.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/common-findings-and-fixes.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_COMMON_FINDINGS_AND_FIXES: &str =
    include_str!("../assets/skills/security-scanning/references/common-findings-and-fixes.md");

/// `security-scanning` reference file `references/open-source-safety.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/open-source-safety.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_OPEN_SOURCE_SAFETY: &str =
    include_str!("../assets/skills/security-scanning/references/open-source-safety.md");

/// `security-scanning` reference file `references/supply-chain-and-sbom.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/supply-chain-and-sbom.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_SUPPLY_CHAIN_AND_SBOM: &str =
    include_str!("../assets/skills/security-scanning/references/supply-chain-and-sbom.md");

/// `security-scanning` reference file `references/tooling-matrix.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/tooling-matrix.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_TOOLING_MATRIX: &str =
    include_str!("../assets/skills/security-scanning/references/tooling-matrix.md");

/// `security-scanning` reference file `references/triage-and-remediation.md` (issue #2903).
///
/// Why: upstream `security-scanning` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `security-scanning`'s
/// `SKILL.md` at `skills/security-scanning/references/triage-and-remediation.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const SECURITY_SCANNING_TRIAGE_AND_REMEDIATION: &str =
    include_str!("../assets/skills/security-scanning/references/triage-and-remediation.md");

/// `api-design-patterns` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/api-design-patterns/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/api-design-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const API_DESIGN_PATTERNS: &str = include_str!("../assets/skills/api-design-patterns.md");

/// `api-design-patterns` reference file `references/authentication.md` (issue #2903).
///
/// Why: upstream `api-design-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `api-design-patterns`'s
/// `SKILL.md` at `skills/api-design-patterns/references/authentication.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const API_DESIGN_PATTERNS_AUTHENTICATION: &str =
    include_str!("../assets/skills/api-design-patterns/references/authentication.md");

/// `api-design-patterns` reference file `references/graphql-patterns.md` (issue #2903).
///
/// Why: upstream `api-design-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `api-design-patterns`'s
/// `SKILL.md` at `skills/api-design-patterns/references/graphql-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const API_DESIGN_PATTERNS_GRAPHQL_PATTERNS: &str =
    include_str!("../assets/skills/api-design-patterns/references/graphql-patterns.md");

/// `api-design-patterns` reference file `references/grpc-patterns.md` (issue #2903).
///
/// Why: upstream `api-design-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `api-design-patterns`'s
/// `SKILL.md` at `skills/api-design-patterns/references/grpc-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const API_DESIGN_PATTERNS_GRPC_PATTERNS: &str =
    include_str!("../assets/skills/api-design-patterns/references/grpc-patterns.md");

/// `api-design-patterns` reference file `references/rest-patterns.md` (issue #2903).
///
/// Why: upstream `api-design-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `api-design-patterns`'s
/// `SKILL.md` at `skills/api-design-patterns/references/rest-patterns.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const API_DESIGN_PATTERNS_REST_PATTERNS: &str =
    include_str!("../assets/skills/api-design-patterns/references/rest-patterns.md");

/// `api-design-patterns` reference file `references/versioning-strategies.md` (issue #2903).
///
/// Why: upstream `api-design-patterns` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `api-design-patterns`'s
/// `SKILL.md` at `skills/api-design-patterns/references/versioning-strategies.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const API_DESIGN_PATTERNS_VERSIONING_STRATEGIES: &str =
    include_str!("../assets/skills/api-design-patterns/references/versioning-strategies.md");

/// `web-performance-optimization` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/web-performance-optimization/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/web-performance-optimization.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const WEB_PERFORMANCE_OPTIMIZATION: &str =
    include_str!("../assets/skills/web-performance-optimization.md");

/// `web-performance-optimization` reference file `references/core-web-vitals.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/core-web-vitals.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_CORE_WEB_VITALS: &str =
    include_str!("../assets/skills/web-performance-optimization/references/core-web-vitals.md");

/// `web-performance-optimization` reference file `references/framework-specific.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/framework-specific.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_FRAMEWORK_SPECIFIC: &str =
    include_str!("../assets/skills/web-performance-optimization/references/framework-specific.md");

/// `web-performance-optimization` reference file `references/modern-patterns-2025.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/modern-patterns-2025.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_MODERN_PATTERNS_2025: &str = include_str!(
    "../assets/skills/web-performance-optimization/references/modern-patterns-2025.md"
);

/// `web-performance-optimization` reference file `references/monitoring.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/monitoring.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_MONITORING: &str =
    include_str!("../assets/skills/web-performance-optimization/references/monitoring.md");

/// `web-performance-optimization` reference file `references/optimization-techniques.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/optimization-techniques.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_OPTIMIZATION_TECHNIQUES: &str = include_str!(
    "../assets/skills/web-performance-optimization/references/optimization-techniques.md"
);

/// `web-performance-optimization` reference file `references/quick-wins.md` (issue #2903).
///
/// Why: upstream `web-performance-optimization` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `web-performance-optimization`'s
/// `SKILL.md` at `skills/web-performance-optimization/references/quick-wins.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const WEB_PERFORMANCE_OPTIMIZATION_QUICK_WINS: &str =
    include_str!("../assets/skills/web-performance-optimization/references/quick-wins.md");

/// `api-documentation` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/api-documentation/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/api-documentation.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const API_DOCUMENTATION: &str = include_str!("../assets/skills/api-documentation.md");

/// `env-manager` skill — upstream port (issue #2903, epic #2902).
///
/// Why: ported from `bobmatnyc/claude-mpm-skills` `universal/**/env-manager/SKILL.md`
/// so agents whose `skills:` frontmatter declares it (resolved via the DOC-42
/// co-deploy mechanism, #2889) get real content instead of a dangling
/// reference. Registration in `ALL` (not just the asset file existing) is
/// what makes `deploy_all_skill_tiers` actually ship it — the tm-doctor.md
/// orphaning lesson (`bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to `skills/env-manager.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_skills_are_in_bundle`.
pub const ENV_MANAGER: &str = include_str!("../assets/skills/env-manager.md");

/// `env-manager` reference file `references/frameworks.md` (issue #2903).
///
/// Why: upstream `env-manager` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `env-manager`'s
/// `SKILL.md` at `skills/env-manager/references/frameworks.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ENV_MANAGER_FRAMEWORKS: &str =
    include_str!("../assets/skills/env-manager/references/frameworks.md");

/// `env-manager` reference file `references/security.md` (issue #2903).
///
/// Why: upstream `env-manager` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `env-manager`'s
/// `SKILL.md` at `skills/env-manager/references/security.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ENV_MANAGER_SECURITY: &str =
    include_str!("../assets/skills/env-manager/references/security.md");

/// `env-manager` reference file `references/synchronization.md` (issue #2903).
///
/// Why: upstream `env-manager` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `env-manager`'s
/// `SKILL.md` at `skills/env-manager/references/synchronization.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ENV_MANAGER_SYNCHRONIZATION: &str =
    include_str!("../assets/skills/env-manager/references/synchronization.md");

/// `env-manager` reference file `references/troubleshooting.md` (issue #2903).
///
/// Why: upstream `env-manager` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `env-manager`'s
/// `SKILL.md` at `skills/env-manager/references/troubleshooting.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ENV_MANAGER_TROUBLESHOOTING: &str =
    include_str!("../assets/skills/env-manager/references/troubleshooting.md");

/// `env-manager` reference file `references/validation.md` (issue #2903).
///
/// Why: upstream `env-manager` uses progressive disclosure — Claude Code loads
/// this file on demand alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside `env-manager`'s
/// `SKILL.md` at `skills/env-manager/references/validation.md`.
/// Test: `bundle_table_is_complete`, `skill_port_batch1_references_land_on_disk`.
pub const ENV_MANAGER_VALIDATION: &str =
    include_str!("../assets/skills/env-manager/references/validation.md");
