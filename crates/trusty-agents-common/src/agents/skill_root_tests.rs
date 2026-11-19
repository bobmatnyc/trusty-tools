//! Tests for `skill_root` (#7727).

use std::fs;
use std::path::Path;

use super::skill_root::*;
use crate::agent_content::AgentRoster;

/// The repository roster (`content/agents`, #9011) — what `AGENT_ASSETS` embedded.
fn repo_roster() -> AgentRoster {
    AgentRoster::load(&crate::agent_content::tests::repo_content()).expect("repo roster")
}
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
    for (file_name, contents) in repo_roster().iter() {
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

/// #7684: a deployed agent carries `omitClaudeMd: true` inside its frontmatter.
#[test]
fn compose_for_deploy_stamps_omit_claude_md() {
    let src = tempfile::tempdir().unwrap();
    for (file_name, contents) in repo_roster().iter() {
        fs::write(src.path().join(file_name), contents).unwrap();
    }
    let root = std::env::temp_dir().join("tm-skills");
    for name in ["engineer", "rust-engineer"] {
        let composed = compose_agent_for_deploy(name, src.path(), &root).unwrap();
        let fm = composed
            .strip_prefix("---\n")
            .and_then(|rest| rest.split("\n---\n").next())
            .expect("composed agent opens with frontmatter");
        assert_eq!(
            fm.lines().filter(|l| *l == OMIT_CLAUDE_MD_LINE).count(),
            1,
            "{name} frontmatter must carry `omitClaudeMd: true` once:\n{fm}"
        );
    }
}

#[test]
fn stamp_omit_claude_md_is_idempotent_and_skips_unfenced_text() {
    let once = stamp_omit_claude_md("---\nname: a\n---\nbody\n");
    assert_eq!(once, "---\nomitClaudeMd: true\nname: a\n---\nbody\n");
    assert_eq!(stamp_omit_claude_md(&once), once);
    let user = "---\nomitClaudeMd: false\n---\n";
    assert_eq!(
        stamp_omit_claude_md(user),
        user,
        "an explicit value is kept"
    );
    assert_eq!(stamp_omit_claude_md("no fence"), "no fence");
}
