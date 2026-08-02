//! Audit deployed skills against the RUNNING BINARY'S OWN bundled asset
//! (issue #4604).
//!
//! Why: [`crate::core::skill_staleness::stale_skills`] compares
//! `FrameworkPaths::skill_source_dir()` — which for every binary-only install
//! resolves to `~/.trusty-mpm/framework/skills/`, an EXTRACTION CACHE — against
//! the checksum recorded in the deploy manifest. Both sides of that comparison
//! can be the same stale content. Measured 2026-08-01: PR #4583 rewrote the
//! bundled `tm-workflow` skill and shipped in `trusty-mpm 1.3.1`, but the cache
//! had not refreshed since before the merge, so its sha256 was byte-identical
//! to the checksum already in the deployed manifest. The check compared stale
//! against stale, correctly reported "no drift", and named only two OTHER
//! skills — which read as an all-clear for `tm-workflow` while all three
//! deployed copies still carried the removed text. A check that misses a
//! drifted skill while flagging others is worse than no check.
//!
//! Two structural changes close it:
//!
//! 1. **The reference is the binary's own embedded asset**, never the
//!    extraction cache. The cache is derived state; the compiled-in
//!    `bundle::ALL` table cannot lag the binary running it. The one exception is
//!    a populated `agents/skills` git submodule, which is version-controlled and
//!    authoritative on its own (see `core::skill_source`) — that source is used
//!    when present and named explicitly in the report.
//! 2. **The comparison reads the DEPLOYED FILE**, not the manifest checksum.
//!    The manifest records what tm last wrote; reading it instead of the file
//!    cannot see a hand-edit, and it is the manifest's agreement with the file
//!    that distinguishes "drifted, a redeploy fixes it" from "drifted and
//!    FROZEN, the redeploy will deliberately skip it". Those are different
//!    states with different remediation.
//!
//! UNVERIFIABLE REPORTS UNKNOWN: a managed skill with no embedded counterpart,
//! or a deployed file that cannot be read, is [`SkillDrift::Unverifiable`] and
//! never counted as fresh.
//!
//! Test: `skill_drift_tests.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::bundle;
use crate::core::skill_manifest::SkillManifest;

/// Entry-point filename Claude Code reads inside a deployed skill directory.
const SKILL_ENTRY_FILE: &str = "SKILL.md";

/// What the deployed copy of one skill is, relative to the running binary.
///
/// Why: `tm doctor` previously had one bit (stale / not stale) and could
/// therefore not express the two states the #4604 comment thread demanded be
/// separated — a drift a redeploy repairs versus a drift a redeploy will
/// deliberately skip — nor the unverifiable state that must never render as
/// healthy.
/// What: five states, ordered by how much they should worry an operator.
/// Test: `skill_drift_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDrift {
    /// The deployed file is byte-identical to the reference asset.
    Fresh,
    /// The deployed file differs from the reference, and still matches the
    /// checksum tm recorded when it deployed it — so tm owns it and the next
    /// `tm install` will refresh it.
    Drifted,
    /// The deployed file differs from the reference AND from the checksum tm
    /// recorded — it was hand-edited after deployment, so the deployer's
    /// "checksum differs → user-modified → skip" rule freezes it. A redeploy
    /// will NOT fix this one; it can stay stale forever with no other signal.
    DriftedFrozen,
    /// The manifest says tm deployed this skill here, but the file is gone.
    Missing,
    /// The comparison could not be made; the string says why.
    Unverifiable(String),
}

impl SkillDrift {
    /// Is this state anything other than a clean match?
    ///
    /// Why: callers summarise "how many skills are not fresh" before deciding a
    /// severity; folding the test into the type keeps every call site agreeing
    /// on what counts.
    /// What: `false` only for [`Fresh`](Self::Fresh).
    /// Test: `drift_states_report_not_fresh`.
    pub fn is_problem(&self) -> bool {
        !matches!(self, SkillDrift::Fresh)
    }
}

/// One skill's audit result at one deploy target.
///
/// Why: every finding must name the skill so remediation can be targeted, and
/// the state so the message can prescribe the right remedy.
/// What: the skill stem plus its [`SkillDrift`].
/// Test: `skill_drift_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDriftFinding {
    /// Skill stem (e.g. `tm-workflow`), the manifest key and directory name.
    pub stem: String,
    /// What the deployed copy is, relative to the reference asset.
    pub state: SkillDrift,
}

/// Where the authoritative skill text came from, for the report.
///
/// Why: "compared against the binary's embedded assets" and "compared against
/// the checked-out `agents/skills` submodule" are different claims, and an
/// operator reading a drift report needs to know which one was made — the whole
/// #4604 defect was a reference point nobody named.
/// What: a short label rendered into the doctor message.
/// Test: `reference_prefers_the_submodule`, `reference_falls_back_to_embedded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillReference {
    /// stem → authoritative content.
    pub assets: BTreeMap<String, String>,
    /// Human-readable description of where `assets` came from.
    pub origin: String,
}

