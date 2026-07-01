//! Filesystem-only `tm doctor` probes — instructions, agents, skills.
//!
//! Why: `doctor.rs` crossed the 500-SLOC production cap once the A2
//! `check_skill_source` probe (tm-skills-portfolio epic) was added. These four
//! probes share no state with the network-facing checks (`check_memory`,
//! `check_search`, `check_worktrees`) in `doctor.rs`, so splitting them here —
//! mirroring the existing `doctor_output_style.rs` split — keeps both files
//! under the cap without changing behaviour.
//! What: [`check_instructions`], [`check_agents`], [`check_skills`], and
//! [`check_skill_source`] — each a synchronous filesystem probe returning a
//! [`DoctorCheck`] — plus the shared [`count_files_with_extension`] helper.
//! Test: the `tests` module below covers every status branch of all four
//! probes against temp directories.

use std::path::Path;

use crate::core::agent_manifest::MANIFEST_FILE;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// Probe the instruction pipeline by looking for `last-instructions.md`.
///
/// Why: `prepare_session` writes `<project>/.trusty-mpm/last-instructions.md`
/// every time it assembles a PM prompt; its presence proves the instruction
/// pipeline has run at least once for the project.
/// What: when `project_dir` is given, checks that project's
/// `.trusty-mpm/last-instructions.md`; with no project it cannot scope the
/// probe and reports `Warn`. A missing file is `Warn` (the pipeline simply has
/// not run yet), an empty file is `Fail`.
/// Test: `instructions_present_is_ok`, `instructions_missing_is_warn`.
pub(super) fn check_instructions(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            "instructions",
            CheckStatus::Warn,
            "no project directory supplied — cannot verify last-instructions.md",
        );
    };
    let stash = project.join(".trusty-mpm").join("last-instructions.md");
    match std::fs::metadata(&stash) {
        Ok(meta) if meta.len() > 0 => DoctorCheck::new(
            "instructions",
            CheckStatus::Ok,
            format!("instruction pipeline ran — {} present", stash.display()),
        ),
        Ok(_) => DoctorCheck::new(
            "instructions",
            CheckStatus::Fail,
            format!("{} exists but is empty", stash.display()),
        ),
        Err(_) => DoctorCheck::new(
            "instructions",
            CheckStatus::Warn,
            format!(
                "{} not found — launch a session in this project to run the pipeline",
                stash.display()
            ),
        ),
    }
}

/// Probe agent deployment under `~/.claude/agents/`.
///
/// Why: without deployed agent files Claude Code has nothing to delegate to;
/// the daemon also expects an ownership manifest alongside them.
/// What: `Fail` when the directory is absent or holds no `.md` files; `Warn`
/// when agents are present but the manifest JSON is missing; `Ok` when both
/// agent files and the manifest are present.
/// Test: `agents_missing_dir_is_fail`, `agents_without_manifest_is_warn`.
pub(super) fn check_agents(paths: &FrameworkPaths) -> DoctorCheck {
    let dir = paths.claude_agents_dir();
    let md_count = count_files_with_extension(&dir, "md");
    if md_count == 0 {
        return DoctorCheck::new(
            "agents",
            CheckStatus::Fail,
            format!(
                "no agent files in {} — run `tm install` to deploy agents",
                dir.display()
            ),
        );
    }
    let manifest = dir.join(MANIFEST_FILE);
    if manifest.exists() {
        DoctorCheck::new(
            "agents",
            CheckStatus::Ok,
            format!(
                "{md_count} agent(s) deployed in {} with manifest",
                dir.display()
            ),
        )
    } else {
        DoctorCheck::new(
            "agents",
            CheckStatus::Warn,
            format!(
                "{md_count} agent(s) in {} but {MANIFEST_FILE} is missing",
                dir.display()
            ),
        )
    }
}

