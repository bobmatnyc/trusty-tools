//! Detection and removal of stale pre-rename `mpm-*` skill directories (#1905).
//!
//! Why: trusty-mpm's own bundled skill portfolio moved from an `mpm-*` naming
//! convention to `tm-*` (PR #1872), but [`crate::core::skill_deployer`] never
//! deletes a previously-deployed directory when a skill is renamed — it only
//! writes new ones (`skill_deployer.rs` documents this: "Deselecting a skill
//! does not remove a previously deployed copy"). Left alone, a
//! `~/.claude/skills/` populated before the rename accumulates permanently
//! orphaned `mpm-*/SKILL.md` directories alongside their `tm-*` replacements.
//! `~/.claude/skills/` is also shared with the unrelated Python `claude-mpm`
//! framework, which ships its own `mpm-*`-prefixed skills (e.g. `mpm-doctor`,
//! `mpm-workflow`, `mpm-config`) purely by naming coincidence — deleting one
//! of those would destroy something the operator installed on purpose, so
//! this module never does a wildcard `mpm-*` sweep.
//! What: [`FORMER_TRUSTY_MPM_SKILLS`] is the exhaustive, frozen allowlist of
//! trusty-mpm's own pre-rename skill stems (PR #1872's deletion list).
//! [`find_stale_mpm_skills`] scans a `~/.claude/skills/`-shaped directory for
//! entries in that allowlist, reporting them only once the tm-* rename has
//! actually landed there (at least one `tm-` prefixed skill is deployed) so a
//! pre-rename install isn't mistakenly flagged as already-stale.
//! [`remove_stale_mpm_skills`] deletes the directories a caller has decided to
//! remove.
//! Test: `find_stale_mpm_skills_reports_known_former_skill`,
//! `find_stale_mpm_skills_ignores_unrelated_mpm_prefixed_dir`,
//! `find_stale_mpm_skills_ignores_before_tm_rename_lands`,
//! `find_stale_mpm_skills_missing_dir_is_empty`,
//! `remove_stale_mpm_skills_deletes_directories`.

use std::path::{Path, PathBuf};

/// trusty-mpm's own bundled skill stems before the mpm-*→tm-* rename
/// (PR #1872, closes #1905's rename portion).
///
/// Why: this list must stay exhaustive and frozen rather than being derived
/// from a `mpm-*` glob — `~/.claude/skills/` can legitimately hold unrelated
/// `mpm-*` skills from the Python `claude-mpm` framework, and globbing would
/// delete those.
/// What: the eleven skill stems PR #1872 deleted from
/// `crates/trusty-mpm/src/assets/skills/` when it added the `tm-*` portfolio.
/// Test: `find_stale_mpm_skills_reports_known_former_skill`.
pub const FORMER_TRUSTY_MPM_SKILLS: &[&str] = &[
    "mpm-bug-reporting",
    "mpm-circuit-breaker-enforcement",
    "mpm-delegation-patterns",
    "mpm-git-file-tracking",
    "mpm-pr-workflow",
    "mpm-session-management",
    "mpm-session-pause",
    "mpm-session-resume",
    "mpm-ticketing-integration",
    "mpm-tool-usage-guide",
    "mpm-verification-protocols",
];

/// One stale pre-rename skill directory found on disk.
///
/// Why: callers (the `tm doctor` probe and the `--prune-stale-skills` CLI
/// flow) both need the skill's name for display and its full path to act on;
/// bundling them avoids re-deriving the path from the name twice.
/// What: the skill stem (e.g. `"mpm-bug-reporting"`) and its directory path.
/// Test: `find_stale_mpm_skills_reports_known_former_skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleSkill {
    /// The skill stem, e.g. `"mpm-bug-reporting"`.
    pub name: String,
    /// The full directory path, e.g. `~/.claude/skills/mpm-bug-reporting`.
    pub path: PathBuf,
}