/// Build the skill reference from the RUNNING BINARY, never from the cache.
///
/// Why: this is the #4604 fix in one function. `skill_source_dir()` resolves to
/// the `~/.trusty-mpm/framework/skills/` extraction cache for every binary-only
/// install, and that cache is exactly what lagged the installed binary. The
/// compiled-in table cannot lag the binary that contains it.
/// What: when `submodule_source` is `Some` (a populated, git-tracked
/// `agents/skills` checkout — the one source that legitimately outranks the
/// embedded table, per `core::skill_source`) its top-level `*.md` files are the
/// reference. Otherwise every `skills/<stem>.md` entry of [`bundle::ALL`] is —
/// nested `references/*.md` artifacts are excluded because they are not
/// manifest-keyed skills. A submodule directory that cannot be read falls back
/// to the embedded table rather than yielding an empty reference, since an empty
/// reference would make every skill unverifiable.
/// Test: `reference_prefers_the_submodule`, `reference_falls_back_to_embedded`,
/// `reference_excludes_nested_reference_files`.
pub fn skill_reference(submodule_source: Option<&Path>) -> SkillReference {
    if let Some(dir) = submodule_source
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        let mut assets = BTreeMap::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".md") else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                assets.insert(stem.to_string(), content);
            }
        }
        if !assets.is_empty() {
            return SkillReference {
                assets,
                origin: format!(
                    "the checked-out `agents/skills` submodule at {}",
                    dir.display()
                ),
            };
        }
    }

    let assets = bundle::ALL
        .iter()
        .filter_map(|a| {
            let rel = a.rel_path.strip_prefix("skills/")?;
            // Only a skill's own entry point is manifest-keyed; a multi-file
            // skill's `<stem>/references/<file>.md` siblings are not skills.
            if rel.contains('/') {
                return None;
            }
            let stem = rel.strip_suffix(".md")?;
            Some((stem.to_string(), a.contents.to_string()))
        })
        .collect();
    SkillReference {
        assets,
        origin: "this binary's embedded bundled assets".to_string(),
    }
}

/// Audit every tm-managed skill deployed under `dest_dir`.
///
/// Why: see the module doc. This reads the FILE, so it sees hand-edits the
/// manifest-only comparison structurally could not, and compares against
/// `reference`, so a stale extraction cache cannot make a drifted skill report
/// clean.
/// What: for each stem the deploy manifest at `dest_dir` manages —
/// - no reference asset for that stem → [`SkillDrift::Unverifiable`] (a
///   user-tier or renamed skill this binary does not ship);
/// - `<dest_dir>/<stem>/SKILL.md` absent → [`SkillDrift::Missing`];
/// - unreadable → [`SkillDrift::Unverifiable`];
/// - content equals the reference → [`SkillDrift::Fresh`];
/// - content still matches the manifest checksum → [`SkillDrift::Drifted`];
/// - otherwise → [`SkillDrift::DriftedFrozen`].
///
/// Findings are returned sorted by stem for stable output. An empty manifest
/// (nothing ever deployed here) yields an empty vector — there is nothing to be
/// stale relative to, which is a real answer rather than an unverifiable one.
/// Test: `skill_drift_tests.rs`.
pub fn audit_deployed_skills(
    reference: &SkillReference,
    dest_dir: &Path,
) -> Vec<SkillDriftFinding> {
    let manifest = SkillManifest::load(dest_dir);
    let mut findings: Vec<SkillDriftFinding> = manifest
        .managed
        .keys()
        .map(|stem| SkillDriftFinding {
            stem: stem.clone(),
            state: classify(reference, &manifest, dest_dir, stem),
        })
        .collect();
    findings.sort_by(|a, b| a.stem.cmp(&b.stem));
    findings
}

/// Classify one managed stem. See [`audit_deployed_skills`] for the rules.
fn classify(
    reference: &SkillReference,
    manifest: &SkillManifest,
    dest_dir: &Path,
    stem: &str,
) -> SkillDrift {
    let Some(expected) = reference.assets.get(stem) else {
        return SkillDrift::Unverifiable(format!(
            "`{stem}` is not among {} — this binary ships no copy to compare against",
            reference.origin
        ));
    };

    let path = dest_dir.join(stem).join(SKILL_ENTRY_FILE);
    let deployed = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SkillDrift::Missing,
        Err(e) => {
            return SkillDrift::Unverifiable(format!(
                "`{}` could not be read: {e}",
                path.display()
            ));
        }
    };

    if deployed == *expected {
        return SkillDrift::Fresh;
    }
    if manifest.checksum_matches(stem, &deployed) {
        // tm still owns the file — the next deploy overwrites it.
        SkillDrift::Drifted
    } else {
        // The file no longer matches what tm wrote, so `deploy_skills`'
        // "checksum differs → user-modified → skip" rule will never touch it.
        SkillDrift::DriftedFrozen
    }
}

#[cfg(test)]
#[path = "skill_drift_tests.rs"]
mod tests;
