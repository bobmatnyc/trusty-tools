//! Skill files a composed tcode agent body points at by absolute path (#7727).
//!
//! Why: the shared `BASE-AGENT.md` tells an agent to open a skill file with its
//! read tool at `{{TM_SKILLS}}/<skill>/SKILL.md`. trusty-mpm resolves that
//! placeholder to its own deployed skills tier; tcode has no such tier, and its
//! `read_file` is confined to the run's project or scratch root, so a pointer
//! into any other real directory is one the agent cannot open.
//! What: [`REFERENCED_SKILLS`] names every skill a roster pointer names; its
//! text is the shared `content/skills/<skill>.md`, read at runtime through
//! [`SkillRefs`] (#9013 — the four byte copies tcode compiled in are gone, so
//! the pointer and trusty-mpm's skill are one file). Two resolutions exist:
//! - The project roster deploy writes the files beneath
//!   `<project>/.trusty-code/skill-refs` ([`project_skill_refs_dir`],
//!   [`materialize_skill_refs`]); the pointers land inside the project root.
//! - An in-process compose (embedded fallback, a disk agent carrying the
//!   placeholder) resolves to [`user_skill_refs_dir`] with no disk I/O
//!   ([`resolve_skill_refs`]). `read_file` built with
//!   `ReadFileTool::with_skill_refs` serves exactly those paths through
//!   [`skill_ref`], so the pointer opens in bound and projectless runs alike
//!   and compose never depends on a writable home. No content means the read
//!   fails naming the remedy; it never serves a stale copy.
//!
//! Neither directory is a skill-discovery directory, so these copies never
//! shadow the default skill catalog.
//! Test: `agents::skill_refs::tests`, `tools::fs::read::tests::skill_ref_*`.

use std::path::{Path, PathBuf};

use trusty_agents_common::agents::skill_root::{SKILLS_ROOT_PLACEHOLDER, resolve_skills_root};

use trusty_agents_common::agent_content::{AgentContentError, ResolvedContent};

use crate::paths::WriteTargetError;
use crate::paths::write_dir::NativeWriteDir;

/// Directory name, beneath a `.trusty-code` root, holding the referenced files.
pub const SKILL_REFS_DIRNAME: &str = "skill-refs";

/// Every skill a shared roster pointer names, as `<skill>` of
/// `{{TM_SKILLS}}/<skill>/SKILL.md` (#9013: read from `content/skills/<skill>.md`).
pub const REFERENCED_SKILLS: &[&str] = &[
    "condition-based-waiting",
    // #8010: BASE-AGENT's revert/bisect rule points here (#7628).
    "git-workflow",
    "self-improvement-loop",
    "verification-before-completion",
];

/// The bundle path of the shared skill behind pointer `<skill>/SKILL.md`.
fn content_path(skill: &str) -> String {
    format!("skills/{skill}.md")
}

/// The [`REFERENCED_SKILLS`] entry `relative` names, when it is exactly
/// `<skill>/SKILL.md`.
///
/// What: component-wise equality, so `a/../b/SKILL.md` never matches.
/// Test: `skill_ref_matches_exact_entries_only`.
pub fn referenced_skill(relative: &Path) -> Option<&'static str> {
    REFERENCED_SKILLS
        .iter()
        .find(|skill| Path::new(*skill).join("SKILL.md") == relative)
        .copied()
}

/// The text of every [`REFERENCED_SKILLS`] entry, read from one content source.
///
/// Why: the roster deploy writes all four beside the agents; it must fail as a
/// whole rather than deploy pointers to files it could not write.
/// What: `(relative path, text)` pairs, the relative path `<skill>/SKILL.md`.
/// Test: `materialize_skill_refs_writes_readable_files`.
#[derive(Debug, Clone)]
pub struct SkillRefs {
    files: Vec<(String, String)>,
}

impl SkillRefs {
    /// Reads every referenced skill from `content`.
    ///
    /// # Errors
    /// The first absent, empty or unreadable skill, naming its bundle path.
    pub fn from_content(content: &ResolvedContent) -> Result<Self, AgentContentError> {
        let files = REFERENCED_SKILLS
            .iter()
            .map(|skill| {
                let text = crate::assets::content::require_file(content, &content_path(skill))?;
                Ok((format!("{skill}/SKILL.md"), text))
            })
            .collect::<Result<_, AgentContentError>>()?;
        Ok(Self { files })
    }

