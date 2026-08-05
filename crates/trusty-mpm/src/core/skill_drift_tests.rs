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
use crate::core::skill_deployer::deploy_skills;
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

/// Deploy `stem` into `dest` THROUGH THE REAL DEPLOYER.
///
/// Why (#4622 review, HIGH-1): the previous helper hand-wrote bare-stem manifest
/// keys, which is a manifest shape production never produces —
/// `skills::deployer::deploy_skills` also records every `references/*.md`
/// sibling under a NESTED `<stem>/references/<file>.md` key (measured: 132 keys
/// on this machine's `~/.claude/skills` manifest, 80 of them nested). A fixture
/// that cannot contain a nested key cannot catch a bug about nested keys, and
/// that is exactly the defect class this PR exists to close. Everything now goes
/// through the real deploy path, so the manifest under test is the manifest
/// production writes.
/// What: writes `<src>/<stem>.md` plus one `<src>/<stem>/references/<ref>.md`
/// sibling when `reference` is `Some`, runs `deploy_skills`, and returns the
/// source dir (kept alive so the caller can mutate it).
/// Test: every test in this file.
fn deploy_real(dest: &Path, stem: &str, body: &str, reference: Option<(&str, &str)>) -> TempDir {
    let src = TempDir::new().unwrap();
    fs::write(src.path().join(format!("{stem}.md")), body).unwrap();
    if let Some((ref_name, ref_body)) = reference {
        let refs = src.path().join(stem).join("references");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join(ref_name), ref_body).unwrap();
    }
    deploy_skills(src.path(), dest).unwrap();
    src
}

/// Hand-edit a deployed file AFTER a real deploy, leaving the manifest checksum
/// describing what tm actually wrote — the only honest way to construct the
/// FROZEN state.
fn hand_edit(dest: &Path, manifest_key: &str, new_content: &str) {
    let path = deployed_path(dest, manifest_key);
    fs::write(path, new_content).unwrap();
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

/// #4622 review HIGH-1: the reference must be keyed by MANIFEST KEY, which for a
/// multi-file skill's reference sibling is `<stem>/references/<file>.md`.
///
/// Why: `deploy_skills` records those nested keys, and `audit_deployed_skills`
/// iterates every manifest key. A reference map that only holds bare stems makes
/// every nested key unverifiable, which pins `skill_staleness` to Unknown on any
/// real install. On this machine that is 80 of 132 keys.
/// Test: this test.
#[test]
fn reference_includes_nested_reference_keys() {
    let reference = skill_reference(None);
    let nested: Vec<&String> = reference
        .assets
        .keys()
        .filter(|k| k.contains('/'))
        .collect();
    assert!(
        !nested.is_empty(),
        "the embedded reference must carry `<stem>/references/<file>.md` keys; got only bare stems"
    );
    assert!(
        nested
            .iter()
            .all(|k| k.contains("/references/") && k.ends_with(".md")),
        "nested keys must match the deployer's key format, got: {nested:?}"
    );
}

/// END-TO-END #4622 HIGH-1 PROOF: a REAL bundled deploy, audited against the
/// REAL embedded reference, must be entirely Fresh.
///
/// Why: this is the critic's measured claim reduced to a hermetic test. The
/// bundled source is materialised from `bundle::ALL` exactly as
/// `core::skill_source` does, deployed by the real `deploy_skills` (which writes
/// the nested `<stem>/references/<file>.md` manifest keys), then audited against
/// `skill_reference(None)`. Before the fix, every nested key missed the
/// reference map and became `Unverifiable`, so a pristine install could never
/// report Ok — `skill_staleness` was pinned to Unknown permanently.
/// Test: this test.
#[test]
fn a_pristine_bundled_deploy_is_entirely_fresh() {
    let src = TempDir::new().unwrap();
    // Materialise the bundled skills exactly as `skill_source` does: flat
    // `<stem>.md` entry points plus nested `<stem>/references/<file>.md`.
    for artifact in crate::core::bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
    {
        let rel = artifact.rel_path.strip_prefix("skills/").unwrap();
        let path = src.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, artifact.contents).unwrap();
    }

    let dest = TempDir::new().unwrap();
    deploy_skills(src.path(), dest.path()).unwrap();

    let audit = audit_deployed_skills(&skill_reference(None), dest.path());
    assert_eq!(audit.manifest, ManifestState::Present);

    let nested = audit
        .findings
        .iter()
        .filter(|f| f.stem.contains('/'))
        .count();
    assert!(
        nested > 0,
        "the fixture must contain nested reference keys or it cannot prove anything"
    );

    let problems: Vec<&SkillDriftFinding> = audit
        .findings
        .iter()
        .filter(|f| f.state.is_problem())
        .collect();
    assert!(
        problems.is_empty(),
        "a pristine bundled deploy must be entirely Fresh ({} findings, {nested} nested); \
         unresolved: {problems:?}",
        audit.findings.len()
    );
}

