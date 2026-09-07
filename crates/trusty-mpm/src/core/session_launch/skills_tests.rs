//! Tests for the automatic project-tier stray sweep at session start (#6754).
//!
//! Why: the sweep DELETES files out of the operator's checkout without being
//! asked, on every launch. Three things therefore have to be proven against
//! real directories rather than mocks: that it removes a copy tm's ledger says
//! tm wrote, that it does NOT remove a same-named skill the operator authored,
//! and that a removal it cannot complete leaves the launch running.
//! What: drives [`deploy_session_skills`] — the entry point #6754 names — for
//! the removal and the protection cases, and [`sweep_project_tier_strays`]
//! directly for the notice, the fail-open warn, and the backup-root shape.
//! Test: this file IS the test module.

use super::*;
use crate::core::manifest::{HarnessManifest, HarnessPlan};
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};
use trusty_agents_common::agents::manifest::checksum;

/// A framework root plus a project, shaped the way a managed session sees them.
///
/// `fw` is built the way `prepare_session` builds it — `for_managed_project`,
/// which relocates `claude_skills` onto `<project>/.claude/skills`. That is the
/// shape the sweep has to cope with, so no test may use a home-rooted one.
fn fixture(base: &Path, bundled: &[&str]) -> (FrameworkPaths, PathBuf) {
    let project = base.join("project");
    std::fs::create_dir_all(&project).expect("fixture: project dir");
    let fw = FrameworkPaths::for_managed_workspace_under(base, &project);
    let source = fw.skill_source_dir();
    std::fs::create_dir_all(&source).expect("fixture: bundled source dir");
    for stem in bundled {
        std::fs::write(source.join(format!("{stem}.md")), "# bundled\n")
            .expect("fixture: write bundled skill");
    }
    (fw, project)
}

/// `<project>/.claude/skills`.
fn tier(project: &Path) -> PathBuf {
    project.join(".claude").join("skills")
}

/// Write a directory-shaped skill into the project tier; returns its body.
fn project_skill(project: &Path, stem: &str) -> String {
    let dir = tier(project).join(stem);
    std::fs::create_dir_all(&dir).expect("fixture: project skill dir");
    let body = format!("# {stem}\n");
    std::fs::write(dir.join("SKILL.md"), &body).expect("fixture: write SKILL.md");
    body
}

/// Record `stem` in the tier's ledger — this is what makes a copy a STRAY
/// rather than the operator's own work.
fn record(project: &Path, stem: &str, content: &str) {
    let dir = tier(project);
    let mut manifest = SkillManifest::load(&dir).expect("fixture: load ledger");
    manifest.managed.insert(
        stem.to_string(),
        SkillManifestEntry {
            checksum: checksum(content),
            deployed_at: "2026-09-01T00:00:00Z".to_string(),
        },
    );
    manifest.save(&dir).expect("fixture: save ledger");
}

/// One session start's skill provisioning, against `fixture`'s paths.
fn run_session_start(fw: &FrameworkPaths, project: &Path) -> Vec<String> {
    let plan = HarnessPlan::from_manifest(
        &HarnessManifest::default(),
        fw,
        &crate::content::catalog_root_for(&fw.root),
    );
    let mut roster_errors = Vec::new();
    deploy_session_skills(fw, &plan, project, &HashMap::new(), &mut roster_errors);
    roster_errors
}

/// Capture what one closure emits, as rendered log lines.
fn captured(f: impl FnOnce()) -> Vec<String> {
    use tracing_subscriber::layer::SubscriberExt;

    // #4931: `with_default` is thread-local and never raises the process-global
    // MAX_LEVEL, so without this the capture records nothing.
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    tracing::subscriber::with_default(subscriber, f);
    buffer.tail(64)
}

/// The acceptance (#6754): a session start removes the stray by itself, with no
/// `tm doctor --fix-skills --yes` in between.
///
/// Fails before this fix: `deploy_session_skills` never called the removal, so
/// `tm-ticketing/SKILL.md` was still there when the session started.
#[test]
fn a_session_start_removes_a_stray_project_tier_copy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);

    let roster_errors = run_session_start(&fw, &project);

    assert!(
        !tier(&project).join("tm-ticketing").exists(),
        "session start must leave the tier without the stray"
    );
    assert!(
        roster_errors.is_empty(),
        "and must not report the sweep as a provisioning failure: {roster_errors:?}"
    );
    let manifest = SkillManifest::load(&tier(&project)).expect("ledger");
    assert!(
        !manifest.is_managed("tm-ticketing"),
        "the ledger must stop claiming the removed copy: {:?}",
        manifest.managed.keys().collect::<Vec<_>>()
    );
}

/// The case the removal exists to protect: the operator wrote their own skill
/// under a bundled name. Nothing in tm's ledger claims it, so nothing removes
/// it — not the doctor path, and not session start either.
#[test]
fn a_session_start_keeps_a_user_authored_skill_under_a_bundled_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing", "tm-workflow"]);
    // Authored by hand, never recorded: a bundled NAME is not evidence of a
    // bundled copy.
    let authored = project_skill(&project, "tm-ticketing");
    // Deployed by tm and then hand-edited: recorded, but the bytes moved on.
    let edited = project_skill(&project, "tm-workflow");
    record(&project, "tm-workflow", &format!("{edited}stale\n"));

    run_session_start(&fw, &project);

    assert_eq!(
        std::fs::read_to_string(tier(&project).join("tm-ticketing").join("SKILL.md"))
            .expect("the authored skill must survive"),
        authored
    );
    assert_eq!(
        std::fs::read_to_string(tier(&project).join("tm-workflow").join("SKILL.md"))
            .expect("the hand-edited skill must survive"),
        edited
    );
}

