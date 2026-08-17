//! Bundled-named skills a deploy target does not manage (issue #4605).
//!
//! Why: [`crate::skills::tiers::list_project_custom_stems`] classifies EVERY
//! untracked skill directory in a deploy target as project-custom — the
//! highest-precedence tier — and [`crate::skills::tiers::plan_skill_tiers`]
//! then drops it from `bundled_deploy` before
//! [`crate::skills::deployer::deploy_skills_filtered`] ever runs. For a skill
//! the operator genuinely authored that is correct and load-bearing. For a
//! BUNDLED skill that merely lost (or predates) manifest tracking it is a
//! permanent stall: the deploy loop never runs for that stem, so the ownership
//! check never gets to distinguish "the operator's" from "orphaned bundled",
//! and the file is unreachable by every `tm` command. Measured on 2026-08-01:
//! `tm install` printed only `= tm-slack-canvas-delivery (unchanged)` while
//! two `tm-workflow/SKILL.md` copies (mtime 07-03 and 07-17) kept serving text
//! PR #4583 had removed — a fix that merged, shipped in a signed binary, and
//! passed 24 CI checks reached zero sessions.
//!
//! What: the read-only detector for that state. [`unmanaged_bundled_skills`]
//! returns the skill directories in one deploy target whose stem names a
//! CURRENTLY BUNDLED skill and which that target's
//! [`SkillManifest`](crate::skills::manifest::SkillManifest) does not manage.
//! A stem matching nothing bundled is never returned — that is the operator's
//! own skill and the exclusion is the whole point of the tier system.
//!
//! CONTRACT — content is NOT evidence of ownership. This module deliberately
//! offers no "adopt anything that looks like ours" predicate. An untracked
//! file byte-identical to a bundled version is indistinguishable from a
//! deliberate operator customization that happens to start from the shipped
//! text, and silently overwriting the latter is exactly the harm the tier
//! system exists to prevent. Detection here is REPORT-ONLY; adoption lives in
//! [`crate::skills::reconcile`] behind an explicit, human-initiated command
//! that backs up every file it touches.
//!
//! Test: `unmanaged_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::agents::manifest::Result;
use crate::skills::manifest::SkillManifest;

/// The entry-point filename Claude Code discovers a skill directory by.
///
/// Why: the deployer, this detector, and the reconcile pass must agree on the
/// one filename that makes `<dest>/<stem>/` a skill rather than an arbitrary
/// directory; naming it once keeps them from drifting.
/// What: `SKILL.md`.
/// Test: `unmanaged_finds_a_bundled_named_untracked_skill`.
pub const SKILL_ENTRY_POINT: &str = "SKILL.md";

/// One deployed skill directory that carries a bundled skill's name but is
/// absent from its deploy target's ownership manifest.
///
/// Why: the reporter needs the stem (to name it), the directory (to point the
/// operator at it), and the exact files involved (so a reconcile prints what
/// it will touch before touching it) — and both consumers must see the same
/// three, so they travel together.
/// What: the skill stem, `<dest>/<stem>`, and every managed-shaped file under
/// it — the `SKILL.md` entry point first, then each `references/*.md` sibling,
/// sorted. `files` is never empty: a directory without a `SKILL.md` is not a
/// skill and is never reported.
/// Test: `unmanaged_finds_a_bundled_named_untracked_skill`,
/// `unmanaged_lists_reference_files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmanagedBundledSkill {
    /// The skill name — the directory name under the deploy target.
    pub stem: String,
    /// `<dest>/<stem>` — the deployed skill directory.
    pub dir: PathBuf,
    /// The `SKILL.md` entry point followed by each `references/*.md` sibling.
    pub files: Vec<PathBuf>,
}

impl UnmanagedBundledSkill {
    /// The manifest keys this skill's files would occupy if adopted.
    ///
    /// Why: [`crate::skills::deployer`] keys the entry point by the bare stem
    /// and each reference file by `<stem>/references/<file>`; the reconcile
    /// pass has to insert exactly those keys or the next deploy still skips
    /// the file. Deriving them here — from the same `files` the reporter
    /// printed — keeps report and repair keyed identically.
    /// What: `<stem>` for the entry point, `<stem>/references/<file>` for each
    /// reference file, in `files` order.
    /// Test: `manifest_keys_match_the_deployer_key_shape`.
    pub fn manifest_keys(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name == SKILL_ENTRY_POINT {
                    self.stem.clone()
                } else {
                    format!("{}/references/{name}", self.stem)
                }
            })
            .collect()
    }
}

