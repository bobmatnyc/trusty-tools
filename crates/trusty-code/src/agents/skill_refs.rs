//! Skill files a composed tcode agent body points at by absolute path (#7727).
//!
//! Why: the shared `BASE-AGENT.md` tells an agent to open a skill file with its
//! read tool at `{{TM_SKILLS}}/<skill>/SKILL.md`. trusty-mpm resolves that
//! placeholder to its own deployed skills tier; tcode has no such tier, and its
//! `read_file` is confined to the run's project or scratch root, so a pointer
//! into any other real directory is one the agent cannot open.
//! What: [`REFERENCED_SKILL_FILES`] embeds every file a roster pointer names.
//! Two resolutions exist:
//! - The project roster deploy writes the files beneath
//!   `<project>/.trusty-code/skill-refs` ([`project_skill_refs_dir`],
//!   [`materialize_skill_refs`]); the pointers land inside the project root.
//! - An in-process compose (embedded fallback, a disk agent carrying the
//!   placeholder) resolves to [`user_skill_refs_dir`] with no disk I/O
//!   ([`resolve_skill_refs`]). `read_file` built with
//!   `ReadFileTool::with_skill_refs` serves exactly those paths from
//!   [`embedded_skill_ref`], so the pointer opens in bound and projectless runs
//!   alike and compose never depends on a writable home.
//!
//! Neither directory is a skill-discovery directory, so these copies never
//! shadow the embedded skill catalog.
//! Test: `agents::skill_refs::tests`, `tools::fs::read::tests::skill_ref_*`.

use std::path::{Path, PathBuf};

use trusty_agents_common::agents::manifest::{ManifestError, atomic_write};
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
    // #8010: BASE-AGENT's revert/bisect rule points here (#7628).
    (
        "git-workflow/SKILL.md",
        include_str!("../assets/skill-refs/git-workflow/SKILL.md"),
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

/// `~/.trusty-code/skill-refs` — the address in-process composes resolve to.
///
/// What: a path only; nothing is written there. `read_file` serves it.
pub fn user_skill_refs_dir() -> PathBuf {
    crate::paths::private_state::private_state_dir().join(SKILL_REFS_DIRNAME)
}

/// The embedded content for `relative`, a path beneath a skill-refs root.
///
/// What: component-wise equality, so `a/../b` never matches `b`.
/// Test: `embedded_skill_ref_matches_exact_entries_only`.
pub fn embedded_skill_ref(relative: &Path) -> Option<&'static str> {
    REFERENCED_SKILL_FILES
        .iter()
        .find(|(entry, _)| Path::new(entry) == relative)
        .map(|(_, content)| *content)
}

/// Write every [`REFERENCED_SKILL_FILES`] entry beneath `dir`.
///
/// What: creates parent directories and atomically rewrites a file only when
/// its content differs, so repeated deploys do no writes and a concurrent
/// reader never sees a partial file. Postcondition: every entry is readable at
/// `dir/<relative path>` with its embedded content.
/// Test: `materialize_skill_refs_writes_readable_files`.
pub fn materialize_skill_refs(dir: &Path) -> std::io::Result<()> {
    for (relative, content) in REFERENCED_SKILL_FILES {
        let path = dir.join(relative);
        if std::fs::read_to_string(&path).is_ok_and(|current| current == *content) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write(&path, content).map_err(|e| match e {
            ManifestError::Io(io) => io,
            other => std::io::Error::other(other),
        })?;
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
mod tests {
    use super::*;

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
        materialize_skill_refs(tmp.path()).unwrap();
        materialize_skill_refs(tmp.path()).unwrap();
        for (relative, content) in REFERENCED_SKILL_FILES {
            assert_eq!(
                std::fs::read_to_string(tmp.path().join(relative)).unwrap(),
                *content
            );
        }
    }

    #[test]
    fn embedded_skill_ref_matches_exact_entries_only() {
        let hit = Path::new("self-improvement-loop/SKILL.md");
        assert_eq!(embedded_skill_ref(hit), Some(REFERENCED_SKILL_FILES[2].1));
        for miss in [
            "self-improvement-loop",
            "x/../self-improvement-loop/SKILL.md",
            "/self-improvement-loop/SKILL.md",
        ] {
            assert_eq!(embedded_skill_ref(Path::new(miss)), None, "{miss}");
        }
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