/// Scan `claude_skills_dir` for stale, safely-removable pre-rename skills.
///
/// Why: surfaces exactly the directories left behind by the mpm-*→tm-*
/// rename so `tm doctor` can warn about them and `--prune-stale-skills` can
/// remove them, without ever touching an unrelated `mpm-*` skill or acting
/// before the tm-* replacements actually exist.
/// What: returns every [`FORMER_TRUSTY_MPM_SKILLS`] entry present as a
/// directory directly under `claude_skills_dir`, but only when at least one
/// `tm-`-prefixed entry is also present there (the rename has landed on this
/// machine). Returns an empty vec when `claude_skills_dir` does not exist.
/// Test: `find_stale_mpm_skills_reports_known_former_skill`,
/// `find_stale_mpm_skills_ignores_unrelated_mpm_prefixed_dir`,
/// `find_stale_mpm_skills_ignores_before_tm_rename_lands`,
/// `find_stale_mpm_skills_missing_dir_is_empty`.
pub fn find_stale_mpm_skills(claude_skills_dir: &Path) -> Vec<StaleSkill> {
    let Ok(entries) = std::fs::read_dir(claude_skills_dir) else {
        return Vec::new();
    };

    let tm_rename_landed = entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with("tm-"))
            .unwrap_or(false)
    });
    if !tm_rename_landed {
        return Vec::new();
    }

    FORMER_TRUSTY_MPM_SKILLS
        .iter()
        .filter_map(|&name| {
            let path = claude_skills_dir.join(name);
            path.is_dir().then(|| StaleSkill {
                name: name.to_string(),
                path,
            })
        })
        .collect()
}

/// Delete the given stale skill directories from disk.
///
/// Why: `tm doctor --prune-stale-skills` needs a single call that removes
/// every directory [`find_stale_mpm_skills`] reported, once the operator has
/// opted in via the flag.
/// What: calls `std::fs::remove_dir_all` on each `stale` entry's path,
/// returning the count removed; stops and propagates the first I/O error
/// (leaving any already-removed directories deleted — each removal is
/// independent and idempotent to re-run).
/// Test: `remove_stale_mpm_skills_deletes_directories`.
pub fn remove_stale_mpm_skills(stale: &[StaleSkill]) -> std::io::Result<usize> {
    let mut removed = 0;
    for skill in stale {
        std::fs::remove_dir_all(&skill.path)?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n\n# {name}\n"),
        )
        .unwrap();
    }

    #[test]
    fn find_stale_mpm_skills_reports_known_former_skill() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "mpm-bug-reporting");
        write_skill(tmp.path(), "tm-bug-reporting");

        let stale = find_stale_mpm_skills(tmp.path());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "mpm-bug-reporting");
        assert_eq!(stale[0].path, tmp.path().join("mpm-bug-reporting"));
    }

    #[test]
    fn find_stale_mpm_skills_ignores_unrelated_mpm_prefixed_dir() {
        // `mpm-doctor` belongs to the unrelated Python claude-mpm framework —
        // it must never be reported as stale trusty-mpm output, even with the
        // tm-* rename landed.
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "mpm-doctor");
        write_skill(tmp.path(), "tm-doctor");

        let stale = find_stale_mpm_skills(tmp.path());
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_mpm_skills_ignores_before_tm_rename_lands() {
        // A pre-rename install with only mpm-* skills deployed (no tm-* yet)
        // must not be flagged — the rename has not actually happened there.
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "mpm-bug-reporting");

        let stale = find_stale_mpm_skills(tmp.path());
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_mpm_skills_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(find_stale_mpm_skills(&missing).is_empty());
    }

    #[test]
    fn remove_stale_mpm_skills_deletes_directories() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "mpm-bug-reporting");
        write_skill(tmp.path(), "mpm-pr-workflow");
        write_skill(tmp.path(), "tm-bug-reporting");

        let stale = find_stale_mpm_skills(tmp.path());
        assert_eq!(stale.len(), 2);

        let removed = remove_stale_mpm_skills(&stale).unwrap();
        assert_eq!(removed, 2);
        assert!(!tmp.path().join("mpm-bug-reporting").exists());
        assert!(!tmp.path().join("mpm-pr-workflow").exists());
        assert!(tmp.path().join("tm-bug-reporting").exists());
    }
}