    /// Every `(relative path, text)` pair.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.files.iter().map(|(r, t)| (r.as_str(), t.as_str()))
    }
}

/// The text behind `relative`, a path beneath a skill-refs root, or `Ok(None)`
/// when it names no [`REFERENCED_SKILLS`] entry.
///
/// Why: `read_file` serves the pointer without disk I/O beneath the refs root.
/// What: resolves content only for a referenced path; no content is an error
/// naming the remedy (#9013).
/// Test: `skill_ref_matches_exact_entries_only`.
pub fn skill_ref(relative: &Path) -> Result<Option<String>, AgentContentError> {
    let Some(skill) = referenced_skill(relative) else {
        return Ok(None);
    };
    let content = crate::assets::content::resolve_for_process()?;
    crate::assets::content::require_file(&content, &content_path(skill)).map(Some)
}

/// `<project>/.trusty-code/skill-refs` — the root the project roster deploy uses.
pub fn project_skill_refs_dir(project_root: &Path) -> PathBuf {
    crate::paths::native_child(project_root, SKILL_REFS_DIRNAME)
}

/// `~/.trusty-code/skill-refs` — the address in-process composes resolve to.
///
/// What: a path only; nothing is written there. `read_file` serves it.
pub fn user_skill_refs_dir() -> PathBuf {
    crate::paths::private_state::private_state_dir().join(SKILL_REFS_DIRNAME)
}

/// Write every `refs` entry beneath a pinned `dir`.
///
/// Why: this is the write #7779 caught landing in a victim directory — the old
/// signature took a PATH, so the `create_dir_all`/`atomic_write` pair re-walked
/// `<project>/.trusty-code/skill-refs` long after `check_native_write_target`
/// had approved it, and followed whatever had been swapped in since.
/// What: every entry's `<skill>/` directory is opened as a pinned child of `dir`
/// ([`NativeWriteDir::child`]) and the file is written `openat`-relative to that
/// handle, so the kernel resolves it against the validated inode rather than the
/// path. A file whose content already matches is left alone, so repeated deploys
/// do no writes; the rewrite is atomic, so a concurrent reader never sees a
/// partial file. Postcondition: every entry is readable at
/// `dir.path()/<relative path>` with its content.
/// Test: `materialize_skill_refs_writes_readable_files`,
/// `agents::deploy::deploy_tests::swapped_skill_refs_dir_never_reaches_the_victim`.
pub fn materialize_skill_refs(
    dir: &NativeWriteDir,
    refs: &SkillRefs,
) -> Result<(), WriteTargetError> {
    for (relative, content) in refs.iter() {
        let path = Path::new(relative);
        let (parent, name) = match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
            (Some(parent), Some(name)) => (parent, name),
            _ => {
                return Err(WriteTargetError::Unpinnable {
                    path: dir.path().join(relative),
                    detail: format!("`{relative}` is not a usable skill-ref path"),
                });
            }
        };
        let skill = dir.child(parent)?;
        if skill
            .read(name)?
            .is_some_and(|current| current == content.as_bytes())
        {
            continue;
        }
        skill.atomic_write(name, content.as_bytes())?;
    }
    Ok(())
}

