//! Enumerate the skills a source directory offers, in either supported layout.
//!
//! Why (#4949): the deployer and the tier planner each decide "what is a skill
//! here?" and both decided it with `entry.file_type()?.is_file() &&
//! is_skill_file(name)` — a flat `<stem>.md` at the source root, and nothing
//! else. A directory-shaped skill (`<stem>/SKILL.md` plus whatever it carries)
//! failed that `is_file()` test and was dropped with no warning on every
//! deploy: `deploy_skills_filtered` reported success having never seen it.
//! Bundled skills escaped the bug only because they ship a HYBRID layout — a
//! flat `tm-capabilities.md` entry point beside a `tm-capabilities/references/`
//! companion directory — so the entry point still satisfied `is_file()` and the
//! companion directory was reached afterwards, keyed off the stem. There was no
//! working directory-skill path to route the user tier through; both tiers read
//! the same blind scan. Two real user skills sat hidden behind it for weeks:
//! `duetto-design-system` and `cto-kb-ingest`.
//!
//! What: [`scan_skill_sources`] returns one [`SourceSkill`] per skill in
//! `source`, accepting BOTH layouts, plus the names it had to reject so the
//! caller can report them instead of dropping them. Every non-hidden file a
//! skill's companion directory carries — `metadata.json`, `references/**`,
//! `scripts/**`, at any depth — travels with it as an [`SourceExtra`], keyed
//! `<stem>/<relative-path>`. That key scheme is byte-identical to the
//! `<stem>/references/<file>` keys the previous reference-mirroring code wrote,
//! so existing manifests keep matching and no skill re-deploys spuriously.
//! This module is the ONE place that answers the question, per this repo's
//! common-entry-point rule; [`crate::skills::deployer`] and
//! [`crate::skills::tiers`] both call it rather than re-deriving it.
//!
//! Test: `scan_finds_directory_shaped_skill`, `scan_finds_flat_skill`,
//! `scan_companion_dir_is_not_a_reject`, `scan_rejects_dir_without_skill_md`,
//! `scan_ignores_hidden_entries`, `scan_extras_are_recursive_and_sorted`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::manifest::Result;
use crate::skills::deployer::{is_skill_file, skill_stem};

/// One extra file a skill carries alongside its entry point.
///
/// Why: the manifest keys every managed file individually, so each companion
/// file needs a stable relative identity independent of the absolute source
/// path it was read from.
/// What: `rel` is the `/`-joined path below the skill's own directory (e.g.
/// `references/design-tokens.md`, `scripts/ingest.sh`); `path` is where to read
/// it from.
/// Test: `scan_extras_are_recursive_and_sorted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceExtra {
    /// Path below the skill directory, `/`-separated.
    pub rel: String,
    /// Absolute (or source-relative) path to read the content from.
    pub path: PathBuf,
}

/// One deployable skill discovered in a source directory.
///
/// Why: the deployer needs the entry-point content and the carried files as one
/// unit so a multi-file skill cannot half-deploy, and the tier planner needs the
/// stem from the identical scan so it never greenlights a stem the deployer
/// then rejects (the drift PR #2818's review already caught once).
/// What: the skill name, the file whose content becomes `<dest>/<stem>/SKILL.md`,
/// and every extra file it carries.
/// Test: `scan_finds_directory_shaped_skill`, `scan_finds_flat_skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSkill {
    /// The skill name — manifest key and destination directory name.
    pub stem: String,
    /// Source of the `SKILL.md` content.
    pub entry: PathBuf,
    /// Companion files, sorted by `rel` for deterministic output.
    pub extras: Vec<SourceExtra>,
}

/// Everything one source directory offers, including what it could not offer.
///
/// Why (#4949): a name this scan cannot turn into a skill must reach the
/// caller. Returning only the successes is what made the original defect
/// invisible — there was no channel for "I saw this and could not use it".
/// What: the usable skills, sorted by stem, plus the rejected directory names,
/// sorted.
/// Test: `scan_rejects_dir_without_skill_md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceScan {
    /// Deployable skills, sorted by stem.
    pub skills: Vec<SourceSkill>,
    /// Directory names that look like a skill but carry no `SKILL.md`.
    pub rejected: Vec<String>,
}

