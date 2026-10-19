//! `tm install`'s skill-deploy behavior test, split out of `tests_behavior_a_tests.rs`
//! (#7102).
//!
//! Why: `tests_behavior_a_tests.rs` sat exactly at the 500-SLOC production cap, and
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

use crate::commands::install::{install_to_with, skill_report_lines, test_content, test_roster};

#[test]
fn install_then_deploy_deploys_skills() {
    // Regression for #386: a fresh install must populate the skill tier from
    // the bundled skill sources, not leave it empty. Calls the SAME function
    // `install()`'s skill step actually calls (PR #2818 review, round 3;
    // #7102 moved the destination split behind
    // `skill_install_tiers::deploy_install_skill_tiers`).
    let dir = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(dir.path());
    install_to_with(&paths, false, &test_roster(), &test_content()).unwrap();
    let result = trusty_mpm::core::skill_install_tiers::deploy_install_skill_tiers(&paths)
        .unwrap()
        .stats;
    // #10055: the deployed set is derived from `content/skills` on disk, never
    // a hard-coded count — splitting a skill is a content change only.
    // Stats report deploy keys: `<stem>` for an entry (it lands as
    // `<dest>/<stem>/SKILL.md`), `<stem>/references/<file>` for a reference.
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
    let on_disk = skill_deploy_keys_on_disk();
    let gaps = deploy_gaps(&on_disk, &result.deployed);
    assert!(
        gaps.is_empty(),
        "deployed skills != content/skills on disk: {gaps:#?}"
    );
    // Mutation: one reference dropped, or one key deployed twice, is reported.
    let mut dropped = result.deployed.clone();
    let reference = dropped
        .iter()
        .position(|key| key.contains("/references/"))
        .expect("a deployed references file");
    let lost = dropped.remove(reference);
    assert_eq!(deploy_gaps(&on_disk, &dropped), [format!("missing {lost}")]);
    let mut doubled = result.deployed.clone();
    doubled.push(lost.clone());
    assert_eq!(
        deploy_gaps(&on_disk, &doubled),
        [format!("{lost} deployed 2 times")]
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

/// The deploy key of every skill file under the checkout's `content/skills`:
/// `<stem>` for an entry `<stem>.md`, else its path below `content/skills`
/// (`<stem>/references/<file>`). Dot-entries are skipped. A plain directory
/// walk that shares no code with the loader or the deployer, so either one
/// dropping or doubling a file disagrees with it (#10055).
fn skill_deploy_keys_on_disk() -> Vec<String> {
    fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read a content/skills directory") {
            let entry = entry.expect("a directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let key = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().expect("a file type").is_dir() {
                walk(&entry.path(), &key, out);
            } else if !key.contains('/') {
                out.push(key.trim_end_matches(".md").to_owned());
            } else {
                out.push(key);
            }
        }
    }
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let mut keys = Vec::new();
    walk(&root.join("content/skills"), "", &mut keys);
    assert!(!keys.is_empty(), "the walk found no skill file");
    keys
}

/// Every way `deployed` differs from `expected` read as keys deployed once
/// each: a key missing, a key nobody expected, a key deployed more than once.
fn deploy_gaps(expected: &[String], deployed: &[String]) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<&str, usize> = Default::default();
    for key in deployed {
        *seen.entry(key).or_default() += 1;
    }
    let mut gaps: Vec<String> = expected
        .iter()
        .filter(|key| !seen.contains_key(key.as_str()))
        .map(|key| format!("missing {key}"))
        .collect();
    for (key, times) in seen {
        if !expected.iter().any(|e| e == key) {
            gaps.push(format!("unexpected {key}"));
        }
        if times > 1 {
            gaps.push(format!("{key} deployed {times} times"));
        }
    }
    gaps
}
