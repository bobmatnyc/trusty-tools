//! Tests for [`super`] — the #5224 retired-skill sweep.
//!
//! Why: this module DELETES files from an operator's deployed tree, so the
//! tier-safety guarantees are the tests that matter most: a user-tier skill and
//! a project-tier skill must survive a sweep that removes a genuinely retired
//! one, and a hand-edited copy must never be deleted.
//! What: fixtures go through the REAL deployer, never a hand-written ledger, so
//! a test cannot pass against a manifest shape production does not write.
//! Test: this file.

use super::*;
use crate::core::skill_deployer::deploy_skills;
use crate::core::skill_tiers::deploy_all_skill_tiers;
use std::fs;
use tempfile::TempDir;

/// Deploy `stem` into `dest` through the real deployer, optionally carrying one
/// `references/<name>` sibling so the nested manifest-key shape is exercised.
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

/// The live set as a `BTreeSet`, from string literals.
fn live(stems: &[&str]) -> BTreeSet<String> {
    stems.iter().map(|s| s.to_string()).collect()
}

/// A `FrameworkPaths` rooted entirely under one temp dir, with no submodule.
fn paths_under(tmp: &TempDir) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(tmp.path());
    paths.trusty_mpm_root = None;
    paths
}

#[test]
fn bundled_stems_covers_a_known_skill() {
    // The compiled-in table is the reference this module trusts; if it ever
    // stops yielding stems, every deployed skill would look retired at once.
    let stems = bundled_skill_stems();
    assert!(!stems.is_empty());
    assert!(
        stems.contains("tm-workflow"),
        "expected a known bundled stem, got {} entries",
        stems.len()
    );
    // Nested `<stem>/references/<file>.md` artifacts must fold onto their stem,
    // never appear as a stem of their own.
    assert!(
        !stems.iter().any(|s| s.contains('/') || s.ends_with(".md")),
        "a nested artifact leaked in as its own stem: {stems:?}"
    );
}

#[test]
fn retire_removes_a_pristine_orphan() {
    // The #5224 headline: the binary stopped shipping `tm-pr-workflow`, so its
    // deployed directory and its ledger entry both go.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(
        &dest,
        "tm-pr-workflow",
        "the retired workflow skill",
        Some(("checklist.md", "a carried reference file")),
    );
    assert!(dest.join("tm-pr-workflow").join("SKILL.md").is_file());

    let retired = retire_orphans_in("operator home", &dest, &live(&["tm-workflow"])).unwrap();

    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(retired[0].stem, "tm-pr-workflow");
    assert!(retired[0].removed, "{retired:?}");
    assert!(!dest.join("tm-pr-workflow").exists());
    let manifest = SkillManifest::load_checked(&dest).unwrap();
    assert!(
        manifest
            .managed
            .keys()
            .all(|k| key_stem(k) != "tm-pr-workflow"),
        "the ledger still claims a skill nothing ships: {:?}",
        manifest.managed.keys().collect::<Vec<_>>()
    );
}

#[test]
fn retire_spares_a_user_tier_skill() {
    // TIER SAFETY: a skill the user authors in `~/.trusty-mpm/skills/` is
    // deployed by tm and therefore sits in the ledger with a perfectly matching
    // checksum, indistinguishable from a bundled one by content alone. The live
    // set is what tells them apart, and it must.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let bundled = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    fs::write(bundled.path().join("tm-pr-workflow.md"), "retired").unwrap();
    fs::write(user.path().join("my-own-skill.md"), "authored by me").unwrap();
    deploy_all_skill_tiers(bundled.path(), user.path(), &dest, |_| true).unwrap();
    assert!(dest.join("my-own-skill").join("SKILL.md").is_file());

    // The bundle retired `tm-pr-workflow`; the user tier still supplies
    // `my-own-skill`, so the live set contains it.
    let retired = retire_orphans_in("operator home", &dest, &live(&["my-own-skill"])).unwrap();

    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(retired[0].stem, "tm-pr-workflow");
    assert!(
        dest.join("my-own-skill").join("SKILL.md").is_file(),
        "a user-tier skill was removed"
    );
    assert!(SkillManifest::load_checked(&dest).unwrap().is_managed("my-own-skill"));
}

