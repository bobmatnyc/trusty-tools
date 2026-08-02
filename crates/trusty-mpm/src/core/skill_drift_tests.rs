//! Tests for [`super`] — the #4604 skill-drift audit.
//!
//! Why: the defect these cover is a check that reported CLEAN for a genuinely
//! drifted skill, so the headline case (`stale_cache_no_longer_hides_drift`)
//! reconstructs the exact 2026-08-01 conditions — a cache byte-identical to the
//! deployed manifest checksum while the binary's embedded asset had moved on —
//! and asserts the audit now catches it.
//! What: reference-construction cases, then one case per [`SkillDrift`] state.
//! Test: this file.

use super::*;
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};
use std::fs;
use tempfile::TempDir;

/// Build a reference map from literal (stem, content) pairs.
fn reference_of(pairs: &[(&str, &str)]) -> SkillReference {
    SkillReference {
        assets: pairs
            .iter()
            .map(|(s, c)| (s.to_string(), c.to_string()))
            .collect(),
        origin: "this binary's embedded bundled assets".to_string(),
    }
}

/// Write a deployed skill at `<dest>/<stem>/SKILL.md` and record it in the
/// manifest with the checksum of `manifest_content` (which is what tm WROTE —
/// deliberately allowed to differ from `file_content` so the frozen state can
/// be constructed).
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

#[test]
fn reference_falls_back_to_embedded() {
    // With no submodule, the reference is the compiled-in table — never the
    // `~/.trusty-mpm/framework/skills` extraction cache.
    let reference = skill_reference(None);
    assert!(
        reference.origin.contains("embedded"),
        "origin was: {}",
        reference.origin
    );
    assert!(
        reference.assets.contains_key("tm-workflow"),
        "the binary must embed tm-workflow"
    );
}

#[test]
fn reference_prefers_the_submodule() {
    // A populated, git-tracked `agents/skills` checkout is authoritative on its
    // own (see core::skill_source) and outranks the embedded table.
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("tm-workflow.md"), "submodule text").unwrap();
    let reference = skill_reference(Some(tmp.path()));
    assert_eq!(
        reference.assets.get("tm-workflow").unwrap(),
        "submodule text"
    );
    assert!(
        reference.origin.contains("submodule"),
        "{}",
        reference.origin
    );
}

#[test]
fn reference_excludes_nested_reference_files() {
    // `<stem>/references/<file>.md` artifacts are not manifest-keyed skills;
    // including them would invent stems no manifest can ever match.
    let reference = skill_reference(None);
    assert!(
        !reference.assets.keys().any(|k| k.contains('/')),
        "nested reference artifacts leaked into the skill reference"
    );
}

#[test]
fn empty_submodule_dir_falls_back_to_embedded() {
    // An unpopulated submodule must not produce an EMPTY reference — that
    // would make every deployed skill unverifiable at once.
    let tmp = TempDir::new().unwrap();
    let reference = skill_reference(Some(tmp.path()));
    assert!(
        reference.origin.contains("embedded"),
        "{}",
        reference.origin
    );
}

/// THE #4604 REGRESSION PROOF.
///
/// Reconstructs the measured 2026-08-01 state: the extraction cache and the
/// deployed manifest checksum are byte-identical (both `v1`), while the running
/// binary's embedded asset is `v2`. The old check compared cache-vs-manifest —
/// two copies of `v1` — and reported no drift. The audit compares the DEPLOYED
/// FILE against the BINARY'S asset and catches it.
#[test]
fn stale_cache_no_longer_hides_drift() {
    let dest = TempDir::new().unwrap();
    // Deployed on 07-29 from the then-current source; manifest records the same
    // bytes, so the manifest and the deployed file agree — exactly the state a
    // stale cache also agreed with.
    deploy(
        dest.path(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        "v1 mentions PM_INSTRUCTIONS.md",
    );

    // The binary now embeds the post-#4583 text.
    let reference = reference_of(&[("tm-workflow", "v2 describes the JSON manifest")]);
    let findings = audit_deployed_skills(&reference, dest.path());

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].stem, "tm-workflow");
    assert_eq!(
        findings[0].state,
        SkillDrift::Drifted,
        "the drifted skill the old check reported clean must now be caught"
    );
}

