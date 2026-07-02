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
//! remove. [`run_stale_mpm_skills_migration_once`] wraps both behind a
//! marker-file check so the cleanup runs at most once per machine — this is a
//! one-time migration for the #1905 rename, not permanent `tm doctor`
//! infrastructure, so it is never wired into [`crate::core::doctor`].
//! Test: `find_stale_mpm_skills_reports_known_former_skill`,
//! `find_stale_mpm_skills_ignores_unrelated_mpm_prefixed_dir`,
//! `find_stale_mpm_skills_ignores_before_tm_rename_lands`,
//! `find_stale_mpm_skills_missing_dir_is_empty`,
//! `find_stale_mpm_skills_only_matches_allowlisted_stems`,
//! `find_stale_mpm_skills_never_touches_catalog_sync_directory`,
//! `remove_stale_mpm_skills_deletes_directories`,
//! `migration_runs_once_then_marks_complete`,
//! `migration_is_noop_on_second_run`.

use std::collections::HashSet;
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
/// Why: callers (the one-time [`run_stale_mpm_skills_migration_once`]
/// migration and the hidden `tm doctor --prune-stale-skills` manual escape
/// hatch) both need the skill's name for display/logging and its full path
/// to act on; bundling them avoids re-deriving the path from the name twice.
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
/// rename so [`run_stale_mpm_skills_migration_once`] can remove them, without
/// ever touching an unrelated `mpm-*` skill or acting before the tm-*
/// replacements actually exist.
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
/// Why: both [`run_stale_mpm_skills_migration_once`] and the hidden manual
/// `tm doctor --prune-stale-skills` escape hatch need a single call that
/// removes every directory [`find_stale_mpm_skills`] reported.
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

/// Migration id recorded once the #1905 stale-skill cleanup has run.
///
/// Why: a stable, unique id lets the marker file record several unrelated
/// one-time migrations over time without them colliding.
const STALE_MPM_SKILLS_MIGRATION_ID: &str = "1905-stale-mpm-skills-prune";

/// Marker file recording which one-time migrations have already run,
/// `<framework_root>/migrations.json` (i.e. `~/.trusty-mpm/migrations.json`).
///
/// Why: `~/.trusty-mpm` (`FrameworkPaths::root`) is already the established
/// location for trusty-mpm's own persistent state (`framework/`, `registry/`,
/// logs); a small marker file there needs no new top-level directory.
/// What: joins `"migrations.json"` onto `root`.
fn migrations_marker_path(root: &Path) -> PathBuf {
    root.join("migrations.json")
}

