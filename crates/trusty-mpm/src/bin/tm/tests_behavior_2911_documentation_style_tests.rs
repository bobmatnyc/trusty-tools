//! End-to-end deploy proof for issue #2911's `documentation-style` bundled
//! skill — companion file, following the existing
//! `tests_behavior_2890_skills`/`tests_behavior_2903_skills` split
//! convention (kept separate so no single file grows past its production/test
//! SLOC cap; this file is itself capped at 1500 SLOC as a test file).
//!
//! Why: mirrors `tests_behavior_2903_skills_tests.rs`'s rationale exactly —
//! a skill asset file existing under `src/assets/skills/` is NOT sufficient
//! for it to ship; only `ALL` (`bundle_all.rs`) registration makes
//! `deploy_all_skill_tiers` see it, and the multi-file skill-directory
//! extension (epic #2902 constraint #2, reused here) adds a SECOND failure
//! mode: a `references/*.md` file could be registered in `ALL` yet still
//! never land on disk if the deploy machinery only copied the entry-point
//! `SKILL.md`. Only observing files land in a real deployed `.claude/skills/`
//! tree — via the exact two-step path `tm install` uses — proves both
//! concerns are actually resolved for this skill.
//! What: `documentation_style_lands_in_deployed_dot_claude_skills` calls
//! `install_to` then `deploy_all_skill_tiers` (matching
//! `skill_port_batch1_sample_lands_in_deployed_dot_claude_skills` in
//! `tests_behavior_2903_skills_tests.rs`) against a temp framework root, then
//! reads back the entry `SKILL.md` and all 6 `references/*.md` files from the
//! deployed directory, asserting content is byte-identical to the
//! `include_str!`-embedded constants.
//! Test: this IS the test module.

use trusty_mpm::core::bundle::{
    DOCUMENTATION_STYLE, DOCUMENTATION_STYLE_BLOCK_INLINE, DOCUMENTATION_STYLE_CLASS,
    DOCUMENTATION_STYLE_FILE_LEVEL, DOCUMENTATION_STYLE_METHOD_FUNCTION,
    DOCUMENTATION_STYLE_README, DOCUMENTATION_STYLE_SPEC,
};

use crate::commands::install::install_to;

#[test]
fn documentation_style_lands_in_deployed_dot_claude_skills() {
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());

    // Step 1: `tm install`'s first half — write every bundled artifact
    // (including the 7 documentation-style entries) under the framework root.
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
        result.deployed.contains(&"documentation-style".to_string()),
        "documentation-style must be deployed; got {:?}",
        result.deployed
    );
    assert!(
        result
            .deployed
            .contains(&"documentation-style/references/method-function.md".to_string()),
        "documentation-style's reference files must deploy alongside it; got {:?}",
        result.deployed
    );

    // Entry point AND every references/*.md sibling must land, at the
    // multi-file layout (`<dest>/<name>/references/<file>.md`, alongside
    // `<dest>/<name>/SKILL.md`).
    let skill_dir = paths.claude_skills_dir().join("documentation-style");
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        DOCUMENTATION_STYLE,
    );
    for (file_name, expected) in [
        ("spec.md", DOCUMENTATION_STYLE_SPEC),
        ("readme.md", DOCUMENTATION_STYLE_README),
        ("file-level.md", DOCUMENTATION_STYLE_FILE_LEVEL),
        ("class.md", DOCUMENTATION_STYLE_CLASS),
        ("method-function.md", DOCUMENTATION_STYLE_METHOD_FUNCTION),
        ("block-inline.md", DOCUMENTATION_STYLE_BLOCK_INLINE),
    ] {
        let ref_path = skill_dir.join("references").join(file_name);
        assert!(
            ref_path.is_file(),
            "expected reference file at {}",
            ref_path.display()
        );
        assert_eq!(std::fs::read_to_string(&ref_path).unwrap(), expected);
    }
}