#[test]
fn retire_spares_a_project_tier_skill() {
    // TIER SAFETY: a skill hand-placed in the deploy target has NO ledger entry,
    // so it is not a candidate at all — the strongest of the three gates.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-pr-workflow", "retired", None);
    let hand_placed = dest.join("my-project-skill");
    fs::create_dir_all(&hand_placed).unwrap();
    fs::write(hand_placed.join("SKILL.md"), "hand-placed by the operator").unwrap();

    // Deliberately hostile live set: it names NEITHER skill, so only the ledger
    // gate can save the hand-placed one.
    let retired = retire_orphans_in("project", &dest, &live(&[])).unwrap();

    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(retired[0].stem, "tm-pr-workflow");
    assert!(
        hand_placed.join("SKILL.md").is_file(),
        "a hand-placed project-tier skill was removed"
    );
}

#[test]
fn retire_keeps_a_hand_edited_orphan_but_releases_the_ledger() {
    // DELIBERATE DESIGN (#5224): a retired skill the operator edited is theirs.
    // The files stay exactly where they are; only the ledger claim goes, because
    // a claim that tm can refresh the file is false once nothing ships it — and
    // that stale claim is what pins `skill_staleness` to Unknown.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-pr-workflow", "as shipped", None);
    let deployed = dest.join("tm-pr-workflow").join("SKILL.md");
    fs::write(&deployed, "as shipped, plus my own notes").unwrap();

    let retired = retire_orphans_in("operator home", &dest, &live(&[])).unwrap();

    assert_eq!(retired.len(), 1, "{retired:?}");
    assert!(!retired[0].removed, "{retired:?}");
    assert!(
        retired[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("edited after it was deployed")),
        "{retired:?}"
    );
    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "as shipped, plus my own notes",
        "an operator edit was destroyed"
    );
    assert!(
        !SkillManifest::load_checked(&dest).unwrap().is_managed("tm-pr-workflow"),
        "the ledger still claims a skill nothing ships"
    );
}

#[test]
fn retire_keeps_an_orphan_holding_an_untracked_file() {
    // `remove_dir_all` would take the operator's own file with it.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-pr-workflow", "as shipped", None);
    let stray = dest.join("tm-pr-workflow").join("my-notes.md");
    fs::write(&stray, "notes trusty-mpm never deployed").unwrap();

    let retired = retire_orphans_in("operator home", &dest, &live(&[])).unwrap();

    assert_eq!(retired.len(), 1, "{retired:?}");
    assert!(!retired[0].removed, "{retired:?}");
    assert!(stray.is_file(), "an untracked operator file was removed");
}

#[test]
fn retire_is_a_noop_when_nothing_is_orphaned() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-workflow", "current", None);
    let before =
        fs::read_to_string(dest.join(crate::core::skill_manifest::SKILL_MANIFEST_FILE)).unwrap();

    let retired = retire_orphans_in("operator home", &dest, &live(&["tm-workflow"])).unwrap();

    assert!(retired.is_empty(), "{retired:?}");
    assert!(dest.join("tm-workflow").join("SKILL.md").is_file());
    assert_eq!(
        fs::read_to_string(dest.join(crate::core::skill_manifest::SKILL_MANIFEST_FILE)).unwrap(),
        before,
        "a no-op sweep rewrote the ledger"
    );
}

#[test]
fn retire_does_not_touch_a_deselected_but_still_shipped_skill() {
    // Deselection is NOT retirement: `deploy_skills_filtered`'s documented HR-3
    // behavior ("deselecting a skill does not remove a previously deployed
    // copy") must survive this change. The live set is built from what sources
    // CONTAIN, never from what a manifest selects.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-workflow", "current", None);

    // `tm-workflow` is still shipped — merely excluded by some harness manifest.
    let retired = retire_orphans_in("operator home", &dest, &live(&["tm-workflow"])).unwrap();

    assert!(retired.is_empty(), "{retired:?}");
    assert!(dest.join("tm-workflow").join("SKILL.md").is_file());
}