#[test]
fn reference_prefers_the_submodule() {
    // A populated, git-tracked `agents/skills` checkout is authoritative on its
    // own (see core::skill_source) and outranks the embedded table.
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("tm-workflow.md"), "submodule text").unwrap();
    let refs = tmp.path().join("tm-workflow").join("references");
    fs::create_dir_all(&refs).unwrap();
    fs::write(refs.join("extra.md"), "submodule reference").unwrap();

    let reference = skill_reference(Some(tmp.path()));
    assert_eq!(
        reference.assets.get("tm-workflow").unwrap(),
        "submodule text"
    );
    assert_eq!(
        reference
            .assets
            .get("tm-workflow/references/extra.md")
            .unwrap(),
        "submodule reference",
        "the submodule reference must also carry nested keys"
    );
    assert!(
        reference.origin.contains("submodule"),
        "{}",
        reference.origin
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

#[test]
fn deployed_path_matches_the_deployer_layout() {
    // A bare stem's entry point is `<stem>/SKILL.md`; a nested reference key
    // mirrors the source layout verbatim. Both must agree with
    // `skills::deployer::deploy_skills`, which is what writes them.
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-doctor", "v1", Some(("guide.md", "ref v1")));
    assert!(deployed_path(dest.path(), "tm-doctor").exists());
    assert!(deployed_path(dest.path(), "tm-doctor/references/guide.md").exists());
    assert_eq!(
        deployed_path(dest.path(), "tm-doctor"),
        dest.path().join("tm-doctor").join("SKILL.md")
    );
}

/// THE #4604 REGRESSION PROOF, now through the real deployer.
///
/// Reconstructs the measured 2026-08-01 state: the extraction cache and the
/// deployed manifest checksum are byte-identical (both `v1`), while the running
/// binary's embedded asset is `v2`. The old check compared cache-vs-manifest —
/// two copies of `v1` — and reported no drift.
#[test]
fn stale_cache_no_longer_hides_drift() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(
        dest.path(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        None,
    );

    // The binary now embeds the post-#4583 text.
    let reference = reference_of(&[("tm-workflow", "v2 describes the JSON manifest")]);
    let audit = audit_deployed_skills(&reference, dest.path());

    assert_eq!(audit.findings.len(), 1);
    assert_eq!(audit.findings[0].stem, "tm-workflow");
    assert_eq!(
        audit.findings[0].state,
        SkillDrift::Drifted,
        "the drifted skill the old check reported clean must now be caught"
    );
}

/// SIDE-BY-SIDE MUTATION PROOF: the OLD comparison misses what the new one
/// catches, on byte-identical inputs.
#[test]
fn the_old_cache_comparison_reports_clean_on_the_same_inputs() {
    let dest = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let _src = deploy_real(
        dest.path(),
        "tm-workflow",
        "v1 mentions PM_INSTRUCTIONS.md",
        None,
    );
    // The extraction cache never refreshed after the binary shipped `v2`, so it
    // still holds `v1` — byte-identical to the recorded checksum.
    fs::write(
        cache.path().join("tm-workflow.md"),
        "v1 mentions PM_INSTRUCTIONS.md",
    )
    .unwrap();

    let old = crate::core::skill_staleness::stale_skills(cache.path(), dest.path());
    println!("OLD (cache vs manifest)       -> {old:?}");

    let reference = reference_of(&[("tm-workflow", "v2 describes the JSON manifest")]);
    let new = audit_deployed_skills(&reference, dest.path());
    println!("NEW (deployed file vs binary) -> {:?}", new.findings);

    assert!(
        old.is_empty(),
        "the pre-#4604 comparison is expected to MISS this drift; if it no longer \
         does, this proof needs rewriting: {old:?}"
    );
    assert_eq!(new.findings[0].state, SkillDrift::Drifted);
}

/// THE #4622 HIGH-1 PROOF: a real deploy with reference files must report Ok.
///
/// Why: `deploy_skills` writes nested `<stem>/references/<file>.md` manifest
/// keys. Before the fix every one of them missed the reference map and became
/// `Unverifiable`, so a perfectly fresh install could never report Ok.
/// Test: this test.
#[test]
fn a_real_deploy_with_reference_files_is_fresh() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(
        dest.path(),
        "documentation-style",
        "entry v1",
        Some(("spec.md", "reference v1")),
    );

    let reference = reference_of(&[
        ("documentation-style", "entry v1"),
        ("documentation-style/references/spec.md", "reference v1"),
    ]);
    let audit = audit_deployed_skills(&reference, dest.path());

    assert_eq!(
        audit.findings.len(),
        2,
        "both the entry point and its reference sibling must be audited: {:?}",
        audit.findings
    );
    assert!(
        audit.findings.iter().all(|f| f.state == SkillDrift::Fresh),
        "a freshly deployed multi-file skill must be entirely Fresh, got: {:?}",
        audit.findings
    );
}

