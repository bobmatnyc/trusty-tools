//! #9540: the Architect's shipped skills, through the real launch.
//!
//! Why: the deployer's own suite proves the write rules; only a launch proves
//! the launch and the mid-session refresh call it, for the Architect alone,
//! and that a deploy failure never fails the launch.
//! What: runs `prepare_session_inner` (and `sync_session_assets`) against a
//! scratch `$HOME` whose user config allow-lists the project, then reads the
//! project tier, the managed tier and the operator tier back.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::super::tests::EnvVarGuard;
use super::super::{HostInputs, PrepReport, prepare_session_inner};
use crate::core::paths::FrameworkPaths;
use crate::core::project_skill_tier::architect_skills::{self, SKILLS};
use crate::core::session_profile::SessionProfile;
use tempfile::tempdir;

/// Launch `project` with `home` as `$HOME`.
fn launch(project: &Path, home: &Path) -> PrepReport {
    let _home = EnvVarGuard::set("HOME", home);
    let fw = FrameworkPaths::under(home);
    prepare_session_inner(
        &fw,
        project,
        None,
        true,
        None,
        None,
        HostInputs::with_home(Some(home)),
    )
    .expect("the launch prepares")
}

/// A scratch `$HOME` whose user config allow-lists `project`.
fn home_allowing(project: &Path) -> tempfile::TempDir {
    let home = tempdir().unwrap();
    let root = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[supervisor]\nprojects = [{:?}]\n",
            project.display().to_string()
        ),
    )
    .unwrap();
    home
}

/// Make `project` request the supervisor profile.
fn request_supervisor(project: &Path) {
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .unwrap();
}

/// `<tier>/<stem>/SKILL.md` for every shipped skill.
fn copies(tier: &Path) -> Vec<PathBuf> {
    SKILLS
        .iter()
        .map(|s| architect_skills::skill_path(tier, s.stem))
        .collect()
}

/// Every tier an Architect skill must never reach.
fn foreign_tiers(home: &Path) -> Vec<PathBuf> {
    let fw = FrameworkPaths::under(home);
    vec![fw.skill_deploy_dir(), fw.claude_skills_dir()]
}

#[test]
#[serial_test::serial]
fn an_architect_launch_writes_both_skills_byte_equal() {
    let project = tempdir().unwrap();
    request_supervisor(project.path());
    let home = home_allowing(project.path());

    let report = launch(project.path(), home.path());

    assert_eq!(report.profile, SessionProfile::Architect);
    let tier = architect_skills::project_tier(project.path());
    for skill in SKILLS {
        let path = architect_skills::skill_path(&tier, skill.stem);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("{} not deployed: {e}", path.display()));
        assert_eq!(bytes, skill.contents.as_bytes(), "{}", path.display());
    }
    for foreign in foreign_tiers(home.path()) {
        for path in copies(&foreign) {
            assert!(
                !path.exists(),
                "an Architect skill reached {}",
                path.display()
            );
        }
    }
}

#[test]
#[serial_test::serial]
fn a_pm_launch_writes_no_architect_skill_anywhere() {
    // A plain PM, and a supervisor request with no allow-list entry (#8453).
    for requested in [false, true] {
        let project = tempdir().unwrap();
        if requested {
            request_supervisor(project.path());
        }
        let home = tempdir().unwrap();

        let report = launch(project.path(), home.path());

        assert_eq!(report.profile, SessionProfile::Pm);
        let mut tiers = foreign_tiers(home.path());
        tiers.push(architect_skills::project_tier(project.path()));
        for tier in tiers {
            for path in copies(&tier) {
                assert!(!path.exists(), "a PM launch wrote {}", path.display());
            }
        }
    }
}

#[test]
#[serial_test::serial]
fn a_deleted_copy_returns_on_the_next_launch_and_on_sync_assets() {
    let project = tempdir().unwrap();
    request_supervisor(project.path());
    let home = home_allowing(project.path());
    launch(project.path(), home.path());
    let tier = architect_skills::project_tier(project.path());
    let path = architect_skills::skill_path(&tier, architect_skills::FLEET_CHECK.stem);

    std::fs::remove_file(&path).unwrap();
    launch(project.path(), home.path());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        architect_skills::FLEET_CHECK.contents
    );

    std::fs::remove_file(&path).unwrap();
    {
        let _home = EnvVarGuard::set("HOME", home.path());
        // The workspace-scoped layout `sync_session_assets` documents.
        let mut fw = FrameworkPaths::under(home.path());
        fw.claude_agents = project.path().join(".claude").join("agents");
        fw.claude_skills = project.path().join(".claude").join("skills");
        super::super::sync_assets::sync_session_assets(&fw, project.path())
            .expect("sync-assets runs");
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        architect_skills::FLEET_CHECK.contents,
        "the mid-session refresh did not restore the deleted copy"
    );
}

#[test]
#[serial_test::serial]
fn an_edited_copy_survives_a_launch_and_is_named_by_health() {
    let project = tempdir().unwrap();
    request_supervisor(project.path());
    let home = home_allowing(project.path());
    launch(project.path(), home.path());
    let tier = architect_skills::project_tier(project.path());
    let path = architect_skills::skill_path(&tier, architect_skills::FLEET_CHECK.stem);
    std::fs::write(&path, "THE ARCHITECT'S OWN EDIT\n").unwrap();

    launch(project.path(), home.path());

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "THE ARCHITECT'S OWN EDIT\n"
    );
    let (ok, detail) = architect_skills::health(project.path());
    assert!(!ok, "{detail}");
    assert!(
        detail.contains("tm-fleet-check: project-custom"),
        "{detail}"
    );
}

#[test]
#[serial_test::serial]
fn a_second_launch_writes_no_architect_skill() {
    let project = tempdir().unwrap();
    request_supervisor(project.path());
    let home = home_allowing(project.path());
    launch(project.path(), home.path());
    let tier = architect_skills::project_tier(project.path());
    let mut files = copies(&tier);
    files.push(tier.join(architect_skills::RECORD_FILE));
    let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for f in &files {
        std::fs::File::options()
            .write(true)
            .open(f)
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()))
            .set_modified(then)
            .unwrap();
    }

    launch(project.path(), home.path());

    for f in &files {
        let mtime = std::fs::metadata(f).unwrap().modified().unwrap();
        assert_eq!(mtime, then, "the second launch rewrote {}", f.display());
    }
}

#[test]
#[serial_test::serial]
fn a_deploy_error_does_not_fail_the_launch() {
    let project = tempdir().unwrap();
    request_supervisor(project.path());
    let home = home_allowing(project.path());
    // A FILE where the skill's directory must go: unwritable even as root.
    let tier = architect_skills::project_tier(project.path());
    std::fs::create_dir_all(&tier).unwrap();
    std::fs::write(tier.join(architect_skills::FLEET_CHECK.stem), "x").unwrap();

    let report = launch(project.path(), home.path());

    assert!(
        report
            .roster_errors
            .iter()
            .any(|e| e.contains("architect skills not deployed") && e.contains("tm-fleet-check")),
        "{:?}",
        report.roster_errors
    );
    let (ok, detail) = architect_skills::health(project.path());
    assert!(!ok, "a failed deploy reported healthy: {detail}");
}
