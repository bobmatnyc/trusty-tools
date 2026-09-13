//! Deploy-mode coverage for `{{TM_SKILLS}}` skill paths (#7727).
//!
//! Why: the placeholder is only fixed when every production deploy resolves it
//! to a skills directory that actually holds the file the pointer names. These
//! tests run the real deploy entry points for each install shape and check the
//! deployed bytes against the deployed skill tree.
//! What: the default managed root, a `TRUSTY_MPM_ROOT`/`--root` managed root
//! (injected as the explicit `managed_root` argument the standalone commands
//! pass after resolution, never through `std::env::set_var`), and a managed
//! project session's asset sync.
//! Test: this file.

use std::path::Path;

use trusty_agents_common::agents::skill_root::SKILLS_ROOT_PLACEHOLDER;

use crate::core::paths::FrameworkPaths;

/// The skill files `BASE-AGENT.md` points every agent at.
const POINTED: &[&str] = &[
    "condition-based-waiting/SKILL.md",
    "verification-before-completion/SKILL.md",
    "self-improvement-loop/SKILL.md",
];

/// No deployed agent carries the raw placeholder, and `engineer` names every
/// pointed-at file under `skills_root`, each of which exists on disk.
fn assert_resolved(agents_dir: &Path, skills_root: &Path) {
    let mut checked = 0;
    for entry in std::fs::read_dir(agents_dir)
        .expect("agents deployed")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains(SKILLS_ROOT_PLACEHOLDER),
            "{} carries the raw placeholder",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked > 1,
        "expected a deployed roster in {}",
        agents_dir.display()
    );

    let engineer = std::fs::read_to_string(agents_dir.join("engineer.md")).expect("engineer.md");
    for relative in POINTED {
        let pointer = skills_root.join(relative);
        assert!(
            engineer.contains(&pointer.display().to_string()),
            "engineer.md must point at {}",
            pointer.display()
        );
        assert!(
            pointer.is_file(),
            "{} must exist on disk",
            pointer.display()
        );
    }
}

#[test]
fn default_root_deploy_resolves_skill_paths_that_exist() {
    let base = tempfile::tempdir().unwrap();
    let fw = FrameworkPaths::under(base.path());
    crate::core::skill_source::ensure_skill_source_fresh(&fw).unwrap();
    crate::core::skill_install_tiers::deploy_install_skill_tiers(&fw).unwrap();

    let out = crate::core::agent_source::autodeploy_agents_for(
        &fw,
        &fw.agent_deploy_dir(),
        &fw.skill_deploy_dir(),
    );

    assert!(
        out.warnings.is_empty(),
        "unexpected warnings: {:?}",
        out.warnings
    );
    assert_resolved(&fw.agent_deploy_dir(), &fw.skill_deploy_dir());
}

#[test]
fn managed_root_override_deploy_resolves_skill_paths_that_exist() {
    let base = tempfile::tempdir().unwrap();
    // What `TRUSTY_MPM_ROOT=<root>` / `--root <root>` resolves to; the
    // standalone commands pass it to `ensure_global_config_dir` explicitly.
    let root = base.path().join("custom-root");
    let config_dir = root.join("claude-config");
    let mut fw = FrameworkPaths::under(base.path());
    fw.agents = root.join("framework").join("agents");
    fw.skills = root.join("framework").join("skills");
    crate::core::agent_source::ensure_agent_source_fresh(&fw.agents).unwrap();
    crate::core::skill_source::ensure_skill_source_fresh(&fw).unwrap();

    crate::core::standalone::global_config::ensure_global_config_dir_with_exe(
        &root,
        &config_dir,
        Some(Path::new(crate::test_support::STABLE_HOOK_EXE)),
    )
    .unwrap();

    assert_resolved(&config_dir.join("agents"), &config_dir.join("skills"));
}

#[test]
fn managed_project_sync_resolves_skill_paths_that_exist() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let fw = FrameworkPaths::for_managed_project(base.path().join(".trusty-mpm"), &repo);
    crate::core::agent_source::ensure_agent_source_fresh(&fw.agents).unwrap();
    crate::core::skill_source::ensure_skill_source_fresh(&fw).unwrap();
    crate::core::skill_install_tiers::deploy_install_skill_tiers(&fw).unwrap();

    crate::core::session_launch::sync_session_assets(&fw, &repo).unwrap();

    assert_resolved(&fw.agent_deploy_dir(), &fw.skill_deploy_dir());
}
