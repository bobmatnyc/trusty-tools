//! Two entries claiming ONE asset name inside ONE tier (#6649).
//!
//! Why: every existing tier probe compares a directory against another
//! directory. `asset_tier` asks whether a tm-owned agent sits in the wrong
//! tier; `skill_project_tier` asks whether a bundled skill lingers at the
//! project tier. Neither can see the collision that lives entirely INSIDE one
//! directory: `rust-engineer.md` beside a `rust-engineer/` directory, or
//! `QA.md` beside `qa.md` on a case-insensitive filesystem where only one of
//! them can ever load. Which entry wins is decided by loader order and, on
//! APFS, by which file was created first — so the operator reads one file and
//! the harness runs the other, with nothing reporting the split.
//!
//! What: [`scan_duplicate_stems`] groups one directory's entries by the name an
//! asset resolves under and returns every group holding more than one entry.
//! One implementation serves both kinds: an agent is `<stem>.md` and a skill is
//! `<stem>/SKILL.md`, so both key on the same normalised stem, and the
//! cross-shape collision (`foo.md` beside `foo/`) is the case worth catching.
//!
//! READ-ONLY, AND DELIBERATELY UNREPAIRED. tm cannot know which of two entries
//! the operator meant to keep — that is not an ownership question the ledger
//! can answer, because both entries may be the operator's own. So this reports
//! and stops; the operator deletes or renames one (owner ruling 2026-09-02).
//!
//! Test: `asset_duplicates_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Why [`scan_duplicate_stems`] could not answer for a directory.
///
/// Why (ADR-0045): an empty result renders as "this tier holds no duplicate",
/// which is a positive claim. A directory the process cannot enumerate must not
/// produce that claim — that is the #4605/#5626 fail-open shape, and #6649's
/// own fail-open deliverable forbids it.
/// What: one variant. An ABSENT directory is not an error: an unprovisioned
/// tier genuinely holds no duplicate, so only a non-`NotFound` `read_dir`
/// failure lands here.
/// Test: `an_unreadable_tier_is_an_error_not_an_empty_scan`,
/// `an_absent_tier_scans_clean`.
#[derive(Debug, Error)]
pub enum DuplicateScanError {
    /// The directory exists (or its state is unknown) and could not be listed.
    #[error("cannot scan asset tier {path}: {source}")]
    Unscannable {
        /// The directory the scan was asked for.
        path: PathBuf,
        /// The underlying `read_dir` failure.
        #[source]
        source: std::io::Error,
    },
}

/// One asset name claimed by more than one entry in a single tier.
///
/// Why: the operator needs the NAME to know which asset is ambiguous and the
/// PATHS to know which two files to reconcile. A count alone is unactionable.
/// What: the normalised stem and every entry that resolves to it, sorted.
/// `paths` always holds at least two entries — a group of one is not a
/// duplicate and is never constructed.
/// Test: `a_file_beside_a_directory_of_the_same_stem_is_a_duplicate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateStem {
    /// The name both entries resolve under, lowercased.
    pub stem: String,
    /// The colliding entries, sorted by path.
    pub paths: Vec<PathBuf>,
}

/// The name one directory entry resolves under, or `None` when it is not an
/// asset entry at all.
///
/// Why: the two kinds spell the same asset differently — `qa.md` for an agent,
/// `qa/` for a skill — so a raw file name groups them apart and the
/// cross-shape collision this module exists to find would never form a group.
/// Stripping one trailing `.md` is what makes the two shapes share a key.
/// Lowercasing is what catches `QA.md` beside `qa.md`: macOS and Windows
/// filesystems are case-INSENSITIVE, so those two names are one file to the
/// loader even though `read_dir` reports two entries on a case-sensitive volume.
/// What: `None` for a dot-entry — the deploy ledgers
/// (`.trusty-mpm-manifest.json`, `.trusty-mpm-skills-manifest.json`), the
/// project-tier stamp, and `.DS_Store` are bookkeeping, not assets, and two of
/// them collide by design. Otherwise the file name with at most one trailing
/// `.md` removed, lowercased.
/// Test: `dot_entries_never_form_a_group`,
/// `case_variant_stems_are_a_duplicate`,
/// `a_disabled_quarantine_sibling_is_not_a_duplicate`.
fn stem_key(file_name: &str) -> Option<String> {
    if file_name.starts_with('.') {
        return None;
    }
    Some(
        file_name
            .strip_suffix(".md")
            .unwrap_or(file_name)
            .to_lowercase(),
    )
}

/// Every asset name claimed by two or more entries of `dir`.
///
/// Why: see the module doc — this is the one collision shape that lives inside
/// a single tier, which every tier-vs-tier probe is structurally unable to see.
/// What: groups `dir`'s entries by [`stem_key`] and returns the groups of two
/// or more, sorted by stem, each group's paths sorted. An ABSENT `dir` scans
/// clean (`Ok(vec![])`); an unreadable one is
/// [`DuplicateScanError::Unscannable`], never an empty scan. Reads names only —
/// it opens no file and follows no symlink.
/// Test: `a_file_beside_a_directory_of_the_same_stem_is_a_duplicate`,
/// `case_variant_stems_are_a_duplicate`, `a_clean_tier_has_no_duplicates`,
/// `an_absent_tier_scans_clean`,
/// `an_unreadable_tier_is_an_error_not_an_empty_scan`.
pub fn scan_duplicate_stems(dir: &Path) -> Result<Vec<DuplicateStem>, DuplicateScanError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DuplicateScanError::Unscannable {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(key) = stem_key(&name) else { continue };
        groups.entry(key).or_default().push(entry.path());
    }

    Ok(groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(stem, mut paths)| {
            paths.sort();
            DuplicateStem { stem, paths }
        })
        .collect())
}

/// Render up to `max` duplicate stems as one comma-separated list.
///
/// Why: the doctor row and the launch notice name the same stems in the same
/// order, and two renderers would let the two disagree about which names an
/// operator sees.
/// What: the stems, comma-separated, with `(+N more)` when `found` is longer
/// than `max`. Empty string for an empty slice.
/// Test: `named_stems_summarise_the_remainder`.
pub fn name_duplicates(found: &[DuplicateStem], max: usize) -> String {
    let named: Vec<&str> = found.iter().take(max).map(|d| d.stem.as_str()).collect();
    let rest = found.len().saturating_sub(named.len());
    if rest == 0 {
        return named.join(", ");
    }
    format!("{} (+{rest} more)", named.join(", "))
}

#[cfg(test)]
#[path = "asset_duplicates_tests.rs"]
mod tests;
