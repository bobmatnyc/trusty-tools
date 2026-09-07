//! Tests for the #4947 skill-reachability doctor probe.
//!
//! Why: split out of `doctor_skill_reachability.rs` so that file stays under
//! the 500-SLOC production cap, matching the existing
//! `doctor_skill_project_tier{,_tests}.rs` pattern.
//! What: every test drives REAL directories under a temp root — a stubbed
//! scanner would test the stub, and testing the stub is precisely how #4949
//! went unnoticed while two skills were dropped from every deploy. Covers the
//! clean roster, each of the four unreachable shapes, the shadow warn, the
//! severity ordering between them, and the two states that must never render
//! as healthy.
//! Test: this file IS the test module.

use super::*;

/// A well-formed skill document declaring `stem` as its name.
fn skill_doc(stem: &str) -> String {
    format!("---\nname: {stem}\ndescription: fixture\n---\n\n# {stem}\n")
}

/// Build a `FrameworkPaths` rooted in `base` whose bundled roster declares
/// `stems` deployable, and return it with the project directory to probe.
///
/// Everything resolves under `base`, so no test can reach the real `$HOME`.
fn fixture(base: &Path, stems: &[&str]) -> (FrameworkPaths, PathBuf) {
    let paths = FrameworkPaths::under(base);
    let source = paths.skill_source_dir();
    std::fs::create_dir_all(&source).expect("fixture: bundled source dir");
    for stem in stems {
        std::fs::write(source.join(format!("{stem}.md")), skill_doc(stem))
            .expect("fixture: write bundled skill");
    }
    let project = base.join("project");
    std::fs::create_dir_all(&project).expect("fixture: project dir");
    (paths, project)
}

/// Deploy `stem` into `tier` with the given entry-point body, returning the
/// entry-point path.
fn deploy_with(tier: &Path, stem: &str, body: &str) -> PathBuf {
    let dir = tier.join(stem);
    std::fs::create_dir_all(&dir).expect("fixture: deployed skill dir");
    let entry = dir.join("SKILL.md");
    std::fs::write(&entry, body).expect("fixture: write SKILL.md");
    entry
}

/// Deploy `stem` into `tier` as a correct, reachable copy.
fn deploy(tier: &Path, stem: &str) -> PathBuf {
    deploy_with(tier, stem, &skill_doc(stem))
}

/// The happy path: every rostered skill sits in a tier the harness reads,
/// parses, and resolves under its own name.
#[test]
fn a_fully_deployed_roster_is_ok() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow", "tm-ticketing"]);
    let tier = paths.skill_deploy_dir();
    deploy(&tier, "tm-workflow");
    deploy(&tier, "tm-ticketing");

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
    assert_eq!(check.name, "skill_reachability");
    assert!(
        check.message.contains('2'),
        "the count belongs in the clean message: {}",
        check.message
    );
}

/// The regression this check exists for (#4947, #4949): a skill the framework
/// declares deployable that reached no tier at all.
///
/// Fails when detection is stubbed out — comment out the `deployed.get(stem)`
/// miss arm and this reports `Ok` while the capability is absent from every
/// session, which is the state every presence-only probe already reports green.
#[test]
fn a_rostered_skill_deployed_nowhere_fails_and_names_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow", "cto-kb-ingest"]);
    deploy(&paths.skill_deploy_dir(), "tm-workflow");

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "a rostered skill nothing deployed must FAIL, not warn: {}",
        check.message
    );
    assert!(
        check.message.contains("cto-kb-ingest"),
        "the unreachable skill must be named: {}",
        check.message
    );
    assert!(
        check.message.contains("#4947"),
        "the message must cite the issue: {}",
        check.message
    );
}

/// A deployed file whose frontmatter does not parse is on disk and still
/// unusable — presence-only probes call it deployed.
#[test]
fn malformed_frontmatter_fails_and_names_the_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow"]);
    let entry = deploy_with(
        &paths.skill_deploy_dir(),
        "tm-workflow",
        "---\nname: tm-workflow\ndescription: broken: unquoted colon\n---\n\nbody\n",
    );

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains(&entry.display().to_string()),
        "the message must name the file: {}",
        check.message
    );
    assert!(
        check.message.contains("does not parse"),
        "and say what is wrong with it: {}",
        check.message
    );
}

