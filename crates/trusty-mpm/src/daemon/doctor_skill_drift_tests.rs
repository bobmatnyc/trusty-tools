//! Tests for [`super`] — the #4604 `skill_staleness` severity table.
//!
//! Why: the check's job is to be believed, so every verdict it can render needs
//! a case that mutates exactly one input and proves the verdict moves with it —
//! including the two the pre-#4604 version could not produce at all (`Unknown`
//! for unverifiable, and the separate FROZEN report).
//! What: one test per severity branch plus the message-bounding case.
//! Test: this file.

use super::*;
use crate::core::skill_drift::SkillReference;
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};
use std::collections::BTreeMap;
use std::fs;
use tempfile::TempDir;

/// Build a reference map from literal (stem, content) pairs.
fn reference_of(pairs: &[(&str, &str)]) -> SkillReference {
    SkillReference {
        assets: pairs
            .iter()
            .map(|(s, c)| (s.to_string(), c.to_string()))
            .collect::<BTreeMap<_, _>>(),
        origin: "this binary's embedded bundled assets".to_string(),
    }
}

/// Deploy `stem` into `dest` with `file_content` on disk and the checksum of
/// `manifest_content` recorded — allowing the frozen state to be constructed.
fn deploy(dest: &Path, stem: &str, file_content: &str, manifest_content: &str) {
    let dir = dest.join(stem);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), file_content).unwrap();

    let mut manifest = SkillManifest::load(dest);
    manifest.managed.insert(
        stem.to_string(),
        SkillManifestEntry {
            checksum: trusty_agents_common::agents::manifest::checksum(manifest_content),
            deployed_at: "2026-07-29T00:00:00Z".to_string(),
        },
    );
    manifest.save(dest).unwrap();
}

/// A `FrameworkPaths` rooted entirely under one temp dir, with no submodule.
fn paths_under(tmp: &TempDir) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(tmp.path());
    paths.trusty_mpm_root = None;
    paths
}

#[test]
fn staleness_ok_when_every_tier_matches() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    deploy(&paths.claude_skills_dir(), "tm-doctor", "v2", "v2");

    let check = report(&reference_of(&[("tm-doctor", "v2")]), &paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

/// The #4604 headline: a drifted skill the old check reported clean.
#[test]
fn staleness_catches_drift_the_stale_cache_hid() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    // Deployed 07-29; manifest agrees with the file, as it did in the incident.
    deploy(
        &paths.claude_skills_dir(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        "v1 mentions PM_INSTRUCTIONS.md",
    );

    let check = report(
        &reference_of(&[("tm-workflow", "v2 describes the JSON manifest")]),
        &paths,
        None,
    );
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("tm-workflow"), "{}", check.message);
    assert!(check.message.contains("REPAIRABLE"), "{}", check.message);
}

#[test]
fn staleness_escalates_when_conventions_skill_drifts() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    deploy(&paths.claude_skills_dir(), "tm-pr-workflow", "v1", "v1");

    let check = report(&reference_of(&[("tm-pr-workflow", "v2")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "message: {}",
        check.message
    );
    assert!(
        check.message.contains("tm-pr-workflow"),
        "{}",
        check.message
    );
    assert!(
        check.message.to_lowercase().contains("convention"),
        "{}",
        check.message
    );
}

#[test]
fn staleness_warns_when_ordinary_skill_drifts() {
    // A conventions-bearing skill is deployed and FRESH here; only the ordinary
    // one drifted. The escalation must not leak onto it.
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(&dest, "tm-doctor", "v1", "v1");
    deploy(&dest, "tm-pr-workflow", "v2", "v2");

    let check = report(
        &reference_of(&[("tm-doctor", "v2"), ("tm-pr-workflow", "v2")]),
        &paths,
        None,
    );
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("tm-doctor"), "{}", check.message);
}

/// A hand-edited (frozen) skill must be reported SEPARATELY, because
/// `tm install` will deliberately skip it — the owner's explicit requirement.
#[test]
fn staleness_reports_frozen_separately_from_repairable_drift() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    deploy(
        &paths.claude_skills_dir(),
        "tm-doctor",
        "hand-edited",
        "what tm wrote",
    );

    let check = report(&reference_of(&[("tm-doctor", "v2")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("FROZEN"), "{}", check.message);
    assert!(
        check.message.contains("--fix-skills"),
        "the frozen report must name the opt-in remedy: {}",
        check.message
    );
    assert!(
        !check.message.contains("REPAIRABLE"),
        "a frozen-only finding must not claim `tm install` repairs it: {}",
        check.message
    );
}

/// UNVERIFIABLE MUST REPORT UNKNOWN, NEVER OK.
#[test]
fn staleness_is_unknown_when_a_skill_cannot_be_verified() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    // Managed at this tier, but this binary ships no such skill.
    deploy(&paths.claude_skills_dir(), "my-custom-skill", "x", "x");

    let check = report(&reference_of(&[("tm-doctor", "v2")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Unknown,
        "message: {}",
        check.message
    );
    assert!(
        check.message.contains("could NOT be verified"),
        "{}",
        check.message
    );
}

#[test]
fn staleness_is_unknown_with_no_reference_assets() {
    // Nothing to compare against is not a clean bill of health.
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let check = report(&reference_of(&[]), &paths, None);
    assert_eq!(check.status, CheckStatus::Unknown);
}

#[test]
fn staleness_reports_a_missing_deployed_file() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    deploy(&dest, "tm-doctor", "v2", "v2");
    fs::remove_file(dest.join("tm-doctor").join("SKILL.md")).unwrap();

    let check = report(&reference_of(&[("tm-doctor", "v2")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("ABSENT"), "{}", check.message);
}

/// The check must cover EVERY deploy tier, not just the one it is invoked
/// from — the tiers are where #4604 was measured (two of three drifted).
#[test]
fn staleness_covers_the_managed_config_tier() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let managed = paths.agent_deploy_dir().parent().unwrap().join("skills");
    deploy(&managed, "tm-workflow", "v1", "v1");

    let check = report(&reference_of(&[("tm-workflow", "v2")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(
        check.message.contains("managed config/tm-workflow"),
        "the managed-config tier must be named: {}",
        check.message
    );
}

#[test]
fn staleness_message_is_bounded() {
    // Ten drifted skills must not produce a ten-entry message.
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    let stems: Vec<String> = (0..10).map(|i| format!("skill-{i}")).collect();
    for stem in &stems {
        deploy(&dest, stem, "v1", "v1");
    }
    let pairs: Vec<(&str, &str)> = stems.iter().map(|s| (s.as_str(), "v2")).collect();

    let check = report(&reference_of(&pairs), &paths, None);
    assert!(check.message.contains("+5 more"), "{}", check.message);
}