/// And drift in a REFERENCE FILE alone must still be caught.
#[test]
fn drift_in_a_reference_file_is_caught() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(
        dest.path(),
        "documentation-style",
        "entry v1",
        Some(("spec.md", "reference v1")),
    );

    // Only the reference sibling moved on in the binary.
    let reference = reference_of(&[
        ("documentation-style", "entry v1"),
        ("documentation-style/references/spec.md", "reference v2"),
    ]);
    let audit = audit_deployed_skills(&reference, dest.path());

    let drifted: Vec<&SkillDriftFinding> = audit
        .findings
        .iter()
        .filter(|f| f.state == SkillDrift::Drifted)
        .collect();
    assert_eq!(drifted.len(), 1, "findings: {:?}", audit.findings);
    assert_eq!(drifted[0].stem, "documentation-style/references/spec.md");
}

/// The control: identical content reports clean.
#[test]
fn matching_deploy_is_fresh() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-workflow", "v2", None);
    let reference = reference_of(&[("tm-workflow", "v2")]);
    let audit = audit_deployed_skills(&reference, dest.path());
    assert_eq!(audit.findings[0].state, SkillDrift::Fresh);
    assert!(!audit.findings[0].state.is_problem());
}

#[test]
fn hand_edited_deploy_is_frozen_not_merely_drifted() {
    // The deployed file differs from BOTH the reference and the checksum tm
    // recorded, so `deploy_skills` will skip it forever. That is a distinct
    // state from ordinary drift and must be reported as such.
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-pr-workflow", "v1 as deployed", None);
    hand_edit(dest.path(), "tm-pr-workflow", "hand-edited by the operator");

    let reference = reference_of(&[("tm-pr-workflow", "v2 from the binary")]);
    let audit = audit_deployed_skills(&reference, dest.path());
    assert_eq!(audit.findings[0].state, SkillDrift::DriftedFrozen);
}

#[test]
fn missing_deployed_file_is_reported() {
    // The manifest claims tm deployed it; the file is gone.
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-doctor", "v1", None);
    fs::remove_file(deployed_path(dest.path(), "tm-doctor")).unwrap();

    let reference = reference_of(&[("tm-doctor", "v1")]);
    let audit = audit_deployed_skills(&reference, dest.path());
    assert_eq!(audit.findings[0].state, SkillDrift::Missing);
}