/// A session's `fw` carries `claude_skills` RELOCATED onto the project tier, so
/// passing it straight to the sweep would make the reserved-tier guard compare
/// that tier against itself and refuse every sweep.
#[test]
fn a_session_fw_whose_claude_skills_is_the_project_tier_still_sweeps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing"]);
    assert_eq!(
        fw.claude_skills_dir(),
        tier(&project),
        "the premise: a managed session's fw aims `claude_skills` at the project tier"
    );
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);

    assert_eq!(sweep_project_tier_strays(&fw, &project), 1);
}

/// The notice: one line, naming the count, only when something was removed.
#[test]
#[serial_test::serial]
fn the_sweep_notice_names_the_removed_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing", "tm-workflow"]);
    for stem in ["tm-ticketing", "tm-workflow"] {
        let body = project_skill(&project, stem);
        record(&project, stem, &body);
    }

    let lines = captured(|| {
        assert_eq!(sweep_project_tier_strays(&fw, &project), 2);
    });

    let notice = lines
        .iter()
        .find(|l| l.contains("stray project-tier"))
        .unwrap_or_else(|| panic!("no sweep notice: {lines:#?}"));
    assert!(
        notice.contains("removed=2"),
        "the count is a field: {notice}"
    );
    assert!(
        notice.contains("backup=") && notice.contains("backup-session-stray-sweep-"),
        "and the notice says where the copies went: {notice}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("stray project-tier"))
            .count(),
        1,
        "one line, not one per removal: {lines:#?}"
    );
}

/// A tier with nothing to sweep says nothing at all — a session start must not
/// print a line about work it did not do.
#[test]
#[serial_test::serial]
fn a_clean_tier_emits_no_sweep_notice() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing"]);
    project_skill(&project, "our-house-style");

    let lines = captured(|| {
        assert_eq!(sweep_project_tier_strays(&fw, &project), 0);
    });

    assert!(
        !lines.iter().any(|l| l.contains("stray project-tier")),
        "a clean tier is silent: {lines:#?}"
    );
    assert!(
        !tmp.path()
            .join(".trusty-mpm")
            .read_dir()
            .expect("framework root")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("backup-")),
        "and leaves no empty backup root behind"
    );
}

/// Fail open (#6754): a removal that cannot complete is one warn line naming
/// the path, and the session start it runs inside carries on.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn a_removal_failure_warns_with_the_path_and_the_launch_continues() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, project) = fixture(tmp.path(), &["tm-ticketing"]);
    let body = project_skill(&project, "tm-ticketing");
    record(&project, "tm-ticketing", &body);

    // Read+execute, no write: the ledger and the bytes are still readable, so
    // the copy is verified removable and backed up — and then `remove_dir_all`
    // cannot unlink `SKILL.md` out of it.
    let stray = tier(&project).join("tm-ticketing");
    std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o500))
        .expect("drop write permission");
    if std::fs::remove_file(stray.join("SKILL.md")).is_ok() {
        // Running as root, or a filesystem that ignores the mode bits — the
        // failure under test cannot be staged here.
        std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o755)).expect("restore");
        return;
    }

    let mut lines = Vec::new();
    let roster_errors = captured_into(&mut lines, || run_session_start(&fw, &project));
    std::fs::set_permissions(&stray, std::fs::Permissions::from_mode(0o755)).expect("restore");

    let warn = lines
        .iter()
        .find(|l| l.contains("could not remove a stray project-tier skill copy"))
        .unwrap_or_else(|| panic!("no fail-open warn: {lines:#?}"));
    assert!(warn.contains("WARN"), "the level is warn: {warn}");
    assert!(
        warn.contains(&stray.display().to_string()),
        "and the line names the path: {warn}"
    );
    assert!(
        roster_errors.is_empty(),
        "a failed sweep is not a provisioning failure: {roster_errors:?}"
    );
    assert!(
        stray.join("SKILL.md").is_file(),
        "and the copy tm could not remove is still there"
    );
}

/// [`captured`], for a closure whose value the caller needs.
fn captured_into<T>(lines: &mut Vec<String>, f: impl FnOnce() -> T) -> T {
    let mut out = None;
    *lines = captured(|| out = Some(f()));
    out.expect("the closure ran")
}

/// The backups land beside the other `~/.trusty-mpm/backup-*` roots and carry
/// the run's timestamp, so a second session cannot overwrite a first session's
/// copies.
#[test]
fn the_backup_root_is_timestamped_under_the_framework_root() {
    let now = chrono::DateTime::from_timestamp(1_754_000_000, 0).expect("fixed instant");
    assert_eq!(
        stray_backup_root(Path::new("/home/.trusty-mpm"), now),
        PathBuf::from("/home/.trusty-mpm/backup-session-stray-sweep-20250731-221320")
    );
}
