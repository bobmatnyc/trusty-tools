//! Detection and removal of stale pre-rename `mpm-*` skill directories (#1905).
//!
//! Why: trusty-mpm's own bundled skill portfolio moved from an `mpm-*` naming
//! convention to `tm-*` (PR #1872), but [`crate::core::skill_deployer`] never
//! deletes a previously-deployed directory when a skill is renamed — it only
//! writes new ones (`skill_deployer.rs` documents this: "Deselecting a skill
//! does not remove a previously deployed copy"). Left alone, a
//! `~/.claude/skills/` populated before the rename accumulates permanently
//! orphaned `mpm-*/SKILL.md` directories alongside their `tm-*` replacements.
//!
//! CRITICAL fork-model constraint (owner clarification): `/mpm-*` is NOT a
//! deprecated namespace trusty-mpm owns outright. `/tm-*` is a trusty-specific
//! FORK of a handful of `/mpm-*` skills; the general, harness-agnostic
//! `/mpm-*` skill (and agent) family is meant to keep existing indefinitely,
//! sourced from the separate claude-mpm project via the catalog-sync
//! mechanism (`~/.trusty-mpm/catalog/repo`, `tm catalog sync`). That means a
//! deployed `~/.claude/skills/mpm-<name>/` directory whose name happens to
//! match one of trusty-mpm's OLD bundled skill names is not automatically
//! trusty-mpm's own leftover — it could be a legitimate, current claude-mpm
//! catalog skill that only coincidentally shares that name. Name matching
//! alone can never safely decide "was this trusty-mpm's own deploy?", so this
//! module never deletes on name alone: [`is_confirmed_trusty_mpm_origin`]
//! is the mandatory gate between "name matches" and "actually safe to
//! remove", verified through [`crate::core::skill_manifest::SkillManifest`]
//! (primary) or a frozen content checksum (fallback) — see that function's
//! doc comment for the exact precedence.
//! What: [`FORMER_TRUSTY_MPM_SKILLS`] is the exhaustive, frozen allowlist of
//! trusty-mpm's own pre-rename skill stems (PR #1872's deletion list).
//! [`find_stale_mpm_skills`] scans a `~/.claude/skills/`-shaped directory for
//! entries in that allowlist whose origin [`is_confirmed_trusty_mpm_origin`]
//! can verify, reporting them only once the tm-* rename has actually landed
//! there (at least one `tm-` prefixed skill is deployed) so a pre-rename
//! install isn't mistakenly flagged as already-stale.
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
//! `find_stale_mpm_skills_ignores_differently_authored_same_name_skill`,
//! `find_stale_mpm_skills_confirms_origin_via_frozen_checksum_fallback`,
//! `remove_stale_mpm_skills_deletes_directories`,
//! `migration_runs_once_then_marks_complete`,
//! `migration_is_noop_on_second_run`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::agent_manifest::checksum;
use crate::core::skill_manifest::SkillManifest;

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

