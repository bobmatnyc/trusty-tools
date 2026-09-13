//! Tests for `skill_root` (#7727).

use std::fs;
use std::path::Path;

use super::skill_root::*;
use crate::agent_assets::AGENT_ASSETS;
use crate::agents::builder::AgentBuildError;

#[test]
fn resolves_every_placeholder() {
    let body = format!("a {SKILLS_ROOT_PLACEHOLDER}/x/SKILL.md b {SKILLS_ROOT_PLACEHOLDER}/y");
    let root = std::env::temp_dir().join("tm-skills");
    let out = resolve_skills_root(&body, &root).expect("absolute root resolves");
    let root = root.to_str().unwrap();
    assert_eq!(out, format!("a {root}/x/SKILL.md b {root}/y"));
}

#[test]
fn relative_skills_root_is_refused() {
    let err = resolve_skills_root("no placeholder", Path::new("skills")).unwrap_err();
    assert!(
        matches!(err, AgentBuildError::UnresolvedSkillsRoot(_)),
        "a relative root must be refused even with nothing to substitute: {err}"
    );
}

#[test]
fn compose_for_deploy_resolves_the_real_base_agent() {
    let src = tempfile::tempdir().unwrap();
    for (file_name, contents) in AGENT_ASSETS {
        fs::write(src.path().join(file_name), contents).unwrap();
    }
    let root = std::env::temp_dir().join("tm-skills");
    let composed = compose_agent_for_deploy("engineer", src.path(), &root).unwrap();
    assert!(!composed.contains(SKILLS_ROOT_PLACEHOLDER));
    let pointer = format!("{}/verification-before-completion/SKILL.md", root.display());
    assert!(
        composed.contains(&pointer),
        "BASE-AGENT's pointers must resolve to `{pointer}`"
    );
}