/// SIDE-BY-SIDE MUTATION PROOF: the OLD comparison misses what the new one
/// catches, on byte-identical inputs.
///
/// Why: #4604's complaint is not that the check was noisy — it is that the
/// check reported CLEAN for a drifted skill, which reads as an all-clear. This
/// runs the pre-#4604 comparison (`skill_staleness::stale_skills`, source cache
/// vs deploy manifest — still in the tree, used by the launch path for a
/// different question) and the new audit against the SAME on-disk state, and
/// pins that only the new one sees the drift.
/// What: reconstructs the measured 2026-08-01 state — the extraction cache and
/// the deployed manifest checksum are both `v1`, while the binary embeds `v2`.
/// Test: this test.
#[test]
fn the_old_cache_comparison_reports_clean_on_the_same_inputs() {
    let dest = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    // The deployed copy and the manifest agree (the deploy was consistent).
    deploy(
        dest.path(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        "v1 mentions PM_INSTRUCTIONS.md",
    );
    // The extraction cache never refreshed after the binary shipped `v2`, so it
    // still holds `v1` — byte-identical to the recorded checksum.
    fs::write(
        cache.path().join("tm-workflow.md"),
        "v1 mentions PM_INSTRUCTIONS.md",
    )
    .unwrap();

    // OLD: compares the cache against the manifest — two copies of `v1`.
    let old = crate::core::skill_staleness::stale_skills(cache.path(), dest.path());
    println!("OLD (cache vs manifest)      -> {old:?}");

    // NEW: compares the deployed FILE against the BINARY's embedded asset.
    let reference = reference_of(&[("tm-workflow", "v2 describes the JSON manifest")]);
    let new = audit_deployed_skills(&reference, dest.path());
    println!("NEW (deployed file vs binary) -> {new:?}");

    assert!(
        old.is_empty(),
        "the pre-#4604 comparison is expected to MISS this drift; if it no longer \
         does, this proof needs rewriting: {old:?}"
    );
    assert_eq!(
        new[0].state,
        SkillDrift::Drifted,
        "the new audit must catch what the old comparison missed"
    );
}

/// The control for the proof above: identical content reports clean.
#[test]
fn matching_deploy_is_fresh() {
    let dest = TempDir::new().unwrap();
    deploy(dest.path(), "tm-workflow", "v2", "v2");
    let reference = reference_of(&[("tm-workflow", "v2")]);
    let findings = audit_deployed_skills(&reference, dest.path());
    assert_eq!(findings[0].state, SkillDrift::Fresh);
    assert!(!findings[0].state.is_problem());
}

#[test]
fn hand_edited_deploy_is_frozen_not_merely_drifted() {
    // The deployed file differs from BOTH the reference and the checksum tm
    // recorded, so `deploy_skills` will skip it forever. That is a distinct
    // state from ordinary drift and must be reported as such — the owner's
    // explicit requirement on #4604.
    let dest = TempDir::new().unwrap();
    deploy(
        dest.path(),
        "tm-pr-workflow",
        "hand-edited by the operator",
        "v1 as deployed",
    );
    let reference = reference_of(&[("tm-pr-workflow", "v2 from the binary")]);
    let findings = audit_deployed_skills(&reference, dest.path());
    assert_eq!(findings[0].state, SkillDrift::DriftedFrozen);
}

#[test]
fn missing_deployed_file_is_reported() {
    // The manifest claims tm deployed it; the file is gone.
    let dest = TempDir::new().unwrap();
    deploy(dest.path(), "tm-doctor", "v1", "v1");
    fs::remove_file(dest.path().join("tm-doctor").join("SKILL.md")).unwrap();
    let reference = reference_of(&[("tm-doctor", "v1")]);
    let findings = audit_deployed_skills(&reference, dest.path());
    assert_eq!(findings[0].state, SkillDrift::Missing);
}

#[test]
fn skill_absent_from_the_binary_is_unverifiable_not_fresh() {
    // A user-tier or renamed skill this binary does not ship cannot be
    // compared. UNKNOWN, never OK — the generalising rule of #4469/#4033/#4604.
    let dest = TempDir::new().unwrap();
    deploy(dest.path(), "my-custom-skill", "anything", "anything");
    let reference = reference_of(&[("tm-workflow", "v2")]);
    let findings = audit_deployed_skills(&reference, dest.path());
    assert!(
        matches!(findings[0].state, SkillDrift::Unverifiable(_)),
        "got {:?}",
        findings[0].state
    );
    assert!(findings[0].state.is_problem());
}

#[test]
fn nothing_deployed_yields_no_findings() {
    // An empty manifest is a real answer: there is no prior deploy to be stale
    // relative to.
    let dest = TempDir::new().unwrap();
    let reference = reference_of(&[("tm-workflow", "v2")]);
    assert!(audit_deployed_skills(&reference, dest.path()).is_empty());
}

#[test]
fn drift_states_report_not_fresh() {
    assert!(!SkillDrift::Fresh.is_problem());
    assert!(SkillDrift::Drifted.is_problem());
    assert!(SkillDrift::DriftedFrozen.is_problem());
    assert!(SkillDrift::Missing.is_problem());
    assert!(SkillDrift::Unverifiable("x".into()).is_problem());
}