/// Sha256 checksums of the exact content trusty-mpm bundled for each
/// [`FORMER_TRUSTY_MPM_SKILLS`] stem, frozen at the last commit before PR
/// #1872 deleted these files (commit `bbd2faf8`, the parent of the deletion
/// commit `2a245e87`). Computed via
/// `git show bbd2faf8:crates/trusty-mpm/src/assets/skills/<name>.md | shasum -a 256`.
///
/// Why: the manifest-based origin check ([`is_confirmed_trusty_mpm_origin`])
/// cannot help when a deployed skill predates trusty-mpm's skill-deployment
/// manifest (or the manifest was lost/reset) — in that case the ONLY
/// remaining safe signal is byte-for-byte content identity with what
/// trusty-mpm itself used to ship. Falling back to name alone would violate
/// the fork-model constraint (a claude-mpm catalog skill may legitimately
/// share one of these names); this frozen table lets the fallback stay
/// exact instead.
/// What: `(stem, sha256_hex)` pairs for all eleven former skill names.
/// Test: `find_stale_mpm_skills_confirms_origin_via_frozen_checksum_fallback`.
// Each string below is a public sha256 content checksum (verifiable via the
// `git show` command in the doc comment above), not a credential — the
// `pragma` comments silence detect-secrets' high-entropy-hex false positive.
const FORMER_TRUSTY_MPM_SKILL_CONTENT_SHA256: &[(&str, &str)] = &[
    (
        "mpm-bug-reporting",
        "ae4217996f43b7d32c109675bb2e87a7f03a017b1d5d080e404f69d1130693da", // pragma: allowlist secret
    ),
    (
        "mpm-circuit-breaker-enforcement",
        "9201be68031dac92209ef262d07a87b295040c540b130f706b495db77fb0bab5", // pragma: allowlist secret
    ),
    (
        "mpm-delegation-patterns",
        "a0206ac2bc81264b8b14e854260179e1f1050fad9c2949813c6c8c447759cb4f", // pragma: allowlist secret
    ),
    (
        "mpm-git-file-tracking",
        "8450fa909f729bd8095db3a3705bcb44c91c48f069fcd23f212e144b90612aaf", // pragma: allowlist secret
    ),
    (
        "mpm-pr-workflow",
        "9dd922ecd082cc37525c618bf01ecc8ce6b94e80386672459f92aa9d872c895d", // pragma: allowlist secret
    ),
    (
        "mpm-session-management",
        "ce576c484f307967bccf3f01fbff6c143f0c2cd88ae220e73522b7024312bbcf", // pragma: allowlist secret
    ),
    (
        "mpm-session-pause",
        "3222569435343d98f757099e1cb628a961ff7a192e14e09cfcabcf9a7dedc5e4", // pragma: allowlist secret
    ),
    (
        "mpm-session-resume",
        "8b12545b4db016b0da7b860d613792e86e4888ec21afd74c0a2e3bb9c9ca5495", // pragma: allowlist secret
    ),
    (
        "mpm-ticketing-integration",
        "0b3347fdfaed66616541bf1c6d8e049df693fa7581a41a269bc9e53f384565d7", // pragma: allowlist secret
    ),
    (
        "mpm-tool-usage-guide",
        "23ea465fea861077adbcf6429ee733a231009eed4b5fbb1df21dfa7196efca8b", // pragma: allowlist secret
    ),
    (
        "mpm-verification-protocols",
        "35faabef27f1bc21f4b509dd60d5c229cfc4f0f12517064025c722a1a9d28f5d", // pragma: allowlist secret
    ),
];

/// Look up the frozen pre-rename content checksum for a former skill name.
///
/// Why: small helper so [`is_confirmed_trusty_mpm_origin`] reads as a linear
/// primary-then-fallback check rather than an inline linear scan.
/// What: returns the sha256 hex digest recorded in
/// [`FORMER_TRUSTY_MPM_SKILL_CONTENT_SHA256`] for `name`, or `None` when
/// `name` is not one of the eleven frozen entries.
fn frozen_content_checksum(name: &str) -> Option<&'static str> {
    FORMER_TRUSTY_MPM_SKILL_CONTENT_SHA256
        .iter()
        .find(|(stem, _)| *stem == name)
        .map(|(_, sha)| *sha)
}