#[test]
fn retire_missing_target_is_empty() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("never-deployed");
    assert!(
        retire_orphans_in("project", &missing, &live(&[]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn live_stems_include_the_user_tier() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    fs::create_dir_all(paths.user_skill_source_dir()).unwrap();
    fs::write(
        paths.user_skill_source_dir().join("my-own-skill.md"),
        "authored by me",
    )
    .unwrap();

    let stems = live_skill_stems(&paths, &paths.claude_skills_dir()).unwrap();

    assert!(stems.contains("my-own-skill"), "{stems:?}");
    // And the compiled-in bundle is always in there too.
    assert!(stems.contains("tm-workflow"), "{stems:?}");
}

#[test]
fn live_stems_include_project_custom_stems_in_the_target() {
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let dest = paths.claude_skills_dir();
    let hand_placed = dest.join("my-project-skill");
    fs::create_dir_all(&hand_placed).unwrap();
    fs::write(hand_placed.join("SKILL.md"), "hand-placed").unwrap();

    let stems = live_skill_stems(&paths, &dest).unwrap();
    assert!(stems.contains("my-project-skill"), "{stems:?}");
}

#[test]
fn live_stems_are_none_when_a_source_cannot_be_read() {
    // FAIL SAFE: an unreadable source means an INCOMPLETE live set, which would
    // misread live skills as retired. The sweep must be skipped, not narrowed.
    use std::os::unix::fs::PermissionsExt;
    if nix_running_as_root() {
        return; // root ignores the mode bits; the case is unreachable.
    }
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let user = paths.user_skill_source_dir();
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("my-own-skill.md"), "authored by me").unwrap();
    fs::set_permissions(&user, fs::Permissions::from_mode(0o000)).unwrap();

    let stems = live_skill_stems(&paths, &paths.claude_skills_dir());

    // Restore before asserting so a failure still leaves a removable temp dir.
    fs::set_permissions(&user, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        stems.is_none(),
        "an unreadable source must abort the sweep, not narrow the live set"
    );
}

/// Whether this process would bypass the mode bits the test above relies on.
fn nix_running_as_root() -> bool {
    // SAFETY: `getuid` is always safe — it takes no arguments, reads a process
    // property, and cannot fail.
    unsafe { libc::getuid() == 0 }
}

#[test]
fn live_stems_survive_a_missing_source_directory() {
    // An absent tier is not an error — most machines have no user-custom tier
    // and no catalog checkout at all, and the sweep must still run for them.
    let tmp = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    assert!(!paths.user_skill_source_dir().exists());

    let stems = live_skill_stems(&paths, &paths.claude_skills_dir())
        .expect("a missing source tier must not abort the sweep");
    assert!(stems.contains("tm-workflow"), "{stems:?}");
}

#[test]
fn verdict_allows_a_pristine_skill() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-doctor", "v1", Some(("extra.md", "reference")));
    let manifest = SkillManifest::load_checked(&dest).unwrap();
    assert_eq!(
        skill_removal_verdict(&manifest, &dest, "tm-doctor"),
        SkillRemoval::Removable
    );
}

#[test]
fn verdict_keeps_a_hand_edited_skill() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-doctor", "v1", None);
    fs::write(dest.join("tm-doctor").join("SKILL.md"), "edited").unwrap();
    let manifest = SkillManifest::load_checked(&dest).unwrap();
    assert!(matches!(
        skill_removal_verdict(&manifest, &dest, "tm-doctor"),
        SkillRemoval::Kept(_)
    ));
}

#[test]
fn verdict_keeps_an_untracked_file() {
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("skills");
    let _src = deploy_real(&dest, "tm-doctor", "v1", None);
    fs::write(dest.join("tm-doctor").join("stray.md"), "mine").unwrap();
    let manifest = SkillManifest::load_checked(&dest).unwrap();
    assert!(matches!(
        skill_removal_verdict(&manifest, &dest, "tm-doctor"),
        SkillRemoval::Kept(_)
    ));
}

#[test]
fn retire_orphaned_skills_sweeps_every_tier() {
    // The orphan folds `skill_staleness` to Unknown independently at each tier,
    // so a sweep covering only one leaves the others reporting Unknown forever.
    let tmp = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let paths = paths_under(&tmp);
    let tiers = skill_deploy_tiers(&paths, Some(project.path()));
    assert_eq!(tiers.len(), 3, "{tiers:?}");
    for tier in &tiers {
        // `tm-retired-example` is in no source: not compiled in, not in the user
        // tier, not in the catalog.
        let _src = deploy_real(
            &tier.dir,
            "tm-retired-example",
            "gone from the bundle",
            None,
        );
        assert!(tier.dir.join("tm-retired-example").is_dir());
    }

    let retired = retire_orphaned_skills(&paths, Some(project.path()));

    assert_eq!(retired.len(), 3, "{retired:?}");
    for tier in &tiers {
        assert!(
            !tier.dir.join("tm-retired-example").exists(),
            "tier {} kept the orphan",
            tier.label
        );
    }
}
