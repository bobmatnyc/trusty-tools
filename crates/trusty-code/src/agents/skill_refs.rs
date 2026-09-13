//! Skill files a composed tcode agent body points at by absolute path (#7727).
//!
//! Why: the shared `BASE-AGENT.md` tells an agent to open a skill file with its
//! read tool at `{{TM_SKILLS}}/<skill>/SKILL.md`. trusty-mpm resolves that
//! placeholder to its own deployed skills tier; tcode has no such tier — it
//! embeds only `SKILL.md` bodies for catalog use and discovers disk skills from
//! the project — so it materialises the referenced files itself and resolves
//! the placeholder to that directory. Materialising (rather than inlining the
//! skill bodies into every prompt) keeps the #7723 trim: the content loads only
//! when an agent opens the file.
//! What: [`REFERENCED_SKILL_FILES`] embeds every file a roster pointer names;
//! [`materialize_skill_refs`] writes them beneath a directory;
//! [`resolve_skill_refs`] materialises and substitutes when a body carries the
//! placeholder. The directory is `<project>/.trusty-code/skill-refs` for the
//! project roster deploy ([`project_skill_refs_dir`]) and
//! `~/.trusty-code/skill-refs` for in-process composes with no project
//! ([`user_skill_refs_dir`]). Neither is a skill-discovery directory, so these
//! copies never shadow the embedded skill catalog.
//! Test: `agents::skill_refs::tests`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use trusty_agents_common::agents::skill_root::{SKILLS_ROOT_PLACEHOLDER, resolve_skills_root};

/// Directory name, beneath a `.trusty-code` root, holding the referenced files.
pub const SKILL_REFS_DIRNAME: &str = "skill-refs";

/// `(path relative to the skills root, file content)` for every skill file a
/// shared roster pointer names. Byte copies of trusty-mpm's bundled skills,
/// pinned by `referenced_copies_match_trusty_mpm_skills`.
pub const REFERENCED_SKILL_FILES: &[(&str, &str)] = &[
    (
        "condition-based-waiting/SKILL.md",
        include_str!("../assets/skill-refs/condition-based-waiting/SKILL.md"),
    ),
    (
        "self-improvement-loop/SKILL.md",
        include_str!("../assets/skill-refs/self-improvement-loop/SKILL.md"),
    ),
    (
        "verification-before-completion/SKILL.md",
        include_str!("../assets/skill-refs/verification-before-completion/SKILL.md"),
    ),
];

/// `<project>/.trusty-code/skill-refs` — the root the project roster deploy uses.
pub fn project_skill_refs_dir(project_root: &Path) -> PathBuf {
    crate::paths::native_child(project_root, SKILL_REFS_DIRNAME)
}

/// `~/.trusty-code/skill-refs` — the root for composes with no project.
pub fn user_skill_refs_dir() -> PathBuf {
    crate::paths::private_state::private_state_dir().join(SKILL_REFS_DIRNAME)
}

/// Write every [`REFERENCED_SKILL_FILES`] entry beneath `dir`.
///
/// What: creates parent directories and rewrites a file only when its content
/// differs, so repeated composes do no writes. Postcondition: every entry is
/// readable at `dir/<relative path>` with its embedded content.
/// Test: `resolve_skill_refs_writes_readable_files`.
pub fn materialize_skill_refs(dir: &Path) -> std::io::Result<()> {
    for (relative, content) in REFERENCED_SKILL_FILES {
        let path = dir.join(relative);
        if std::fs::read_to_string(&path).is_ok_and(|current| current == *content) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
    }
    Ok(())
}

/// Resolve the skills-root placeholder in a composed body against `dir`.
///
/// What: a body with no placeholder is returned unchanged and touches no disk,
/// so a plain user agent never depends on a home directory. Otherwise the files
/// are materialised beneath `dir` and the placeholder is substituted; an
/// unresolvable `dir` (relative, non-UTF-8) is an error naming it.
/// Test: `resolve_skill_refs_writes_readable_files`,
/// `resolve_skill_refs_refuses_a_relative_dir`.
pub fn resolve_skill_refs(composed: &str, dir: &Path) -> anyhow::Result<String> {
    if !composed.contains(SKILLS_ROOT_PLACEHOLDER) {
        return Ok(composed.to_string());
    }
    let resolved = resolve_skills_root(composed, dir)?;
    materialize_skill_refs(dir).with_context(|| {
        format!(
            "failed to write referenced skill files to {}",
            dir.display()
        )
    })?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_skill_refs_writes_readable_files() {
        let tmp = tempfile::tempdir().unwrap();
        let body = format!("Read `{SKILLS_ROOT_PLACEHOLDER}/self-improvement-loop/SKILL.md`.");
        let out = resolve_skill_refs(&body, tmp.path()).unwrap();
        let path = tmp.path().join("self-improvement-loop/SKILL.md");
        assert_eq!(out, format!("Read `{}`.", path.display()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            REFERENCED_SKILL_FILES[1].1
        );
    }

    #[test]
    fn resolve_skill_refs_refuses_a_relative_dir() {
        let body = format!("{SKILLS_ROOT_PLACEHOLDER}/x");
        assert!(resolve_skill_refs(&body, Path::new("relative")).is_err());
    }

    #[test]
    fn referenced_copies_match_trusty_mpm_skills() {
        let mpm_skills =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trusty-mpm/src/assets/skills");
        for (relative, content) in REFERENCED_SKILL_FILES {
            let skill = relative.trim_end_matches("/SKILL.md");
            let source = std::fs::read_to_string(mpm_skills.join(format!("{skill}.md")))
                .unwrap_or_else(|e| panic!("trusty-mpm skill `{skill}` must exist: {e}"));
            assert_eq!(
                *content, source,
                "`assets/skill-refs/{relative}` drifted from trusty-mpm's `{skill}.md`"
            );
        }
    }

    #[test]
    fn every_roster_pointer_names_an_embedded_file() {
        let marker = format!("{SKILLS_ROOT_PLACEHOLDER}/");
        for (file_name, md) in crate::assets::EMBEDDED_TM_AGENT_SOURCES {
            for (at, _) in md.match_indices(&marker) {
                let rest = &md[at + marker.len()..];
                let relative = rest.split('`').next().unwrap_or(rest);
                assert!(
                    REFERENCED_SKILL_FILES.iter().any(|(r, _)| *r == relative),
                    "{file_name} points at `{relative}`, which tcode does not embed"
                );
            }
        }
    }
}