/// Resolve the skills-root placeholder in a composed body against `dir`.
///
/// Why: compose runs for every agent resolution, so a failure here must never
/// remove an agent from the roster (#7727 review, HIGH 2).
/// What: pure string work. A body with no placeholder is returned unchanged.
/// Otherwise the placeholder becomes `dir`; an unresolvable `dir` (relative —
/// the no-home fallback — or non-UTF-8) logs a warning naming it and returns
/// the body unchanged, so the agent still loads with an unresolved pointer.
/// Test: `resolve_skill_refs_substitutes_without_touching_disk`,
/// `resolve_skill_refs_keeps_the_body_for_a_relative_dir`.
pub fn resolve_skill_refs(composed: &str, dir: &Path) -> String {
    if !composed.contains(SKILLS_ROOT_PLACEHOLDER) {
        return composed.to_string();
    }
    match resolve_skills_root(composed, dir) {
        Ok(resolved) => resolved,
        Err(e) => {
            tracing::warn!(
                path = %dir.display(),
                "skill pointers left unresolved in a composed agent: {e}"
            );
            composed.to_string()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The referenced skills of the running checkout (#9013), so no test
    /// depends on the cwd, HOME or an installed bundle.
    pub(crate) fn test_refs() -> SkillRefs {
        let root = trusty_common::test_harness::test_repo_root()
            .expect("the running checkout's workspace root"); // #9298
        let content =
            trusty_agents_common::agent_content::checkout_content(&root).expect("repo content");
        SkillRefs::from_content(&content).expect("every referenced skill")
    }

    #[test]
    fn resolve_skill_refs_substitutes_without_touching_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skill-refs");
        let body = format!("Read `{SKILLS_ROOT_PLACEHOLDER}/self-improvement-loop/SKILL.md`.");
        let out = resolve_skill_refs(&body, &dir);
        let path = dir.join("self-improvement-loop/SKILL.md");
        assert_eq!(out, format!("Read `{}`.", path.display()));
        assert!(!dir.exists(), "compose must not write the refs dir");
    }

    #[test]
    fn resolve_skill_refs_keeps_the_body_for_a_relative_dir() {
        let body = format!("{SKILLS_ROOT_PLACEHOLDER}/x");
        assert_eq!(resolve_skill_refs(&body, Path::new("relative")), body);
    }

    #[test]
    fn materialize_skill_refs_writes_readable_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = NativeWriteDir::open(tmp.path(), Path::new(SKILL_REFS_DIRNAME)).unwrap();
        let refs = test_refs();
        materialize_skill_refs(&dir, &refs).unwrap();
        materialize_skill_refs(&dir, &refs).unwrap();
        for (relative, content) in refs.iter() {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(relative)).unwrap(),
                content
            );
        }
    }

    #[test]
    fn skill_ref_matches_exact_entries_only() {
        let hit = Path::new("self-improvement-loop/SKILL.md");
        assert_eq!(referenced_skill(hit), Some("self-improvement-loop"));
        let served = skill_ref(hit).expect("the checkout serves content");
        let refs = test_refs();
        let expected = refs
            .iter()
            .find(|(r, _)| *r == "self-improvement-loop/SKILL.md")
            .map(|(_, t)| t);
        assert_eq!(served.as_deref(), expected);
        for miss in [
            "self-improvement-loop",
            "x/../self-improvement-loop/SKILL.md",
            "/self-improvement-loop/SKILL.md",
        ] {
            assert_eq!(referenced_skill(Path::new(miss)), None, "{miss}");
            assert_eq!(skill_ref(Path::new(miss)).expect("no read"), None, "{miss}");
        }
    }

    /// #9013: a pointer serves trusty-mpm's own skill file, not a copy — the
    /// successor of `referenced_copies_match_trusty_mpm_skills`, which pinned
    /// four compiled-in byte copies to it.
    #[test]
    fn referenced_skills_are_the_shared_skill_files() {
        let skills = trusty_common::test_harness::test_repo_root()
            .expect("the running checkout's workspace root")
            .join("content/skills");
        for (relative, content) in test_refs().iter() {
            let skill = relative.trim_end_matches("/SKILL.md");
            let source = std::fs::read_to_string(skills.join(format!("{skill}.md")))
                .unwrap_or_else(|e| panic!("shared skill `{skill}` must exist: {e}"));
            assert_eq!(
                content, source,
                "`{relative}` is not `content/skills/{skill}.md`"
            );
        }
    }

    #[test]
    fn every_roster_pointer_names_an_embedded_file() {
        let marker = format!("{SKILLS_ROOT_PLACEHOLDER}/");
        // #9011: the shared texts are read from the checkout's content.
        let root = trusty_common::test_harness::test_repo_root()
            .expect("the running checkout's workspace root"); // #9298
        let content =
            trusty_agents_common::agent_content::checkout_content(&root).expect("repo content");
        for (file_name, md) in crate::assets::content::catalog_from(&content).expect("catalog") {
            for (at, _) in md.match_indices(&marker) {
                let rest = &md[at + marker.len()..];
                let relative = rest.split('`').next().unwrap_or(rest);
                assert!(
                    referenced_skill(Path::new(relative)).is_some(),
                    "{file_name} points at `{relative}`, which tcode does not serve"
                );
            }
        }
    }
}