/// Confirm a name-matched candidate was actually deployed by trusty-mpm's own
/// deployer, not some other origin sharing the same name.
///
/// Why: under the fork model (owner clarification on #1905), `/mpm-*` is the
/// harness-agnostic original namespace, sourced from the separate claude-mpm
/// catalog and meant to coexist with trusty-mpm's `/tm-*` fork indefinitely.
/// A `~/.claude/skills/mpm-<name>/` directory whose name is in
/// [`FORMER_TRUSTY_MPM_SKILLS`] is therefore NOT automatically trusty-mpm's
/// own stale leftover — a `tm catalog sync` could have legitimately deployed
/// a current claude-mpm skill under that exact same name. This function is
/// the mandatory safety gate between "name matches" and "confirmed safe to
/// delete".
/// What: reads `<skill_dir>/SKILL.md`; unreadable content never confirms
/// (fails closed). Primary check: [`SkillManifest`] (written exclusively by
/// [`crate::core::skill_deployer::deploy_skills`]) has a managed entry for
/// `name` AND its recorded checksum matches the file's current content —
/// this proves trusty-mpm's own deployer wrote it and nothing has replaced
/// the content since. A managed entry whose checksum does NOT match is a
/// confirmed non-match (someone/something else has since overwritten trusty-
/// mpm's deploy) and does not fall through to the checksum fallback.
/// Fallback (only when the manifest has no entry for `name` at all — e.g. a
/// deploy that predates the manifest): the content's sha256 must exactly
/// equal [`frozen_content_checksum`] for `name`. Any candidate failing both
/// checks returns `false`.
/// Test: `find_stale_mpm_skills_reports_known_former_skill`,
/// `find_stale_mpm_skills_ignores_differently_authored_same_name_skill`,
/// `find_stale_mpm_skills_confirms_origin_via_frozen_checksum_fallback`.
fn is_confirmed_trusty_mpm_origin(name: &str, skill_dir: &Path, manifest: &SkillManifest) -> bool {
    let Ok(content) = std::fs::read_to_string(skill_dir.join("SKILL.md")) else {
        return false;
    };

    if manifest.is_managed(name) {
        return manifest.checksum_matches(name, &content);
    }

    frozen_content_checksum(name).is_some_and(|expected| expected == checksum(&content))
}

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
/// ever touching an unrelated `mpm-*` skill (whether a differently-authored
/// claude-mpm catalog skill sharing an old trusty-mpm name, per the fork
/// model, or a skill from an entirely different tool) or acting before the
/// tm-* replacements actually exist.
/// What: returns every [`FORMER_TRUSTY_MPM_SKILLS`] entry present as a
/// directory directly under `claude_skills_dir` whose origin
/// [`is_confirmed_trusty_mpm_origin`] can positively verify, but only when at
/// least one `tm-`-prefixed entry is also present there (the rename has
/// landed on this machine). Returns an empty vec when `claude_skills_dir`
/// does not exist. A name match alone is never sufficient — see
/// [`is_confirmed_trusty_mpm_origin`] for the verification precedence.
/// Test: `find_stale_mpm_skills_reports_known_former_skill`,
/// `find_stale_mpm_skills_ignores_unrelated_mpm_prefixed_dir`,
/// `find_stale_mpm_skills_ignores_before_tm_rename_lands`,
/// `find_stale_mpm_skills_missing_dir_is_empty`,
/// `find_stale_mpm_skills_ignores_differently_authored_same_name_skill`,
/// `find_stale_mpm_skills_confirms_origin_via_frozen_checksum_fallback`.
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

    let manifest = SkillManifest::load(claude_skills_dir);

    FORMER_TRUSTY_MPM_SKILLS
        .iter()
        .filter_map(|&name| {
            let path = claude_skills_dir.join(name);
            if !path.is_dir() || !is_confirmed_trusty_mpm_origin(name, &path, &manifest) {
                return None;
            }
            Some(StaleSkill {
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
    use crate::core::skill_manifest::SkillManifestEntry;
    use tempfile::TempDir;

    /// The exact pre-rename content trusty-mpm bundled for
    /// `mpm-git-file-tracking` (see `test_fixtures/README.md`), whose sha256
    /// is a real entry in `FORMER_TRUSTY_MPM_SKILL_CONTENT_SHA256`. Used to
    /// prove the checksum fallback matches genuine historical content, not a
    /// fabricated string engineered to satisfy the test.
    const FORMER_GIT_FILE_TRACKING_CONTENT: &str =
        include_str!("test_fixtures/former-mpm-git-file-tracking.md");

    /// Write a skill directory with arbitrary content but NO manifest entry —
    /// simulates a directory of unknown/unconfirmed origin (e.g. a legitimate
    /// claude-mpm catalog skill, or any file a test doesn't care to attribute).
    fn write_skill_with_content(dir: &Path, name: &str, content: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    fn write_skill(dir: &Path, name: &str) {
        write_skill_with_content(dir, name, &format!("---\nname: {name}\n---\n\n# {name}\n"));
    }

    /// Simulate a skill trusty-mpm's OWN deployer actually wrote: the
    /// directory content plus a matching [`SkillManifest`] entry, exactly as
    /// [`crate::core::skill_deployer::deploy_skills`] leaves behind. This is
    /// the "confirmed origin" fixture the origin-verification tests need —
    /// `write_skill`/`write_skill_with_content` alone are NOT enough to pass
    /// [`is_confirmed_trusty_mpm_origin`] since #1905's fork-model fix.
    fn write_confirmed_trusty_mpm_skill(claude_skills_dir: &Path, name: &str, content: &str) {
        write_skill_with_content(claude_skills_dir, name, content);
        let mut manifest = SkillManifest::load(claude_skills_dir);
        manifest.managed.insert(
            name.to_string(),
            SkillManifestEntry {
                checksum: checksum(content),
                deployed_at: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        manifest.save(claude_skills_dir).unwrap();
    }

    #[test]
    fn find_stale_mpm_skills_reports_known_former_skill() {
        let tmp = TempDir::new().unwrap();
        write_confirmed_trusty_mpm_skill(
            tmp.path(),
            "mpm-bug-reporting",
            "trusty-mpm's own old bundled content\n",
        );
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
        write_confirmed_trusty_mpm_skill(tmp.path(), "mpm-bug-reporting", "old content\n");

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
        write_confirmed_trusty_mpm_skill(tmp.path(), "mpm-bug-reporting", "old content a\n");
        write_confirmed_trusty_mpm_skill(tmp.path(), "mpm-pr-workflow", "old content b\n");
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
            write_confirmed_trusty_mpm_skill(
                tmp.path(),
                name,
                &format!("old content for {name}\n"),
            );
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
    fn find_stale_mpm_skills_ignores_differently_authored_same_name_skill() {
        // Regression guard (owner clarification on the fork model, #1905):
        // `/mpm-*` is claude-mpm's own harness-agnostic namespace, not
        // something trusty-mpm owns outright — `/tm-*` is a FORK of a subset
        // of it, and general skills keep being sourced from the separate
        // claude-mpm catalog indefinitely. A `~/.claude/skills/mpm-<name>/`
        // directory whose name is allowlisted but whose content trusty-mpm
        // never deployed (no manifest entry, AND content that doesn't match
        // the frozen pre-rename checksum) must survive the migration
        // untouched — it is presumably a legitimate claude-mpm-sourced skill
        // that only coincidentally shares one of trusty-mpm's old names.
        let tmp = TempDir::new().unwrap();
        write_skill_with_content(
            tmp.path(),
            "mpm-session-management",
            "# mpm-session-management\n\nThis is claude-mpm's own harness-agnostic \
             skill content — trusty-mpm's deployer never wrote this file.\n",
        );
        write_skill(tmp.path(), "tm-session-management"); // the rename has landed

        let stale = find_stale_mpm_skills(tmp.path());
        assert!(
            stale.is_empty(),
            "a same-named but differently-authored skill must never be flagged as stale: {stale:?}"
        );
        assert!(tmp.path().join("mpm-session-management").is_dir());
    }

    #[test]
    fn find_stale_mpm_skills_ignores_manifest_entry_with_mismatched_checksum() {
        // A managed entry whose checksum does NOT match the current content
        // means something has overwritten trusty-mpm's own deploy since —
        // this must be treated as unconfirmed, not fall through to the
        // frozen-checksum fallback (which wouldn't match this fabricated
        // content anyway, but the manifest branch must not short-circuit
        // that check incorrectly).
        let tmp = TempDir::new().unwrap();
        write_confirmed_trusty_mpm_skill(tmp.path(), "mpm-bug-reporting", "original content\n");
        // Overwrite the on-disk content without updating the manifest —
        // simulates a claude-mpm catalog sync (or manual edit) replacing it.
        std::fs::write(
            tmp.path().join("mpm-bug-reporting").join("SKILL.md"),
            "replaced content from elsewhere\n",
        )
        .unwrap();
        write_skill(tmp.path(), "tm-bug-reporting");

        let stale = find_stale_mpm_skills(tmp.path());
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_mpm_skills_confirms_origin_via_frozen_checksum_fallback() {
        // Proves the fallback path: a candidate with NO manifest entry (a
        // deploy predating the manifest system) is still confirmed as
        // trusty-mpm's own when its content is byte-identical to the frozen
        // pre-rename checksum table. Uses real historical content (see
        // `test_fixtures/README.md`), not a fabricated string, so this test
        // would fail if the frozen checksum table ever drifted from what
        // trusty-mpm actually shipped.
        let tmp = TempDir::new().unwrap();
        write_skill_with_content(
            tmp.path(),
            "mpm-git-file-tracking",
            FORMER_GIT_FILE_TRACKING_CONTENT,
        );
        write_skill(tmp.path(), "tm-git-file-tracking"); // the rename has landed

        let stale = find_stale_mpm_skills(tmp.path());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "mpm-git-file-tracking");
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
        write_confirmed_trusty_mpm_skill(&skills, "mpm-bug-reporting", "old content\n");
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

        // Simulate a stale directory reappearing after the migration ran
        // (with a confirmed-origin fixture, so this test isolates the
        // marker's short-circuit behavior from the origin check).
        write_confirmed_trusty_mpm_skill(&skills, "mpm-bug-reporting", "old content\n");
        run_stale_mpm_skills_migration_once(&root, &skills);

        // The second run must be a no-op: the marker short-circuits it.
        assert!(skills.join("mpm-bug-reporting").exists());
    }
}
