//! `tm doctor` full manifest-completeness check (issue #2158).
//!
//! Why: the pre-existing `agents`/`skills`/`output_style` probes each check
//! ONE facet in isolation (non-empty directory, resolvable style id) but
//! never diff the deployed payload against the canonical bundled roster as a
//! whole — a managed workspace could pass all three individually while still
//! missing several agents, carrying no ownership manifest, or missing the
//! `hooks` key, none of which the narrower probes catch. This check closes
//! that gap by delegating to [`crate::core::deploy_validate::validate_workspace`],
//! the same diff engine `tm validate` and the daemon's spawn/resume
//! auto-repair gate use, so all three surfaces report identical
//! completeness verdicts. Split into its own file (mirroring the existing
//! `doctor_output_style.rs` / `doctor_fs_checks.rs` split) to keep `doctor.rs`
//! under the 500-SLOC production cap.
//! What: [`check_deployment_completeness`] takes the already-resolved
//! [`FrameworkPaths`] `run_doctor` built for `project_dir` (home-relative when
//! no project is scoped) and folds every [`DeploymentGap`] into one
//! [`DoctorCheck`] — `Ok` when complete, `Fail` naming every gap otherwise.
//! Test: `deployment_check_ok_when_complete`, `deployment_check_fail_lists_gaps`.

use crate::core::deploy_validate::validate_workspace;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// Probe deployment completeness by diffing against the canonical bundled roster.
///
/// Why: see the module doc — this is the single check that catches a
/// partially-provisioned `.claude/` payload as a whole, not just one facet
/// of it.
/// What: `Ok` when [`validate_workspace`] finds no gaps; `Fail` naming every
/// gap (semicolon-joined, capped implicitly by the small roster size so the
/// message stays readable) plus the actionable hint to run `tm validate
/// --repair` otherwise.
/// Test: `deployment_check_ok_when_complete`, `deployment_check_fail_lists_gaps`.
pub(super) fn check_deployment_completeness(paths: &FrameworkPaths) -> DoctorCheck {
    let report = validate_workspace(paths);
    if report.is_complete() {
        return DoctorCheck::new(
            "deployment",
            CheckStatus::Ok,
            "deployed .claude/{agents,skills} and settings.json match the canonical roster",
        );
    }
    let detail: Vec<String> = report.gaps.iter().map(|g| g.describe()).collect();
    DoctorCheck::new(
        "deployment",
        CheckStatus::Fail,
        format!(
            "{} deployment gap(s) found: {} — run `tm validate --repair` to fix",
            detail.len(),
            detail.join("; ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_check_ok_when_complete() {
        // An entirely empty temp dir has no canonical source agents/skills to
        // diff against (both `agent_source_dir()`/`skill_source_dir()` and the
        // deploy targets are empty), so there is nothing to report missing —
        // the settings.json probes are the only ones that can still fail, so
        // seed a minimal complete settings.json plus manifests.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;

        let agents_dir = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents_dir).unwrap();
        crate::core::agent_manifest::AgentManifest::default()
            .save(&agents_dir)
            .unwrap();
        let skills_dir = paths.claude_skills_dir();
        std::fs::create_dir_all(&skills_dir).unwrap();
        crate::core::skill_manifest::SkillManifest::default()
            .save(&skills_dir)
            .unwrap();

        let claude_dir = paths.claude_home_dir().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let style_dir = claude_dir.join("output-styles");
        std::fs::create_dir_all(&style_dir).unwrap();
        let default_style = crate::core::bundle::OUTPUT_STYLES[0];
        std::fs::write(
            style_dir.join(default_style.file_name),
            default_style.content,
        )
        .unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
        )
        .unwrap();

        let check = check_deployment_completeness(&paths);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    }

    #[test]
    fn deployment_check_fail_lists_gaps() {
        // A totally unprovisioned workspace (no manifests, no settings.json)
        // must Fail and name the gaps in the message.
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = FrameworkPaths::under(tmp.path());
        paths.trusty_mpm_root = None;

        let check = check_deployment_completeness(&paths);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("agent ownership manifest"));
        assert!(check.message.contains("tm validate --repair"));
    }
}