/// Load the set of migration ids that have already run.
///
/// Why: [`run_stale_mpm_skills_migration_once`] needs a cheap idempotency
/// check on every `tm` invocation; a missing or unparseable marker file is
/// simply "nothing has run yet", not an error.
/// What: reads and parses `migrations.json` as a JSON array of strings;
/// returns an empty set on any I/O or parse failure.
fn completed_migrations(root: &Path) -> HashSet<String> {
    std::fs::read_to_string(migrations_marker_path(root))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Record `id` as a completed migration by rewriting the marker file.
///
/// Why: must persist durably so the migration never re-runs on a later `tm`
/// invocation, even across process restarts.
/// What: reads the current completed set, inserts `id`, and writes the
/// updated set back via a write-temp-then-rename (matching the atomic-write
/// pattern [`crate::core::agent_manifest::atomic_write`] uses for the
/// agent/skill manifests).
fn mark_migration_complete(root: &Path, id: &str) -> std::io::Result<()> {
    let mut completed = completed_migrations(root);
    completed.insert(id.to_string());
    let json = serde_json::to_string_pretty(&completed).unwrap_or_else(|_| "[]".to_string());
    std::fs::create_dir_all(root)?;
    let path = migrations_marker_path(root);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Run the #1905 stale pre-rename skill cleanup exactly once per machine.
///
/// Why: the owner's explicit direction is that this cleanup is a one-time
/// migration for operators upgrading across the mpm-*→tm-* rename, not
/// permanent `tm doctor` infrastructure — so it must never nag on every
/// invocation. Gating on [`STALE_MPM_SKILLS_MIGRATION_ID`] in the shared
/// migrations marker means it does real work at most once, then becomes a
/// single cheap file read forever after.
/// What: no-ops immediately if the migration id is already recorded.
/// Otherwise calls [`find_stale_mpm_skills`] against `claude_skills_dir`; if
/// any are found, removes them via [`remove_stale_mpm_skills`] and logs the
/// count at `info` (never `warn` — this is expected background maintenance,
/// not an operator-facing problem). The migration id is recorded — so it
/// never runs again — whenever the scan and any removal both succeed,
/// including the common case where nothing was stale. If removal fails, the
/// marker is deliberately NOT written so the next `tm` invocation retries.
/// Call this from a startup path (CLI `main`, daemon startup); it is cheap
/// enough to call unconditionally once tracing is initialized.
/// Test: `migration_runs_once_then_marks_complete`,
/// `migration_is_noop_on_second_run`.
pub fn run_stale_mpm_skills_migration_once(root: &Path, claude_skills_dir: &Path) {
    if completed_migrations(root).contains(STALE_MPM_SKILLS_MIGRATION_ID) {
        return;
    }

    let stale = find_stale_mpm_skills(claude_skills_dir);
    if !stale.is_empty() {
        match remove_stale_mpm_skills(&stale) {
            Ok(count) => {
                tracing::info!(
                    count,
                    "removed stale pre-rename mpm-* skill(s) (one-time #1905 migration)"
                );
            }
            Err(error) => {
                tracing::debug!(
                    %error,
                    "stale mpm-* skill migration failed — will retry on next launch"
                );
                return;
            }
        }
    } else {
        tracing::debug!("no stale pre-rename mpm-* skills found (#1905 migration check)");
    }

    if let Err(error) = mark_migration_complete(root, STALE_MPM_SKILLS_MIGRATION_ID) {
        tracing::debug!(%error, "failed to record #1905 migration marker");
    }
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

    #[test]
    fn find_stale_mpm_skills_only_matches_allowlisted_stems() {
        // Regression guard (owner feedback on #1905): every entry this
        // function returns must have a name literally present in
        // FORMER_TRUSTY_MPM_SKILLS — never a prefix/wildcard `mpm-*` match.
        // This locks in the frozen-allowlist design so a future edit can't
        // accidentally widen it to a glob that would also delete unrelated
        // skills belonging to the separate Python claude-mpm framework.
        let tmp = TempDir::new().unwrap();
        for name in FORMER_TRUSTY_MPM_SKILLS {
            write_skill(tmp.path(), name);
        }
        // Plant several mpm-*-prefixed names that are NOT in the allowlist —
        // standing in for the unrelated claude-mpm framework's own skills,
        // which share the `mpm-` prefix purely by coincidence.
        for unrelated in ["mpm-doctor", "mpm-workflow", "mpm-config", "mpm-help"] {
            write_skill(tmp.path(), unrelated);
        }
        write_skill(tmp.path(), "tm-doctor"); // the rename has landed here

        let stale = find_stale_mpm_skills(tmp.path());
        assert_eq!(stale.len(), FORMER_TRUSTY_MPM_SKILLS.len());
        for skill in &stale {
            assert!(
                FORMER_TRUSTY_MPM_SKILLS.contains(&skill.name.as_str()),
                "{} matched via something other than the frozen allowlist",
                skill.name
            );
        }
    }

    #[test]
    fn find_stale_mpm_skills_never_touches_catalog_sync_directory() {
        // Regression guard (owner feedback on #1905): agents and general
        // (non-tm-prefixed) skills are sourced from the separate claude-mpm
        // repo via the catalog-sync mechanism (`~/.trusty-mpm/catalog/repo`,
        // `tm catalog sync`), NOT compiled in via include_str! like the
        // tm-* skills this module cleans up. This cleanup logic must stay
        // strictly scoped to whatever directory it is explicitly given
        // (`~/.claude/skills/`) and must never see or touch a catalog-sync
        // checkout, even one containing an allowlisted stem name.
        let tmp = TempDir::new().unwrap();
        let claude_skills = tmp.path().join(".claude").join("skills");
        write_skill(&claude_skills, "tm-doctor");

        let catalog_repo_skills = tmp
            .path()
            .join(".trusty-mpm")
            .join("catalog")
            .join("repo")
            .join("skills");
        write_skill(&catalog_repo_skills, "mpm-bug-reporting");
        assert!(catalog_repo_skills.join("mpm-bug-reporting").is_dir());

        let stale = find_stale_mpm_skills(&claude_skills);
        assert!(stale.is_empty());
        // The catalog-sync copy must survive completely untouched.
        assert!(catalog_repo_skills.join("mpm-bug-reporting").is_dir());
    }

    #[test]
    fn migration_runs_once_then_marks_complete() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".trusty-mpm");
        let skills = tmp.path().join(".claude").join("skills");
        write_skill(&skills, "mpm-bug-reporting");
        write_skill(&skills, "tm-bug-reporting");

        run_stale_mpm_skills_migration_once(&root, &skills);

        assert!(!skills.join("mpm-bug-reporting").exists());
        assert!(skills.join("tm-bug-reporting").exists());
        assert!(
            completed_migrations(&root).contains(STALE_MPM_SKILLS_MIGRATION_ID),
            "migration must record itself as complete"
        );
    }

    #[test]
    fn migration_is_noop_on_second_run() {
        // Once the marker is set, a stray mpm-* directory created afterwards
        // (e.g. the user manually restored a backup) must NOT be swept by a
        // later invocation — this is a one-time migration, not an ongoing
        // check.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".trusty-mpm");
        let skills = tmp.path().join(".claude").join("skills");
        write_skill(&skills, "tm-bug-reporting");

        run_stale_mpm_skills_migration_once(&root, &skills);
        assert!(completed_migrations(&root).contains(STALE_MPM_SKILLS_MIGRATION_ID));

        // Simulate a stale directory reappearing after the migration ran.
        write_skill(&skills, "mpm-bug-reporting");
        run_stale_mpm_skills_migration_once(&root, &skills);

        // The second run must be a no-op: the marker short-circuits it.
        assert!(skills.join("mpm-bug-reporting").exists());
    }
}
