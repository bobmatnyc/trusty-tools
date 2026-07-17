//! End-to-end deploy proof for issue #2903's skill-port batch 1 (25 upstream
//! `universal/` skills) — companion file, following the existing
//! `tests_behavior_a/b/c/d/e`/`tests_behavior_reset_agents`/
//! `tests_behavior_skill_tiers`/`tests_behavior_2890_skills` split convention
//! (kept separate so `tests_behavior_a.rs` stays under the 500-SLOC
//! production cap; this file is itself capped at 1500 SLOC as a test file).
//!
//! Why: mirrors `tests_behavior_2890_skills_tests.rs`'s rationale exactly —
//! a skill asset file existing under `src/assets/skills/` is NOT sufficient
//! for it to ship; only `ALL` (`bundle_all.rs`) registration makes
//! `deploy_all_skill_tiers` see it, and issue #2902's constraint #2 (multi-
//! file skill directories) adds a SECOND failure mode this batch introduced:
//! a `references/*.md` file could be registered in `ALL` yet still never
//! land on disk if the deploy machinery only copied the entry-point
//! `SKILL.md`. Only observing files land in a real deployed `.claude/skills/`
//! tree — via the exact two-step path `tm install` uses — proves both
//! concerns are actually resolved.
//! What: `skill_port_batch1_sample_lands_in_deployed_dot_claude_skills` calls
//! `install_to` then `deploy_all_skill_tiers` (matching
//! `install_then_deploy_deploys_skills` in `tests_behavior_a.rs` and
//! `code_critic_skills_land_in_deployed_dot_claude_skills` in
//! `tests_behavior_2890_skills_tests.rs`) against a temp framework root, then
//! reads back a SAMPLE of the 25 batch-1 skills from the deployed directory:
//! `internal-comms` (single-file — no `references/`) and
//! `systematic-debugging` (multi-file — proves the `references/*.md` subtree
//! deploys ALONGSIDE the entry point at
//! `<dest>/systematic-debugging/references/<file>.md`, not just the entry
//! point itself), asserting content is byte-identical to the `include_str!`-
//! embedded constants.
//! Test: this IS the test module.

use trusty_mpm::core::bundle::{
    INTERNAL_COMMS, SYSTEMATIC_DEBUGGING, SYSTEMATIC_DEBUGGING_ANTI_PATTERNS,
    SYSTEMATIC_DEBUGGING_EXAMPLES, SYSTEMATIC_DEBUGGING_TROUBLESHOOTING,
    SYSTEMATIC_DEBUGGING_WORKFLOW,
};

use crate::commands::install::install_to;

#[test]
fn skill_port_batch1_sample_lands_in_deployed_dot_claude_skills() {
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());

    // Step 1: `tm install`'s first half — write every bundled artifact
    // (including the 93 skill-port batch-1 entries) under the framework root.
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
        result.deployed.contains(&"internal-comms".to_string()),
        "internal-comms must be deployed; got {:?}",
        result.deployed
    );
    assert!(
        result
            .deployed
            .contains(&"systematic-debugging".to_string()),
        "systematic-debugging must be deployed; got {:?}",
        result.deployed
    );
    assert!(
        result
            .deployed
            .contains(&"systematic-debugging/references/workflow.md".to_string()),
        "systematic-debugging's reference files must deploy alongside it; got {:?}",
        result.deployed
    );

    // The single-file skill: a real file on disk at Claude Code's native
    // discovery path (`<dest>/<name>/SKILL.md`), byte-identical to the
    // compile-time embedded constant.
    let internal_comms_deployed = paths
        .claude_skills_dir()
        .join("internal-comms")
        .join("SKILL.md");
    assert!(
        internal_comms_deployed.is_file(),
        "expected skill at {}",
        internal_comms_deployed.display()
    );
    assert_eq!(
        std::fs::read_to_string(&internal_comms_deployed).unwrap(),
        INTERNAL_COMMS,
    );

    // The multi-file skill: entry point AND every references/*.md sibling
    // must land, at the multi-file layout the machinery extension adds
    // (`<dest>/<name>/references/<file>.md`, alongside `<dest>/<name>/SKILL.md`).
    let debugging_dir = paths.claude_skills_dir().join("systematic-debugging");
    assert_eq!(
        std::fs::read_to_string(debugging_dir.join("SKILL.md")).unwrap(),
        SYSTEMATIC_DEBUGGING,
    );
    for (file_name, expected) in [
        ("anti-patterns.md", SYSTEMATIC_DEBUGGING_ANTI_PATTERNS),
        ("examples.md", SYSTEMATIC_DEBUGGING_EXAMPLES),
        ("troubleshooting.md", SYSTEMATIC_DEBUGGING_TROUBLESHOOTING),
        ("workflow.md", SYSTEMATIC_DEBUGGING_WORKFLOW),
    ] {
        let ref_path = debugging_dir.join("references").join(file_name);
        assert!(
            ref_path.is_file(),
            "expected reference file at {}",
            ref_path.display()
        );
        assert_eq!(std::fs::read_to_string(&ref_path).unwrap(), expected);
    }
}
