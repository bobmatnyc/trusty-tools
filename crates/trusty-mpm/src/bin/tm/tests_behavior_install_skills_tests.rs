//! `tm install`'s skill-deploy behavior test, split out of `tests_behavior_a.rs`
//! (#7102).
//!
//! Why: `tests_behavior_a.rs` sat exactly at the 500-SLOC production cap, and
//! #7102 adds an assertion to `install_then_deploy_deploys_skills` — that a
//! bundled skill must NOT land in the operator's `~/.claude/skills`. That file's
//! own comments already route each deep skill assertion into a `_tests.rs`
//! sibling for this reason, so the whole test moves here rather than losing the
//! coverage that pays for the cap.
//! What: `install_then_deploy_deploys_skills`, verbatim from its old home plus
//! the #7102 assertion. It exercises
//! `core::skill_install_tiers::deploy_install_skill_tiers` — the function
//! `commands::install::install` calls — so a change to where the installer
//! writes fails here rather than in production.
//! Test: this file IS the test.

use crate::commands::install::{install_to, skill_report_lines};

#[test]
fn install_then_deploy_deploys_skills() {
    // Regression for #386: a fresh install must populate the skill tier from
    // the bundled skill sources, not leave it empty. Calls the SAME function
    // `install()`'s skill step actually calls (PR #2818 review, round 3;
    // #7102 moved the destination split behind
    // `skill_install_tiers::deploy_install_skill_tiers`).
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    install_to(&paths, false).unwrap();
    let result = trusty_mpm::core::skill_install_tiers::deploy_install_skill_tiers(&paths)
        .unwrap()
        .stats;
    // The full /tm- skill portfolio deploys on first install: 21 skills total
    // — 19 /tm- portfolio skills (tm-circuit-breaker, tm-verification-protocols,
    // tm-tool-usage-guide, tm-git-file-tracking, tm-adr, tm-workflow,
    // tm-agent-architecture, tm-postmortem, tm-bug-reporting,
    // tm-teaching-templates, tm-ticketing,
    // tm-delegation-patterns, tm-session-management, tm-session-pause,
    // tm-session-resume, tm-init, tm-issues-prune, tm-cli-operations,
    // tm-slack) + tm-doctor + the tm overview skill
    // (tm-skills-portfolio epic: the `example-skill.md` placeholder and the
    // 11 mpm-* guidance skills no longer ship; the previously-orphaned
    // tm-doctor.md is now wired in; issue #2185 added tm-issues-prune; issue
    // #2321 added tm-cli-operations; issue #4447 added
    // tm-slack-canvas-delivery, replaced 1-for-1 by tm-slack, issue #4761) + 2 (issue
    // #2890: code-review-standards, contract-driven-testing — code-critic's
    // declared `skills:` dependencies; see
    // `tests_behavior_2890_skills_tests.rs` for the dedicated deep assertions
    // on those two) + 93 (issue #2903,
    // skill-port batch 1: 25 upstream universal/ skill entry points + 68
    // references/*.md files carried alongside multi-file skills; see
    // `tests_behavior_2903_skills_tests.rs` for the dedicated deep
    // assertions) + 7
    // (issue #2911: the `documentation-style` bundled skill — entry SKILL.md
    // plus 6 references/*.md files; see
    // `tests_behavior_2911_documentation_style_tests.rs` for the dedicated
    // deep assertions)
    // + 7 (issue #2913: the `tm-capabilities` auto-generated harness catalog
    // — entry SKILL.md plus 5 generated + 1 hand-authored references/*.md
    // files; see `tests_behavior_generate_tests.rs` for the generator's own
    // coverage) + 1
    // (rust-build-performance, per Bob directive 2026-07-17: a single
    // flat-file bundled skill declared by rust-engineer and tauri-engineer;
    // see `tests_behavior_rust_build_performance_tests.rs` for the dedicated
    // deploy-reachability assertion).
    // Issue #5202 retired tm-pr-workflow into tm-workflow: 22 - 1 = 21.
    // 21 + 2 + 93 + 7 + 7 + 1 = 131. See `bundle_tm_skills.rs`/
    // `bundle_tm_capabilities.rs`/`bundle.rs`
    // (CODE_REVIEW_STANDARDS, CONTRACT_DRIVEN_TESTING)/`bundle_all.rs::ALL`
    // for the authoritative list.
    // Stats report stems (no .md suffix) because each skill lands as
    // <dest>/<name>/SKILL.md to match Claude Code's native discovery format.
    for expected in [
        "tm-circuit-breaker",
        "tm-doctor",
        "tm-issues-prune",
        "tm-cli-operations",
        "tm-capabilities",
        "documentation-style",
    ] {
        assert!(
            result.deployed.contains(&expected.to_string()),
            "{expected} must be deployed; got {:?}",
            result.deployed
        );
    }
    assert_eq!(
        result.deployed.len(),
        134,
        "expected 134 skill files deployed (19 /tm- portfolio + tm-prose-style (#7423) \
         + tm-secrets (#7527) + tm-doctor + tm overview + code-review-standards + \
         contract-driven-testing + 93 skill-port batch-1 entries + 7 documentation-style \
         entries + 8 tm-capabilities entries (#4946 added references/framework.md) + 1 \
         rust-build-performance entry); got {:?}",
        result.deployed
    );
    assert!(result.skipped.is_empty());
    assert!(result.unchanged.is_empty());
    // Each skill must be deployed as a directory with SKILL.md inside — in the
    // MANAGED tier, never the operator's `~/.claude/skills` (#6586, #7102).
    assert!(
        !paths
            .claude_skills_dir()
            .join("tm-circuit-breaker")
            .exists(),
        "a bundled skill must not land in ~/.claude/skills — `tm doctor`'s \
         legacy_sources check flags exactly that (#7102)"
    );
    let deployed = paths
        .skill_deploy_dir()
        .join("tm-circuit-breaker")
        .join("SKILL.md");
    assert!(
        deployed.is_file(),
        "expected skill at {}",
        deployed.display()
    );
    let lines = skill_report_lines(&result);
    assert!(
        lines.iter().any(|l| l.contains("tm-circuit-breaker")),
        "lines = {lines:?}"
    );
}