/// Whether a directory entry name is hidden and must be ignored.
///
/// Why: `.git`, `.DS_Store`, and editor sidecars live beside real content and
/// must never be deployed or reported as a malformed skill.
/// What: true when the name starts with `.`.
/// Test: `scan_ignores_hidden_entries`.
fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// Enumerate the skills in `source`, accepting both supported layouts.
///
/// Why: see the module doc — one shared answer to "what is a skill here?",
/// covering the directory shape that #4949 dropped silently.
/// What: a flat `<stem>.md` file is a skill whose entry point is that file. A
/// non-hidden subdirectory containing `SKILL.md` is a skill whose entry point
/// is that `SKILL.md`. Either way, every non-hidden file under `<source>/<stem>/`
/// (except a directory skill's own `SKILL.md`) is carried as an extra. A
/// subdirectory that merely accompanies a flat skill of the same name is the
/// hybrid bundled layout, NOT a malformed skill, and is never rejected — every
/// bundled skill would otherwise warn on every deploy. Any other non-hidden
/// subdirectory without a `SKILL.md` is returned in `rejected`. A missing
/// `source` yields an empty scan; errors reading an existing directory
/// propagate.
/// Test: `scan_finds_directory_shaped_skill`, `scan_finds_flat_skill`,
/// `scan_companion_dir_is_not_a_reject`, `scan_rejects_dir_without_skill_md`.
pub fn scan_skill_sources(source: &Path) -> Result<SourceScan> {
    let mut scan = SourceScan::default();
    if !source.is_dir() {
        return Ok(scan);
    }

    // Two passes over one read_dir result: the flat entry points must all be
    // known before any directory is judged, because a directory whose name
    // matches a flat skill is that skill's companion, not a candidate.
    let mut flat: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut dirs: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if is_hidden(name) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_file() && is_skill_file(name) {
            flat.insert(skill_stem(name).to_string(), entry.path());
        } else if file_type.is_dir() {
            dirs.push(name.to_string());
        }
    }

    for (stem, entry) in &flat {
        scan.skills.push(SourceSkill {
            stem: stem.clone(),
            entry: entry.clone(),
            extras: collect_extras(&source.join(stem), None)?,
        });
    }

    dirs.sort_unstable();
    for name in dirs {
        if flat.contains_key(&name) {
            // Companion directory of a flat skill — already carried above.
            continue;
        }
        let dir = source.join(&name);
        if dir.join("SKILL.md").is_file() {
            scan.skills.push(SourceSkill {
                stem: name.clone(),
                entry: dir.join("SKILL.md"),
                extras: collect_extras(&dir, Some("SKILL.md"))?,
            });
        } else {
            scan.rejected.push(name);
        }
    }

    scan.skills.sort_by(|a, b| a.stem.cmp(&b.stem));
    Ok(scan)
}

/// Collect every non-hidden file under `dir`, recursively.
///
/// Why: a skill's carried files are not limited to `references/*.md` — the two
/// skills #4949 hid between them ship `metadata.json`, four `references/*.md`
/// docs, and a `scripts/ingest.sh`. Filtering to `.md` here would fix the
/// `is_file()` half of the defect and leave a second silent-drop in its place.
/// What: walks `dir` depth-first, returning `/`-joined relative paths and their
/// source paths, skipping hidden entries at every level and `skip_root` (a
/// directory skill's own entry point) at the top. A missing `dir` yields an
/// empty vector. Results are sorted by `rel`.
/// Test: `scan_extras_are_recursive_and_sorted`, `scan_ignores_hidden_entries`.
fn collect_extras(dir: &Path, skip_root: Option<&str>) -> Result<Vec<SourceExtra>> {
    let mut extras = Vec::new();
    collect_extras_into(dir, "", skip_root, &mut extras)?;
    extras.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(extras)
}

/// Recursive worker for [`collect_extras`].
///
/// Why: split out so the public helper owns the sort and the caller never has
/// to thread a prefix through.
/// What: appends each non-hidden file below `dir` to `out`, prefixing its name
/// with `prefix`. Errors reading an existing directory propagate.
/// Test: via [`collect_extras`]'s tests.
fn collect_extras_into(
    dir: &Path,
    prefix: &str,
    skip_root: Option<&str>,
    out: &mut Vec<SourceExtra>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if is_hidden(name) {
            continue;
        }
        if prefix.is_empty() && skip_root == Some(name) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_extras_into(&entry.path(), &rel, skip_root, out)?;
        } else if file_type.is_file() {
            out.push(SourceExtra {
                rel,
                path: entry.path(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_scan_tests.rs"]
mod tests;