#[test]
fn skill_absent_from_the_binary_is_unverifiable_not_fresh() {
    // A user-tier or renamed skill this binary does not ship cannot be
    // compared. UNKNOWN, never OK.
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "my-custom-skill", "anything", None);
    let reference = reference_of(&[("tm-workflow", "v2")]);
    let audit = audit_deployed_skills(&reference, dest.path());
    assert!(
        matches!(audit.findings[0].state, SkillDrift::Unverifiable(_)),
        "got {:?}",
        audit.findings[0].state
    );
    assert!(audit.findings[0].state.is_problem());
}

#[test]
fn nothing_deployed_yields_no_findings() {
    // An untouched directory is a real answer: there is no prior deploy to be
    // stale relative to, and no manifest is expected.
    let dest = TempDir::new().unwrap();
    let reference = reference_of(&[("tm-workflow", "v2")]);
    let audit = audit_deployed_skills(&reference, dest.path());
    assert!(audit.findings.is_empty());
    assert_eq!(audit.manifest, ManifestState::AbsentAndEmpty);
}

/// #4622 review MEDIUM: an absent manifest over a POPULATED tier is not clean.
#[test]
fn absent_manifest_over_deployed_skills_is_unknown() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-workflow", "v1", None);
    fs::remove_file(
        dest.path()
            .join(crate::core::skill_manifest::SKILL_MANIFEST_FILE),
    )
    .unwrap();

    let audit = audit_deployed_skills(&reference_of(&[("tm-workflow", "v1")]), dest.path());
    assert_eq!(
        audit.manifest,
        ManifestState::AbsentButPopulated,
        "skills on disk with no ownership ledger cannot be verified"
    );
}

/// #4622 review MEDIUM: a corrupt manifest is not an empty one.
#[test]
fn corrupt_manifest_is_unknown_not_empty() {
    let dest = TempDir::new().unwrap();
    let _src = deploy_real(dest.path(), "tm-workflow", "v1", None);
    fs::write(
        dest.path()
            .join(crate::core::skill_manifest::SKILL_MANIFEST_FILE),
        "{ not json",
    )
    .unwrap();

    let audit = audit_deployed_skills(&reference_of(&[("tm-workflow", "v1")]), dest.path());
    assert!(
        matches!(audit.manifest, ManifestState::Unreadable(_)),
        "got {:?}",
        audit.manifest
    );
    assert!(audit.findings.is_empty());
}

#[test]
fn drift_states_report_not_fresh() {
    assert!(!SkillDrift::Fresh.is_problem());
    assert!(SkillDrift::Drifted.is_problem());
    assert!(SkillDrift::DriftedFrozen.is_problem());
    assert!(SkillDrift::Missing.is_problem());
    assert!(SkillDrift::Unverifiable("x".into()).is_problem());
}

/// #4881: the ledger lock's sidecar is bookkeeping, not deployed content.
///
/// Why: `with_skill_manifest_lock` creates
/// `.trusty-mpm-skills-manifest.json.lock` when the lock is TAKEN, which is
/// before anything is written. Counting it as content would report a tier that
/// was never deployed to as unattributable, and `tm doctor` renders
/// `AbsentButPopulated` as unverifiable — a false alarm on an empty tier.
#[test]
fn a_lock_sidecar_alone_is_not_a_populated_tier() {
    let dest = TempDir::new().unwrap();
    fs::write(
        crate::core::skill_manifest::skill_manifest_lock_path(dest.path()),
        "",
    )
    .unwrap();

    let audit = audit_deployed_skills(&reference_of(&[("tm-workflow", "v1")]), dest.path());
    assert_eq!(
        audit.manifest,
        ManifestState::AbsentAndEmpty,
        "a tier holding only the lock sidecar was never deployed to"
    );
}
