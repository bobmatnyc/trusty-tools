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
//! never counted as fresh; a tier whose ownership ledger is absent-over-content
//! or unparseable yields the matching [`ManifestState`] and no findings at all,
//! because nothing there is attributable (#4622 review).
//!
//! Keys are the DEPLOY-MANIFEST keys, which come in two shapes — a bare
//! `<stem>` and a nested `<stem>/references/<file>.md`. Handling only the first
//! made 80 of this machine's 132 keys permanently unverifiable (#4622 review,
//! HIGH-1); [`deployed_path`] is the one resolver both the audit and the repair
//! use.
//!
//! Test: `skill_drift_tests.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::bundle;
use crate::core::skill_manifest::{SKILL_MANIFEST_FILE, SkillManifest};

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
    /// The deploy-manifest key: a bare stem (`tm-workflow`) for a skill's entry
    /// point, or `<stem>/references/<file>.md` for one of its reference
    /// siblings. Resolve it to a path with [`deployed_path`].
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
    /// Deploy-manifest key → authoritative content. Keys match
    /// [`SkillDriftFinding::stem`] exactly, including the nested
    /// `<stem>/references/<file>.md` form.
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
/// keyed EXACTLY as the deploy manifest keys them: a bare `<stem>` for an entry
/// point, and `<stem>/references/<file>.md` for each reference sibling
/// (#4622 review, HIGH-1 — dropping the nested entries made all 80 of this
/// machine's 132 manifest keys unverifiable, pinning `skill_staleness` to
/// Unknown on every real install). A submodule directory that cannot be read, or
/// yields nothing, falls back to the embedded table rather than producing an
/// empty reference, since an empty reference would make every skill
/// unverifiable at once.
/// Test: `reference_prefers_the_submodule`, `reference_falls_back_to_embedded`,
/// `reference_includes_nested_reference_keys`,
/// `a_pristine_bundled_deploy_is_entirely_fresh`.
pub fn skill_reference(submodule_source: Option<&Path>) -> SkillReference {
    if let Some(dir) = submodule_source {
        let mut assets = BTreeMap::new();
        collect_submodule_assets(dir, dir, &mut assets);
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
            // A bare `<stem>.md` entry point is keyed by its stem; a nested
            // `<stem>/references/<file>.md` sibling is keyed by that exact
            // relative path, because that is what `skills::deployer` records.
            let key = if rel.contains('/') {
                rel.to_string()
            } else {
                rel.strip_suffix(".md")?.to_string()
            };
            Some((key, a.contents.to_string()))
        })
        .collect();
    SkillReference {
        assets,
        origin: "this binary's embedded bundled assets".to_string(),
    }
}

/// Walk a checked-out `agents/skills` submodule into manifest-keyed assets.
///
/// Why: the submodule mirrors the bundled layout — flat `<stem>.md` entry points
/// beside `<stem>/references/<file>.md` subtrees — so it must produce the same
/// key shapes the embedded table does, or the submodule path would reintroduce
/// the #4622 HIGH-1 defect on dev checkouts only.
/// What: recurses from `root`, keying each `.md` file by its path relative to
/// `root` (with `.md` stripped for a top-level entry point). Hidden files and
/// unreadable entries are skipped, never guessed at.
/// Test: `reference_prefers_the_submodule`.
fn collect_submodule_assets(root: &Path, dir: &Path, assets: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_submodule_assets(root, &path, assets);
            continue;
        }
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let key = match rel.contains('/') {
            true => rel,
            false => rel.trim_end_matches(".md").to_string(),
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            assets.insert(key, content);
        }
    }
}

/// Whether a deploy tier's ownership manifest could be read at all.
///
/// Why (#4622 review, MEDIUM): [`SkillManifest::load`] returns an EMPTY manifest
/// for a file that is absent AND for one that fails to parse. Folding both into
/// "nothing managed here" made a tier with skills on disk but a corrupt ledger
/// report `Ok — every deployed skill matches`, which is the exact
/// unverifiable-rendered-as-healthy failure this PR exists to remove.
/// What: four states. Only [`Present`](Self::Present) and
/// [`AbsentAndEmpty`](Self::AbsentAndEmpty) are answers; the other two are
/// admissions that the tier could not be audited.
/// Test: `nothing_deployed_yields_no_findings`,
/// `absent_manifest_over_deployed_skills_is_unknown`,
/// `corrupt_manifest_is_unknown_not_empty`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestState {
    /// The manifest exists and parsed.
    Present,
    /// No manifest and no deployed content — nothing was ever deployed here.
    AbsentAndEmpty,
    /// No manifest, but the tier directory holds entries. Those files cannot be
    /// attributed to tm or to the operator, so nothing about them is verifiable.
    AbsentButPopulated,
    /// The manifest exists but could not be parsed; the string says why.
    Unreadable(String),
}

/// One deploy tier's audit: the ledger's own state plus the per-skill findings.
///
/// Why: the tier verdict depends on BOTH — a tier with zero drifted skills but
/// an unreadable ledger has not been shown to be clean.
/// What: [`manifest`](Self::manifest) is the ledger state,
/// [`findings`](Self::findings) the per-manifest-key results (empty whenever the
/// ledger could not be read).
/// Test: `skill_drift_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierAudit {
    /// Whether the ownership manifest could be read.
    pub manifest: ManifestState,
    /// Per-skill results, sorted by manifest key.
    pub findings: Vec<SkillDriftFinding>,
}

