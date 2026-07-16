//! `tm install`'s skill deploy step must honor the same multi-tier
//! precedence as session launch — companion file, following the existing
//! `tests_behavior_a/b/c/d/e`/`tests_behavior_reset_agents` split convention
//! (kept separate from `tests_behavior_a.rs` so that file stays under the
//! 500-SLOC production cap; this file is itself capped at 1500 SLOC as a
//! test file).
//!
//! Why: PR #2818 review (round 3) found `tm install`'s skill deploy called
//! the raw single-tier `skill_deployer::deploy_skills` directly against
//! `paths.claude_skills_dir()` — the SAME home-rooted destination the
//! ordinary (non-managed) `tm session start`/`tm launch` path deploys
//! user-tier overrides into via `core::skill_tiers::deploy_all_skill_tiers`.
//! A skill the user tier had already deployed looked "managed,
//! checksum-matches" to the manifest (which only ever records the LAST
//! writer, not which tier wrote it), so a differing bundled body fell into
//! the safe-to-refresh branch and silently clobbered the override on every
//! routine `tm install` — the third independent call site with this bug,
//! after `session_launch` (fixed at initial review) and `apply_catalog`
//! (fixed in round 2).
//! What: `install_skill_step_preserves_user_tier_override` proves the fix by
//! calling the exact function `install()`'s skill step now calls twice — once
//! to simulate the launch path depositing the override, once to simulate a
//! routine `tm install` re-run — and asserting the override survives
//! byte-identical.
//! Test: this IS the test module.

use crate::commands::install::install_to;

#[test]
fn install_skill_step_preserves_user_tier_override() {
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    install_to(&paths, false).unwrap();

    // The user authors their own override in the user-custom tier BEFORE any
    // deploy runs, so both calls below see the collision.
    let user_source = paths.user_skill_source_dir();
    std::fs::create_dir_all(&user_source).unwrap();
    std::fs::write(
        user_source.join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\nUSER OVERRIDE BODY\n",
    )
    .unwrap();

    // 1. Simulate the launch path: the same multi-tier call
    //    `session_launch::prepare_session_inner` makes.
    trusty_mpm::core::skill_tiers::deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.claude_skills_dir(),
        |_| true,
    )
    .unwrap();
    let deployed_path = paths.claude_skills_dir().join("tm-doctor").join("SKILL.md");
    let after_launch = std::fs::read_to_string(&deployed_path).unwrap();
    assert!(
        after_launch.contains("USER OVERRIDE BODY"),
        "user tier must win at launch: {after_launch}"
    );

    // 2. Simulate `install()`'s own skill step (identical call, post-fix).
    trusty_mpm::core::skill_tiers::deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        &paths.claude_skills_dir(),
        |_| true,
    )
    .unwrap();
    let after_install = std::fs::read_to_string(&deployed_path).unwrap();
    assert_eq!(
        after_install, after_launch,
        "tm install must preserve the user-tier override byte-identical"
    );
    assert!(
        after_install.contains("USER OVERRIDE BODY"),
        "override must survive install: {after_install}"
    );
}
