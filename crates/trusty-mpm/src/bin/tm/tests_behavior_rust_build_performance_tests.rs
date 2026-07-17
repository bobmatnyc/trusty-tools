//! End-to-end deploy proof for the `rust-build-performance` bundled skill
//! (per Bob directive 2026-07-17) — companion file, following the existing
//! `tests_behavior_2911_documentation_style_tests.rs` split convention (kept
//! separate so no single file grows past its production/test SLOC cap; this
//! file is itself capped at 1500 SLOC as a test file).
//!
//! Why: mirrors `tests_behavior_2911_documentation_style_tests.rs`'s
//! rationale — a skill asset file existing under `src/assets/skills/` is NOT
//! sufficient for it to ship; only `ALL` (`bundle_all.rs`) registration makes
//! `deploy_all_skill_tiers` see it. Only observing the file land in a real
//! deployed `.claude/skills/` tree — via the exact two-step path `tm install`
//! uses — proves the concern is actually resolved for this skill.
//! What: `rust_build_performance_lands_in_deployed_dot_claude_skills` calls
//! `install_to` then `deploy_all_skill_tiers` against a temp framework root,
//! then reads back the deployed `SKILL.md` and asserts it's byte-identical
//! to the `include_str!`-embedded constant.
//! Test: this IS the test module.

use trusty_mpm::core::bundle::RUST_BUILD_PERFORMANCE;

use crate::commands::install::install_to;

#[test]
fn rust_build_performance_lands_in_deployed_dot_claude_skills() {
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());

    // Step 1: `tm install`'s first half — write every bundled artifact
    // (including rust-build-performance) under the framework root.
    install_to(&paths, false).unwrap();

    // Step 2: `tm install`'s second half — the SAME multi-tier orchestrator
    // that copies the framework-root skill sources into `.claude/skills/`.
    let result = trusty_mpm::core::skill_tiers::deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.claude_skills_dir(),
        |_| true,
    )
    .unwrap()
    .stats;

    assert!(
        result
            .deployed
            .contains(&"rust-build-performance".to_string()),
        "rust-build-performance must be deployed; got {:?}",
        result.deployed
    );

    // Flat single-file skill deploys to `<dest>/<name>/SKILL.md`, matching
    // Claude Code's native discovery format (same layout as every other
    // bundled skill, whether the source is flat or a references/ directory).
    let skill_dir = paths.claude_skills_dir().join("rust-build-performance");
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        RUST_BUILD_PERFORMANCE,
    );
}
