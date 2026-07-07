//! Issue #2149 coverage: a roster-deploy failure must not abort the rest of
//! session preparation.
//!
//! Why: split out of `tests.rs` (mirroring the `doctor_output_style.rs` /
//! `doctor_fs_checks.rs` split pattern already used in this crate) to keep
//! `tests.rs` under the 1500-SLOC test-file cap after these two tests were
//! added.
//! What: `prepare_session_continues_after_agent_deploy_failure` and
//! `prepare_session_continues_after_skill_deploy_failure` each force one
//! roster-deploy stage to fail (a corrupt agent manifest / a file blocking a
//! skill's target directory) and assert `prepare_session_inner` still
//! succeeds, still writes the trusty-mpm output-style identity carrier, and
//! records the failure in [`super::PrepReport::roster_errors`].
//! Test: this is the test module.

use super::*;
use tempfile::tempdir;

#[test]
fn prepare_session_continues_after_agent_deploy_failure() {
    // Issue #2149: a corrupt agent manifest at the deploy TARGET must not
    // abort the rest of preparation — the trusty-mpm identity carrier (the
    // output-style file + the `outputStyle` settings key) MUST still be
    // written so a session always self-identifies as trusty-mpm even when
    // its agent roster is empty or broken. Before this fix, `?` on
    // `deploy_agents_filtered` short-circuited `prepare_session_inner` before
    // ever reaching the output-style write below it.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    // An existing (even empty) agent SOURCE directory is required for
    // `deploy_agents_filtered` to proceed past its "missing source" no-op and
    // reach the manifest load at the TARGET directory below.
    std::fs::create_dir_all(fw.agent_source_dir()).unwrap();
    let agents_dir = fw.claude_agents_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join(crate::core::agent_manifest::MANIFEST_FILE),
        b"not valid json{{{",
    )
    .unwrap();

    let report = prepare_session_inner(&fw, project, None, true, None)
        .expect("a roster-deploy failure must not fail the whole preparation");

    assert_eq!(
        report.deploy,
        crate::core::agent_deployer::DeployResult::default(),
        "the agent deploy result must default out when the deploy step failed"
    );
    assert_eq!(
        report.roster_errors.len(),
        1,
        "exactly the agent-deploy failure must be recorded: {:?}",
        report.roster_errors
    );
    assert!(report.roster_errors[0].contains("agent deploy failed"));

    // The identity carrier still landed despite the roster failure.
    let output_style = project
        .join(".claude")
        .join("output-styles")
        .join("trusty-mpm.md");
    assert!(
        output_style.exists(),
        "output style must still deploy despite the agent-deploy failure: {}",
        output_style.display()
    );
    let settings = std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap();
    assert!(
        settings.contains("\"outputStyle\""),
        "outputStyle key must still be written: {settings}"
    );
}

#[test]
fn prepare_session_continues_after_skill_deploy_failure() {
    // Issue #2149: mirrors
    // `prepare_session_continues_after_agent_deploy_failure` for the skill
    // side — a broken skill-deploy TARGET must not abort identity
    // provisioning either.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    // Pre-create a REGULAR FILE where the bundled `tm-doctor` skill's target
    // directory must go. `ensure_skill_source_fresh` unconditionally
    // self-heals the skill *source* to the full bundled set (including
    // `tm-doctor.md`) before the deploy step runs, so this deterministically
    // makes `create_dir_all` fail when the deploy reaches that entry.
    let skills_dir = fw.claude_skills_dir();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("tm-doctor"), b"not a directory").unwrap();

    let report = prepare_session_inner(&fw, project, None, true, None)
        .expect("a roster-deploy failure must not fail the whole preparation");

    assert_eq!(
        report.skill_deploy,
        crate::core::skill_deployer::DeployStats::default(),
        "the skill deploy result must default out when the deploy step failed"
    );
    assert_eq!(
        report.roster_errors.len(),
        1,
        "exactly the skill-deploy failure must be recorded: {:?}",
        report.roster_errors
    );
    assert!(report.roster_errors[0].contains("skill deploy failed"));

    let output_style = project
        .join(".claude")
        .join("output-styles")
        .join("trusty-mpm.md");
    assert!(
        output_style.exists(),
        "output style must still deploy despite the skill-deploy failure: {}",
        output_style.display()
    );
}