/// Scan one deploy target for bundled-named skills it does not manage.
///
/// Why: this is the signal the tier planner swallows. The planner's exclusion
/// is correct (never overwrite an untracked file) but silent, and silence is
/// the defect — an operator has no way to learn that a bundled skill fix is
/// not reaching this tier. Reporting it is the floor fix for issue #4605
/// regardless of whether any adoption path is offered.
/// What: READ-ONLY. Lists directories directly under `dest` (never recursing
/// past a skill's own `references/`), keeps those that (1) contain a
/// [`SKILL_ENTRY_POINT`], (2) have a stem `bundled` contains, and (3) are NOT
/// managed by `dest`'s [`SkillManifest`], and returns them sorted by stem.
/// A missing or unreadable `dest` yields an empty vec — an unprovisioned tier
/// is not a finding. An EMPTY `bundled` set likewise yields nothing: callers
/// must treat that as "cannot classify by name", never as "nothing is
/// bundled", since the latter reading would condemn every skill at once. A
/// LEDGER that cannot be read is an `Err` (#5626): "untracked" is a claim about
/// what the ledger says, and an unreadable ledger says nothing — reporting
/// every bundled skill in the tier as unmanaged is the same false positive at
/// the scale of the whole tier.
/// Test: `unmanaged_finds_a_bundled_named_untracked_skill`,
/// `unmanaged_ignores_a_managed_skill`, `unmanaged_ignores_an_operator_skill`,
/// `unmanaged_missing_dest_is_empty`, `unmanaged_lists_reference_files`.
pub fn unmanaged_bundled_skills(
    dest: &Path,
    bundled: &BTreeSet<String>,
) -> Result<Vec<UnmanagedBundledSkill>> {
    let manifest = SkillManifest::load_checked(dest)?;
    Ok(bundled_skill_dirs(dest, bundled)
        .into_iter()
        .filter(|skill| !manifest.is_managed(&skill.stem))
        .collect())
}

/// Every bundled-named skill directory at `dest`, managed or not.
///
/// Why: [`unmanaged_bundled_skills`] answers "which bundled skills has this
/// target lost track of?", but `tm reinstall --force` asks the wider question —
/// "which bundled skills is the deployer declining to refresh here, for ANY
/// reason?" — which also covers a MANAGED file hand-edited away from its
/// recorded checksum. Both questions walk the same directory shape, so the walk
/// lives here once and each caller applies its own manifest predicate.
/// What: READ-ONLY. Same scan [`unmanaged_bundled_skills`] documents, minus the
/// ownership filter: directories directly under `dest` whose stem is in
/// `bundled` and which contain a [`SKILL_ENTRY_POINT`], sorted by stem. A
/// missing or unreadable `dest`, or an empty `bundled` set, yields an empty vec.
/// Test: `bundled_skill_dirs_includes_a_managed_skill`,
/// `unmanaged_ignores_a_managed_skill`.
pub fn bundled_skill_dirs(dest: &Path, bundled: &BTreeSet<String>) -> Vec<UnmanagedBundledSkill> {
    let Ok(entries) = std::fs::read_dir(dest) else {
        return Vec::new();
    };

    let mut found: Vec<UnmanagedBundledSkill> = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let stem = entry.file_name().to_str()?.to_owned();
            if !bundled.contains(&stem) {
                return None;
            }
            let dir = entry.path();
            let entry_point = dir.join(SKILL_ENTRY_POINT);
            if !entry_point.is_file() {
                return None;
            }
            let mut files = vec![entry_point];
            files.extend(reference_files(&dir));
            Some(UnmanagedBundledSkill { stem, dir, files })
        })
        .collect();
    found.sort_by(|a, b| a.stem.cmp(&b.stem));
    found
}

/// List a deployed skill's `references/*.md` sibling files, sorted.
///
/// Why: a multi-file skill's reference subtree is deployed under the same
/// ownership rule as its entry point (issue #2903), so a reconcile that
/// adopted only the entry point would leave the references permanently stale —
/// the identical defect one level down.
/// What: every `.md` file directly under `<dir>/references/`, sorted by path.
/// A missing subtree yields an empty vec.
/// Test: `unmanaged_lists_reference_files`.
fn reference_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("references")) else {
        return Vec::new();
    };
    let mut refs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.file_name()
                    .to_str()
                    .is_some_and(crate::skills::deployer::is_skill_file)
        })
        .map(|e| e.path())
        .collect();
    refs.sort();
    refs
}

#[cfg(test)]
#[path = "unmanaged_tests.rs"]
mod tests;