/// The harness resolves a skill by the name it is DEPLOYED under; a document
/// claiming a different `name` is unreachable under the name it claims.
#[test]
fn a_frontmatter_name_that_disagrees_with_the_directory_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow"]);
    deploy_with(
        &paths.skill_deploy_dir(),
        "tm-workflow",
        &skill_doc("tm-workflow-v2"),
    );

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("tm-workflow-v2"),
        "the declared name must be quoted back: {}",
        check.message
    );
}

/// Frontmatter that parses but declares nothing to resolve by.
#[test]
fn frontmatter_without_a_name_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow"]);
    deploy_with(
        &paths.skill_deploy_dir(),
        "tm-workflow",
        "---\ndescription: nameless\n---\n\nbody\n",
    );

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("declares no `name`"),
        "the message must say what is missing: {}",
        check.message
    );
}

/// The owner's "user level only" ruling (#6586), enforced as a reachability
/// finding: a project-tier copy beside the user-tier one is a duplicate only
/// one of which ever loads, so the finding names both paths and which to delete.
#[test]
fn a_project_copy_shadowing_the_user_copy_warns_and_names_both() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let winner = deploy(&paths.skill_deploy_dir(), "tm-ticketing");
    let loser = deploy(&project.join(".claude").join("skills"), "tm-ticketing");

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "a shadowed duplicate still loads, so it warns rather than fails: {}",
        check.message
    );
    assert!(
        check.message.contains(&winner.display().to_string())
            && check.message.contains(&loser.display().to_string()),
        "both paths must be named: {}",
        check.message
    );
    assert!(
        check.message.contains("delete the shadowed copy"),
        "and the message must say which copy to delete: {}",
        check.message
    );
}

/// Severity ordering: a skill nothing can load outranks one that loads from the
/// wrong tier, so a report carrying both must not read as a mere warning.
#[test]
fn an_unreachable_skill_outranks_a_shadow() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow", "tm-ticketing"]);
    deploy(&paths.skill_deploy_dir(), "tm-ticketing");
    deploy(&project.join(".claude").join("skills"), "tm-ticketing");

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("tm-workflow") && check.message.contains("tm-ticketing"),
        "both findings must survive into the message: {}",
        check.message
    );
}

/// An empty roster declares nothing, so "nothing was missing" is not a clean
/// bill of health — the #4605 fail-open shape, kept out.
#[test]
fn an_empty_roster_is_unknown_not_ok() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &[]);

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
    assert_ne!(check.status, CheckStatus::Ok);
}

/// A tier that EXISTS and cannot be listed is undetermined — counting it as
/// empty would report every skill in it unreachable.
#[test]
#[cfg(unix)]
fn an_unreadable_tier_is_unknown_not_ok() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-workflow"]);
    let tier = paths.skill_deploy_dir();
    deploy(&tier, "tm-workflow");

    let original = std::fs::metadata(&tier)
        .expect("tier metadata")
        .permissions();
    std::fs::set_permissions(&tier, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    // Running as root, mode 0o000 does not stop the read and there is nothing
    // to assert. Probe first, restore, then decide.
    let readable_anyway = std::fs::read_dir(&tier).is_ok();
    let check = check_skill_reachability(&paths, Some(&project));
    std::fs::set_permissions(&tier, original).expect("restore tier permissions");

    if readable_anyway {
        return;
    }
    assert_eq!(
        check.status,
        CheckStatus::Unknown,
        "an unreadable tier is undetermined, never healthy: {check:?}"
    );
    assert!(
        check.message.contains("could not be read"),
        "the message must say what it could not do: {}",
        check.message
    );
}

/// The probe is a diagnostic: it must leave every file it reports on alone.
#[test]
fn the_probe_removes_nothing_it_reports() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let winner = deploy(&paths.skill_deploy_dir(), "tm-ticketing");
    let loser = deploy(&project.join(".claude").join("skills"), "tm-ticketing");

    let check = check_skill_reachability(&paths, Some(&project));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        winner.exists() && loser.exists(),
        "the probe writes nothing"
    );
}

/// With no project directory in scope the probe still answers from the two
/// remaining tiers rather than degrading to a guess.
#[test]
fn resolves_without_a_project_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (paths, _project) = fixture(tmp.path(), &["tm-workflow"]);
    deploy(&paths.skill_deploy_dir(), "tm-workflow");

    let check = check_skill_reachability(&paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
}