/// Probe skill deployment under `~/.claude/skills/`.
///
/// Why: skills extend the PM with reusable capabilities; their deployment is a
/// separate step that may not have run yet.
/// What: `Fail` when `~/.claude/skills/` does not exist at all; `Warn` when it
/// exists but is empty (skill deployment not yet implemented / not run); `Ok`
/// when it holds at least one entry.
/// Test: `skills_missing_dir_is_fail`, `skills_empty_dir_is_warn`.
pub(super) fn check_skills(home: &Path) -> DoctorCheck {
    let dir = home.join(".claude").join("skills");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let count = entries.flatten().count();
            if count == 0 {
                DoctorCheck::new(
                    "skills",
                    CheckStatus::Warn,
                    format!(
                        "{} exists but is empty — no skills deployed yet",
                        dir.display()
                    ),
                )
            } else {
                DoctorCheck::new(
                    "skills",
                    CheckStatus::Ok,
                    format!("{count} skill entr(ies) in {}", dir.display()),
                )
            }
        }
        Err(_) => DoctorCheck::new(
            "skills",
            CheckStatus::Fail,
            format!("{} does not exist", dir.display()),
        ),
    }
}

/// Probe the skill *source* directory (`FrameworkPaths::skill_source_dir()`).
///
/// Why: [`check_skills`] only inspects the deploy *target*
/// (`~/.claude/skills/`), so a missing or empty source directory (e.g. an
/// uninitialised `agents/skills/` submodule, or a bundled-assets path that
/// moved) previously deployed zero skills with no operator-visible signal —
/// `deploy_skills_filtered` silently returned an empty `DeployStats` (A2).
/// This probe mirrors the [`check_agents`] present-but-empty distinction so
/// operators see the *cause* (bad source) rather than only the *symptom*
/// (empty target).
/// What: `Fail` when the source directory does not exist; `Warn` when it
/// exists but holds no `.md` skill files; `Ok` when it holds at least one.
/// Test: `skill_source_missing_dir_is_fail`, `skill_source_empty_dir_is_warn`,
/// `skill_source_populated_dir_is_ok`.
pub(super) fn check_skill_source(paths: &FrameworkPaths) -> DoctorCheck {
    let dir = paths.skill_source_dir();
    if !dir.is_dir() {
        return DoctorCheck::new(
            "skill_source",
            CheckStatus::Fail,
            format!(
                "skill source directory {} does not exist — skill deploy will no-op",
                dir.display()
            ),
        );
    }
    let md_count = count_files_with_extension(&dir, "md");
    if md_count == 0 {
        DoctorCheck::new(
            "skill_source",
            CheckStatus::Warn,
            format!(
                "{} exists but has no skill files — skill deploy will no-op",
                dir.display()
            ),
        )
    } else {
        DoctorCheck::new(
            "skill_source",
            CheckStatus::Ok,
            format!("{md_count} skill file(s) available in {}", dir.display()),
        )
    }
}

/// Count files with the given extension directly under `dir`.
///
/// Why: the agent and skill-source probes need the number of deployed `.md`
/// files; a missing directory is simply zero, not an error.
/// What: returns the count of regular entries whose extension equals `ext`; an
/// unreadable directory yields `0`.
/// Test: covered by `agents_missing_dir_is_fail`.
fn count_files_with_extension(dir: &Path, ext: &str) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case(ext))
                    .unwrap_or(false)
            })
            .count(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_present_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".trusty-mpm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("last-instructions.md"), "PM instructions").unwrap();
        let check = check_instructions(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn instructions_missing_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let check = check_instructions(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn instructions_no_project_is_warn() {
        let check = check_instructions(None);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn agents_missing_dir_is_fail() {
        let tmp = tempfile::tempdir().unwrap();
        // `FrameworkPaths::under` derives `<base>/.claude/agents`, which does
        // not exist under a fresh temp dir.
        let paths = FrameworkPaths::under(tmp.path());
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn agents_without_manifest_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("engineer.md"), "agent").unwrap();
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn agents_with_manifest_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("engineer.md"), "agent").unwrap();
        std::fs::write(agents.join(MANIFEST_FILE), "{}").unwrap();
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn skills_missing_dir_is_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn skills_empty_dir_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude").join("skills")).unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn skills_populated_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("tm-doctor.md"), "skill").unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn skill_source_missing_dir_is_fail() {
        // A2: an uninitialised skill source directory must surface as a
        // doctor Fail, not a silent no-op deploy.
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let check = check_skill_source(&paths);
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn skill_source_empty_dir_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        std::fs::create_dir_all(paths.skill_source_dir()).unwrap();
        let check = check_skill_source(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn skill_source_populated_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let source = paths.skill_source_dir();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("tm-doctor.md"), "skill").unwrap();
        let check = check_skill_source(&paths);
        assert_eq!(check.status, CheckStatus::Ok);
    }
}