/// Resolve the on-disk path a deploy-manifest key refers to.
///
/// Why (#4622 review, HIGH-1): manifest keys come in TWO shapes and they do not
/// share a layout. `skills::deployer::deploy_skills` writes a bare stem's entry
/// point to `<dest>/<stem>/SKILL.md`, but records each reference sibling under
/// `<stem>/references/<file>.md` and writes it to that same relative path. One
/// resolver keeps the audit, the repair, and the backup from each inventing a
/// different answer.
/// What: a key containing `/` mirrors the source layout verbatim; a bare key
/// resolves to `<dest>/<key>/SKILL.md`.
/// Test: `deployed_path_matches_the_deployer_layout`.
pub fn deployed_path(dest_dir: &Path, manifest_key: &str) -> std::path::PathBuf {
    if manifest_key.contains('/') {
        dest_dir.join(manifest_key)
    } else {
        dest_dir.join(manifest_key).join(SKILL_ENTRY_FILE)
    }
}

/// The skill stem a manifest key belongs to.
///
/// Why: severity rules (the conventions-bearing escalation) are stated per
/// SKILL, but a key may name one of that skill's reference files.
/// What: the first path segment.
/// Test: `drift_in_a_reference_file_is_caught` (which asserts on the full key)
/// and `staleness_escalates_when_a_reference_file_of_a_conventions_skill_drifts`.
pub fn key_stem(manifest_key: &str) -> &str {
    manifest_key.split('/').next().unwrap_or(manifest_key)
}

/// Audit every tm-managed skill deployed under `dest_dir`.
///
/// Why: see the module doc. This reads the FILE, so it sees hand-edits the
/// manifest-only comparison structurally could not, and compares against
/// `reference`, so a stale extraction cache cannot make a drifted skill report
/// clean.
/// What: probes the ownership manifest first — an unreadable one, or an absent
/// one over a populated tier, yields the matching [`ManifestState`] and NO
/// findings, because nothing there is attributable. Otherwise, for each manifest
/// key —
/// - no reference asset for that key → [`SkillDrift::Unverifiable`];
/// - [`deployed_path`] absent → [`SkillDrift::Missing`];
/// - unreadable → [`SkillDrift::Unverifiable`];
/// - content equals the reference → [`SkillDrift::Fresh`];
/// - content still matches the manifest checksum → [`SkillDrift::Drifted`];
/// - otherwise → [`SkillDrift::DriftedFrozen`].
///
/// Findings are sorted by key for stable output.
/// Test: `skill_drift_tests.rs`.
pub fn audit_deployed_skills(reference: &SkillReference, dest_dir: &Path) -> TierAudit {
    let manifest_path = dest_dir.join(SKILL_MANIFEST_FILE);
    // #5626: the parsed document is CARRIED to the findings loop rather than
    // re-read there. The old second read went through `SkillManifest::load`,
    // whose empty-on-any-error default would have reported "no managed skills"
    // for a ledger this probe had just classified `Present`.
    let mut parsed: Option<SkillManifest> = None;
    let state = match std::fs::read_to_string(&manifest_path) {
        Ok(raw) => match serde_json::from_str::<SkillManifest>(&raw) {
            Ok(m) => {
                parsed = Some(m);
                ManifestState::Present
            }
            Err(e) => ManifestState::Unreadable(format!(
                "{} is not valid JSON: {e}",
                manifest_path.display()
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A tier directory that does not exist, or exists and is empty, was
            // simply never deployed to. One that holds entries WITHOUT a ledger
            // cannot be attributed at all.
            // #4881: "populated" means DEPLOYED SKILLS, not merely non-empty.
            // The ledger lock's sidecar (`.trusty-mpm-skills-manifest.json.lock`)
            // is created when the lock is taken, which is before anything is
            // written — counting it would report a tier that was never deployed
            // to as unattributable, and `tm doctor` would call it unverifiable.
            // Every deployed skill is a `<stem>/` directory, so dot-prefixed
            // entries are bookkeeping by construction.
            let populated = std::fs::read_dir(dest_dir)
                .map(|d| {
                    d.flatten()
                        .any(|e| !e.file_name().to_string_lossy().starts_with('.'))
                })
                .unwrap_or(false);
            if populated {
                ManifestState::AbsentButPopulated
            } else {
                ManifestState::AbsentAndEmpty
            }
        }
        Err(e) => ManifestState::Unreadable(format!(
            "{} could not be read: {e}",
            manifest_path.display()
        )),
    };

    let Some(manifest) = parsed.filter(|_| state == ManifestState::Present) else {
        return TierAudit {
            manifest: state,
            findings: Vec::new(),
        };
    };

    let mut findings: Vec<SkillDriftFinding> = manifest
        .managed
        .keys()
        .map(|key| SkillDriftFinding {
            stem: key.clone(),
            state: classify(reference, &manifest, dest_dir, key),
        })
        .collect();
    findings.sort_by(|a, b| a.stem.cmp(&b.stem));
    TierAudit {
        manifest: state,
        findings,
    }
}

/// Classify one manifest key. See [`audit_deployed_skills`] for the rules.
fn classify(
    reference: &SkillReference,
    manifest: &SkillManifest,
    dest_dir: &Path,
    key: &str,
) -> SkillDrift {
    let Some(expected) = reference.assets.get(key) else {
        return SkillDrift::Unverifiable(format!(
            "`{key}` is not among {} — this binary ships no copy to compare against",
            reference.origin
        ));
    };

    let path = deployed_path(dest_dir, key);
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
    if manifest.checksum_matches(key, &deployed) {
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
