//! Explicit, human-initiated adoption of unreachable bundled skills (#4605).
//!
//! Why: [`crate::skills::unmanaged`] reports bundled-named skills a deploy
//! target does not manage; reporting alone leaves the operator with no way to
//! fix them short of hand-deleting files. This module is the repair half —
//! deliberately NOT automatic. Adoption overwrites a file tm cannot prove it
//! wrote, so it runs only when a human asks for it by name, and it backs up
//! every file it touches first.
//!
//! What: [`adopt_unmanaged_bundled_skills`] copies each unmanaged bundled
//! skill directory under a backup root, then records its files in the deploy
//! target's manifest with the checksum of the content ALREADY ON DISK. That
//! single act is what unsticks the pipeline: the stem stops being classified
//! project-custom by [`crate::skills::tiers::list_project_custom_stems`], so
//! the next [`crate::skills::tiers::deploy_all_skill_tiers`] plans it into
//! `bundled_deploy`, and [`crate::skills::deployer::deploy_one_file`] then
//! sees "managed, checksum matches, content differs" — its existing safe-
//! refresh branch. No new write path, no new ownership rule; the adoption is
//! the whole fix and the refresh is the machinery that already worked.
//!
//! CONTRACT — a stem matching nothing bundled is NEVER touched. The roster is
//! the only admission test, and it is applied by
//! [`crate::skills::unmanaged::unmanaged_bundled_skills`], not re-derived
//! here, so report and repair cannot drift on which files are in scope.
//! Content similarity is deliberately not a test: an untracked file identical
//! to a shipped version is indistinguishable from a customization that started
//! from it.
//!
//! Test: `reconcile_tests.rs`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::agents::manifest::{Result, checksum};
use crate::skills::manifest::{SkillManifest, SkillManifestEntry};
use crate::skills::unmanaged::{UnmanagedBundledSkill, unmanaged_bundled_skills};

/// Filename of the per-backup-root ledger of what was copied where.
///
/// Why: matches the `moved-paths.log` this repository's existing
/// `~/.trusty-mpm/backup-doctor-remediation-<date>/` backups already carry, so
/// an operator finds the same artifact in the same place rather than learning
/// a second convention.
/// What: `moved-paths.log`, appended one `BACKUP: <src> -> <dst>` line per file.
/// Test: `adopt_writes_the_backup_ledger`.
pub const BACKUP_LEDGER_FILE: &str = "moved-paths.log";

/// One skill adopted into a deploy target's manifest.
///
/// Why: the caller prints exactly what was touched, per file, per tier — so
/// the stem, where its pre-adoption copy went, and the manifest keys now
/// tracking it all travel together.
/// What: the stem, the deployed directory, the backup directory the originals
/// were copied to, and the manifest keys inserted (entry point first).
/// Test: `adopt_registers_the_skill_and_its_references`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedSkill {
    /// The skill name.
    pub stem: String,
    /// `<dest>/<stem>` — the deployed directory now under management.
    pub dir: PathBuf,
    /// Where the pre-adoption copies were written.
    pub backup_dir: PathBuf,
    /// Manifest keys inserted, in `<stem>`-then-`<stem>/references/<file>` order.
    pub adopted_keys: Vec<String>,
}

