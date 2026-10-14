//! #9540: the Architect skill deployer against a scratch project tier.
//!
//! Why: df-D22's rules — write when absent, refresh only an untouched copy,
//! never overwrite a project-custom one, never fail the caller — each need a
//! case that a wrong deployer fails.
//! What: drives [`super::deploy_skills_into`] and [`super::health`] on temp
//! dirs; the launch-path cases live in
//! `session_launch::skills::architect_9540_tests`.
//! Test: this is the test module.

use super::*;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

/// A shipped table of one skill with `contents`.
fn one(contents: &'static str) -> [ArchitectSkill; 1] {
    [ArchitectSkill {
        stem: "tm-probe",
        contents,
    }]
}

/// Pin `path`'s mtime to a fixed past instant and return it.
fn age(path: &Path) -> SystemTime {
    let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(then)
        .unwrap();
    then
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn an_absent_skill_is_written_byte_equal() {
    let project = tempdir().unwrap();
    let report = deploy_architect_skills(project.path());
    for skill in SKILLS {
        let path = skill_path(&project_tier(project.path()), skill.stem);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            skill.contents.as_bytes(),
            "{}",
            path.display()
        );
    }
    assert_eq!(report.written, vec!["tm-fleet-check", "tm-context-refresh"]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn an_edited_copy_is_kept_and_reported() {
    use tracing_subscriber::layer::SubscriberExt;

    let project = tempdir().unwrap();
    deploy_architect_skills(project.path());
    let path = skill_path(&project_tier(project.path()), FLEET_CHECK.stem);
    std::fs::write(&path, "PROJECT EDIT\n").unwrap();

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let report =
        tracing::subscriber::with_default(subscriber, || deploy_architect_skills(project.path()));

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "PROJECT EDIT\n");
    assert_eq!(report.kept_custom, vec!["tm-fleet-check"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("WARN")
            && l.contains("tm-fleet-check")
            && l.contains("project-custom")),
        "no WARN line names the kept copy: {lines:#?}"
    );
    let (ok, detail) = health(project.path());
    assert!(!ok, "{detail}");
    assert!(
        detail.contains("tm-fleet-check: project-custom"),
        "{detail}"
    );
    assert!(!detail.contains("tm-context-refresh"), "{detail}");
}

#[test]
fn an_untouched_copy_is_refreshed_when_the_shipped_bytes_move() {
    let dir = tempdir().unwrap();
    deploy_skills_into(dir.path(), &one("v1\n"));
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.written, vec!["tm-probe"]);
    assert_eq!(
        std::fs::read_to_string(skill_path(dir.path(), "tm-probe")).unwrap(),
        "v2\n"
    );
}

#[test]
fn a_seeded_copy_with_no_record_and_other_bytes_is_custom() {
    // A `tm fleet init` seed that predates the record and was edited since.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "older seed\n").unwrap();
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "older seed\n");
}

#[test]
fn a_byte_equal_seed_is_adopted_then_refreshed() {
    // An unrecorded copy equal to the shipped one is adopted, not rewritten,
    // and is then refreshed like any copy tm deployed.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "v1\n").unwrap();
    let pinned = age(&path);
    let adopted = deploy_skills_into(dir.path(), &one("v1\n"));
    assert!(adopted.written.is_empty(), "{:?}", adopted.written);
    assert_eq!(mtime(&path), pinned);
    let refreshed = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(refreshed.written, vec!["tm-probe"]);
}

#[test]
fn a_second_run_writes_nothing() {
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    let first = deploy_architect_skills(project.path());
    assert_eq!(first.written.len(), SKILLS.len());
    let mut files: Vec<PathBuf> = SKILLS.iter().map(|s| skill_path(&tier, s.stem)).collect();
    files.push(tier.join(RECORD_FILE));
    let pinned: Vec<SystemTime> = files.iter().map(|f| age(f)).collect();

    let second = deploy_architect_skills(project.path());

    assert_eq!(second, DeployReport::default());
    let after: Vec<SystemTime> = files.iter().map(|f| mtime(f)).collect();
    assert_eq!(after, pinned, "a run with nothing changed rewrote a file");
}

#[test]
fn a_blocked_skill_dir_is_an_error_line() {
    // A FILE where the skill's directory must go: unwritable even as root.
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    std::fs::create_dir_all(&tier).unwrap();
    std::fs::write(tier.join(FLEET_CHECK.stem), "not a dir").unwrap();

    let report = deploy_architect_skills(project.path());

    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("tm-fleet-check"),
        "{:?}",
        report.errors
    );
    assert_eq!(report.written, vec!["tm-context-refresh"]);
    let (ok, detail) = health(project.path());
    assert!(!ok, "a failed deploy reported healthy: {detail}");
    assert!(detail.contains("tm-fleet-check: unreadable"), "{detail}");
}

#[test]
fn health_names_each_skill_that_is_not_current() {
    let project = tempdir().unwrap();
    let (ok, detail) = health(project.path());
    assert!(!ok);
    assert!(detail.contains("tm-fleet-check: missing"), "{detail}");
    assert!(detail.contains("tm-context-refresh: missing"), "{detail}");

    deploy_architect_skills(project.path());
    let (ok, detail) = health(project.path());
    assert!(ok, "{detail}");
}

#[test]
fn architect_skills_never_join_the_bundled_pm_roster() {
    // Drift guard: a bundled skill deploys to every PM's managed tier.
    let bundled = crate::core::skill_retire::bundled_skill_stems(
        crate::core::content_source::test_support::rc(),
    );
    assert!(!bundled.is_empty(), "the bundled roster read as empty");
    for skill in SKILLS {
        assert!(
            !bundled.contains(skill.stem),
            "{} is in the bundled PM skill roster",
            skill.stem
        );
    }
}
