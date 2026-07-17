//! End-to-end deploy proof for issue #2890's two new bundled skills —
//! companion file, following the existing `tests_behavior_a/b/c/d/e`/
//! `tests_behavior_reset_agents`/`tests_behavior_skill_tiers` split
//! convention (kept separate from `tests_behavior_a.rs` so that file stays
//! under the 500-SLOC production cap; this file is itself capped at 1500
//! SLOC as a test file).
//!
//! Why: code-critic's `skills:` frontmatter (#2890, DOC-42) declares
//! `code-review-standards` and `contract-driven-testing`. Both asset files
//! existing under `src/assets/skills/` is NOT sufficient for them to ship —
//! `tm install`/`tm update` deploy exclusively from the `ALL` bundle table
//! (`bundle_all.rs`), which is populated by `install_to` writing every
//! `ALL` entry under the framework root, which `deploy_all_skill_tiers`
//! then reads from (`paths.skill_source_dir()`). A file that exists on disk
//! but is missing from `ALL` silently never deploys — the exact historical
//! bug that orphaned `tm-doctor.md` (see `bundle_tm_skills.rs`'s module doc
//! and `bundle_tests.rs::tm_doctor_skill_is_wired_into_bundle`). Parsing the
//! new `skills:` frontmatter cleanly is not evidence the files ship; only
//! observing them land in a real deployed `.claude/skills/` tree is.
//! What: `code_critic_skills_land_in_deployed_dot_claude_skills` calls the
//! exact two-step path `tm install` uses (`install_to` then
//! `deploy_all_skill_tiers`, matching `install_then_deploy_deploys_skills`
//! in `tests_behavior_a.rs`) against a temp framework root, then reads back
//! `<dest>/code-review-standards/SKILL.md` and
//! `<dest>/contract-driven-testing/SKILL.md` from the deployed directory and
//! asserts their content is byte-identical to the `include_str!`-embedded
//! constants.
//! Test: this IS the test module.

use trusty_mpm::core::bundle::{CODE_REVIEW_STANDARDS, CONTRACT_DRIVEN_TESTING};

use crate::commands::install::install_to;

#[test]
fn code_critic_skills_land_in_deployed_dot_claude_skills() {
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());

    // Step 1: `tm install`'s first half — write every bundled artifact
    // (including the two new skill files) under the framework root.
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
            .contains(&"code-review-standards".to_string()),
        "code-review-standards must be deployed; got {:?}",
        result.deployed
    );
    assert!(
        result
            .deployed
            .contains(&"contract-driven-testing".to_string()),
        "contract-driven-testing must be deployed; got {:?}",
        result.deployed
    );

    // The proof: each skill lands as a real file on disk under the deployed
    // `.claude/skills/` tree, at the path Claude Code's native discovery
    // format expects (`<dest>/<name>/SKILL.md`), with content identical to
    // what `bundle.rs` embeds at compile time.
    let code_review_standards_deployed = paths
        .claude_skills_dir()
        .join("code-review-standards")
        .join("SKILL.md");
    assert!(
        code_review_standards_deployed.is_file(),
        "expected skill at {}",
        code_review_standards_deployed.display()
    );
    assert_eq!(
        std::fs::read_to_string(&code_review_standards_deployed).unwrap(),
        CODE_REVIEW_STANDARDS,
    );

    let contract_driven_testing_deployed = paths
        .claude_skills_dir()
        .join("contract-driven-testing")
        .join("SKILL.md");
    assert!(
        contract_driven_testing_deployed.is_file(),
        "expected skill at {}",
        contract_driven_testing_deployed.display()
    );
    assert_eq!(
        std::fs::read_to_string(&contract_driven_testing_deployed).unwrap(),
        CONTRACT_DRIVEN_TESTING,
    );
}