/// Adopt every unmanaged bundled-named skill at `dest` into its manifest,
/// backing each one up under `backup_root` first.
///
/// Why: see the module doc — this is the only path that makes an untracked
/// bundled skill reachable again, and it must never run without a human
/// asking. Backing up before recording ownership means the operator can always
/// recover a customization the roster match happened to catch.
/// What: finds the in-scope skills with
/// [`unmanaged_bundled_skills`] (the roster is the ONLY admission test),
/// copies each file to `backup_root/<mirrored absolute path>`, appends a
/// [`BACKUP_LEDGER_FILE`] line per copy, then inserts a
/// [`SkillManifestEntry`] per file whose checksum is that of the CONTENT
/// CURRENTLY ON DISK — never the source's — so the deployer's existing
/// managed-and-unmodified branch decides the refresh. Saves the manifest once
/// at the end, atomically. Returns the adopted skills sorted by stem; an empty
/// result means nothing was in scope and NOTHING was written (no backup root,
/// no manifest rewrite). Adoption does not itself rewrite any skill content —
/// run a deploy afterwards to pull the current bundled text in.
/// Test: `adopt_registers_the_skill_and_its_references`,
/// `adopt_backs_up_before_recording`, `adopt_leaves_an_operator_skill_alone`,
/// `adopt_then_deploy_refreshes_a_stale_skill`, `adopt_no_findings_writes_nothing`.
pub fn adopt_unmanaged_bundled_skills(
    dest: &Path,
    bundled: &BTreeSet<String>,
    backup_root: &Path,
) -> Result<Vec<AdoptedSkill>> {
    let found = unmanaged_bundled_skills(dest, bundled);
    if found.is_empty() {
        return Ok(Vec::new());
    }

    let mut manifest = SkillManifest::load(dest);
    let now = chrono::Utc::now().to_rfc3339();
    let mut adopted = Vec::with_capacity(found.len());

    for skill in &found {
        let backup_dir = backup_target(backup_root, &skill.dir);
        let mut adopted_keys = Vec::with_capacity(skill.files.len());

        for (key, file) in skill.manifest_keys().into_iter().zip(&skill.files) {
            let content = std::fs::read_to_string(file)?;
            back_up(backup_root, file)?;
            manifest.managed.insert(
                key.clone(),
                SkillManifestEntry {
                    checksum: checksum(&content),
                    deployed_at: now.clone(),
                },
            );
            adopted_keys.push(key);
        }

        adopted.push(AdoptedSkill {
            stem: skill.stem.clone(),
            dir: skill.dir.clone(),
            backup_dir,
            adopted_keys,
        });
    }

    manifest.save(dest)?;
    Ok(adopted)
}

/// Preview what [`adopt_unmanaged_bundled_skills`] would touch at `dest`.
///
/// Why: the operator sees the file list before anything is written, and the
/// preview must be produced by the SAME scan the adoption runs — a separately
/// derived preview is a preview of a different command.
/// What: [`unmanaged_bundled_skills`] verbatim. Read-only.
/// Test: `preview_matches_what_adoption_touches`.
pub fn preview_unmanaged_bundled_skills(
    dest: &Path,
    bundled: &BTreeSet<String>,
) -> Vec<UnmanagedBundledSkill> {
    unmanaged_bundled_skills(dest, bundled)
}

/// Where `source`'s backup copy lives under `backup_root`.
///
/// Why: two deploy tiers can hold a same-named skill (`~/.claude/skills/tm-x`
/// and `$CLAUDE_CONFIG_DIR/skills/tm-x`), so a flat backup directory would
/// have the second silently clobber the first. Mirroring the source path makes
/// collisions impossible and the origin of every backup obvious.
/// What: `backup_root` joined with `source`'s components minus any root
/// prefix — `/Users/x/.claude/skills/tm-x` becomes
/// `<backup_root>/Users/x/.claude/skills/tm-x`. A relative `source` is joined
/// as-is.
/// Test: `backup_target_mirrors_the_absolute_path`.
fn backup_target(backup_root: &Path, source: &Path) -> PathBuf {
    let relative: PathBuf = source
        .components()
        .filter(|c| !matches!(c, std::path::Component::RootDir))
        .collect();
    backup_root.join(relative)
}

/// Copy one file under `backup_root` and record it in the backup ledger.
///
/// Why: the copy is what makes adoption recoverable; the ledger line is what
/// makes it findable months later without diffing directory trees.
/// What: creates the mirrored parent directory, copies `source` to
/// [`backup_target`], then appends `BACKUP: <source> -> <target>` to
/// `<backup_root>/`[`BACKUP_LEDGER_FILE`]. Errors propagate — a backup that
/// cannot be written must abort the adoption, never proceed without it.
/// Test: `adopt_backs_up_before_recording`, `adopt_writes_the_backup_ledger`.
fn back_up(backup_root: &Path, source: &Path) -> Result<()> {
    let target = backup_target(backup_root, source);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, &target)?;

    std::fs::create_dir_all(backup_root)?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(backup_root.join(BACKUP_LEDGER_FILE))?;
    writeln!(log, "BACKUP: {} -> {}", source.display(), target.display())?;
    Ok(())
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
