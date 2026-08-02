//! Tests for [`super`] — the #4604 `skill_staleness` severity table.
//!
//! Why: the check's job is to be believed, so every verdict it can render needs
//! a case that mutates exactly one input and proves the verdict moves with it —
//! including the two the pre-#4604 version could not produce at all (`Unknown`
//! for unverifiable, and the separate FROZEN report).
//! What: one test per severity branch plus the message-bounding case.
//! Test: this file.

use super::*;
use crate::core::skill_deployer::deploy_skills;
use crate::core::skill_drift::{SkillReference, deployed_path};
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

/// Deploy `stem` into `dest` THROUGH THE REAL DEPLOYER (#4622 review, HIGH-1).
///
/// Why: a hand-written manifest cannot contain the nested
/// `<stem>/references/<file>.md` keys production writes, so a fixture built that
/// way cannot exercise them. Everything here goes through `deploy_skills`.
/// What: writes the source tree, deploys it, returns the source dir.
/// Test: every test in this file.
fn deploy_real(dest: &Path, stem: &str, body: &str, reference: Option<(&str, &str)>) -> TempDir {
    let src = TempDir::new().unwrap();
    fs::create_dir_all(dest).unwrap();
    fs::write(src.path().join(format!("{stem}.md")), body).unwrap();
    if let Some((ref_name, ref_body)) = reference {
        let refs = src.path().join(stem).join("references");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join(ref_name), ref_body).unwrap();
    }
    deploy_skills(src.path(), dest).unwrap();
    src
}

/// Hand-edit a deployed file after a real deploy — constructs the FROZEN state.
fn hand_edit(dest: &Path, manifest_key: &str, new_content: &str) {
    fs::write(deployed_path(dest, manifest_key), new_content).unwrap();
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
    let _src = deploy_real(&paths.claude_skills_dir(), "tm-doctor", "v2", None);

    let check = report(&reference_of(&[("tm-doctor", "v2")]), &paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

/// The #4604 headline: a drifted skill the old check reported clean.
#[test]
fn staleness_catches_drift_the_stale_cache_hid() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    // Deployed 07-29; manifest agrees with the file, as it did in the incident.
    let _src = deploy_real(
        &paths.claude_skills_dir(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        None,
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
    let _src = deploy_real(&paths.claude_skills_dir(), "tm-pr-workflow", "v1", None);

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
    let _a = deploy_real(&dest, "tm-doctor", "v1", None);
    let _b = deploy_real(&dest, "tm-pr-workflow", "v2", None);

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
    let _src = deploy_real(
        &paths.claude_skills_dir(),
        "tm-doctor",
        "what tm wrote",
        None,
    );
    hand_edit(&paths.claude_skills_dir(), "tm-doctor", "hand-edited");

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
    let _src = deploy_real(&paths.claude_skills_dir(), "my-custom-skill", "x", None);

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
    let _src = deploy_real(&dest, "tm-doctor", "v2", None);
    fs::remove_file(deployed_path(&dest, "tm-doctor")).unwrap();

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
    let _src = deploy_real(&managed, "tm-workflow", "v1", None);

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
    let _srcs: Vec<TempDir> = stems
        .iter()
        .map(|stem| deploy_real(&dest, stem, "v1", None))
        .collect();
    let pairs: Vec<(&str, &str)> = stems.iter().map(|s| (s.as_str(), "v2")).collect();

    let check = report(&reference_of(&pairs), &paths, None);
    assert!(check.message.contains("+5 more"), "{}", check.message);
}

/// #4622 review HIGH-1 at the CHECK level: a pristine multi-file deploy is Ok.
///
/// Why: `deploy_skills` records nested `<stem>/references/<file>.md` manifest
/// keys, and before the fix every one of them was `Unverifiable` — so the check
/// could never report Ok on any real install, no matter how clean it was.
/// Test: this test.
#[test]
fn staleness_is_ok_for_a_pristine_multi_file_deploy() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let _src = deploy_real(
        &paths.claude_skills_dir(),
        "tm-doctor",
        "entry v1",
        Some(("guide.md", "reference v1")),
    );

    let check = report(
        &reference_of(&[
            ("tm-doctor", "entry v1"),
            ("tm-doctor/references/guide.md", "reference v1"),
        ]),
        &paths,
        None,
    );
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

/// A drifted reference file OF A CONVENTIONS-BEARING SKILL must still escalate.
///
/// Why: the escalation is stated per SKILL, but the manifest key names the
/// reference file. Matching the raw key would silently downgrade a lapsed
/// convention carried in `documentation-style/references/method-function.md` to
/// an ordinary warning.
/// Test: this test.
#[test]
fn staleness_escalates_when_a_reference_file_of_a_conventions_skill_drifts() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let _src = deploy_real(
        &paths.claude_skills_dir(),
        "documentation-style",
        "entry v1",
        Some(("method-function.md", "reference v1")),
    );

    let check = report(
        &reference_of(&[
            ("documentation-style", "entry v1"),
            (
                "documentation-style/references/method-function.md",
                "reference v2",
            ),
        ]),
        &paths,
        None,
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "message: {}",
        check.message
    );
    assert!(
        check.message.contains("method-function.md"),
        "{}",
        check.message
    );
}

/// #4622 review MEDIUM: an unreadable ownership ledger is UNKNOWN, not Ok.
#[test]
fn staleness_is_unknown_when_a_tier_ledger_is_corrupt() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    let _src = deploy_real(&dest, "tm-doctor", "v1", None);
    fs::write(
        dest.join(crate::core::skill_manifest::SKILL_MANIFEST_FILE),
        "{ not json",
    )
    .unwrap();

    let check = report(&reference_of(&[("tm-doctor", "v1")]), &paths, None);
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

/// #4622 review MEDIUM: skills on disk with no ledger at all is UNKNOWN.
#[test]
fn staleness_is_unknown_when_a_tier_has_no_ledger() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    let _src = deploy_real(&dest, "tm-doctor", "v1", None);
    fs::remove_file(dest.join(crate::core::skill_manifest::SKILL_MANIFEST_FILE)).unwrap();

    let check = report(&reference_of(&[("tm-doctor", "v1")]), &paths, None);
    assert_eq!(
        check.status,
        CheckStatus::Unknown,
        "message: {}",
        check.message
    );
}
