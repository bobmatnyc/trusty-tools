//! Orphan agent files in the shared canonical agent deploy dir (#8992).
//!
//! Why: every bundled agent deploys into ONE directory every PM shares
//! ([`FrameworkPaths::agent_deploy_dir`], #4409), and the deploy path only
//! writes. An agent the bundle drops stays on disk and keeps reaching every
//! session's agent listing — `ops` has outlived its removal in #4765 that way.
//! ADR-0025 clause 9 / #391 left orphan retraction unshipped; agents had no
//! counterpart to `core::skill_retire` (#5224).
//!
//! What: [`scan_orphan_agents`] classifies the dir read-only, for the
//! `agent_orphans` doctor row; [`sweep_orphan_agents`] backs up and removes the
//! removable ones. The sweep runs ONLY from `tm doctor --fix-agents`, never at
//! launch or deploy (ruling D05): every PM reads this dir, so nothing is
//! deleted while another PM launches.
//!
//! WHAT LICENSES A DELETION — [`agent_removal_verdict`], the same rule
//! `tm catalog apply --prune` applies (its `prune_guard::agent_verdict` is now
//! a thin adapter onto it): a ledger entry, a framework-owned origin, and a
//! checksum the file still matches. The #4698 `provenance: framework-owned`
//! stamp is reported but never a licence — a copied or hand-edited file can
//! carry it, and only the ledger records the bytes tm wrote.
//!
//! FAIL CLOSED: an empty live set, a missing ledger, a corrupt ledger, or an
//! unlistable dir refuses the whole sweep. Every uncertainty keeps a file.
//!
//! Test: `agent_orphans_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::manifest::{
    AgentManifest, MANIFEST_FILE, ManifestLoad, with_agent_manifest_lock,
};
use trusty_agents_common::agents::metadata::agent_metadata_from_str;
use trusty_agents_common::agents::provenance::Provenance;

use crate::core::agent_manifest::ManifestError;
use crate::core::bundled_roster::bundled_roster_with;
use crate::core::content_source::{self, AgentRoster};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::paths::FrameworkPaths;
use crate::core::project_tier_agent_strays::back_up_and_remove;

/// The doctor check these steps answer.
pub const CHECK: &str = "agent_orphans";

/// The one-line summary every dir-wide refusal carries.
const SWEEP_WHAT: &str = "remove orphaned agent files from the shared agent dir";

/// Whether one ledger-tracked agent file may be deleted.
///
/// Why: "deleted" and "left in place because it is yours" must reach the
/// operator as separate facts; a silent skip hides the guard.
/// What: `Removable`, or `Kept` with an operator-facing reason.
/// Test: `an_edited_orphan_is_kept`, `a_pristine_orphan_is_removed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRemoval {
    /// tm wrote it and nobody changed it since.
    Removable,
    /// Left on disk; the string says why.
    Kept(String),
}

/// One agent file (or ledger entry) the current bundle no longer ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanAgent {
    /// The file name, which is also the ledger key.
    pub file_name: String,
    /// `<dir>/<file_name>`.
    pub path: PathBuf,
    /// Whether the sweep may delete it.
    pub verdict: AgentRemoval,
}

/// May this managed agent file be deleted?
///
/// Why (#391, shared by #8992): deleting a file whose bytes the user changed
/// destroys work nobody asked to back up, and a checksum gate alone never
/// protects a pristine file the user owns — the ledger's `origin` does. One
/// copy of this gate serves `tm catalog apply --prune` and the orphan sweep so
/// the two cannot disagree about what is safe.
/// What: `Removable` when the file is absent (nothing to lose) or when the
/// ledger records it framework-owned AND its content still matches the
/// recorded checksum. `Kept` when the entry is missing, user-owned
/// (`User`/`Registry`), edited, or unreadable.
/// Test: `apply_prune_skips_hand_edited_agent`,
/// `apply_prune_spares_a_user_origin_agent`, `an_edited_orphan_is_kept`,
/// `a_user_origin_orphan_is_kept`.
pub fn agent_removal_verdict(
    manifest: &AgentManifest,
    target: &Path,
    filename: &str,
) -> AgentRemoval {
    let path = target.join(filename);
    if !path.exists() {
        return AgentRemoval::Removable;
    }
    match manifest.managed.get(filename) {
        Some(entry) if entry.origin.is_framework_owned() => {}
        Some(entry) => {
            return AgentRemoval::Kept(format!(
                "the ledger records it as {:?}-owned, not framework-owned",
                entry.origin
            ));
        }
        None => {
            return AgentRemoval::Kept(format!(
                "{} has no ledger entry, so trusty-mpm does not own it",
                path.display()
            ));
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(content) if manifest.checksum_matches(filename, &content) => AgentRemoval::Removable,
        Ok(_) => AgentRemoval::Kept(format!(
            "{} was edited after it was deployed",
            path.display()
        )),
        Err(e) => AgentRemoval::Kept(format!(
            "{} could not be read, so it cannot be verified: {e}",
            path.display()
        )),
    }
}

/// The lower-cased stem a file name or agent name is matched on.
fn stem_key(name: &str) -> String {
    name.strip_suffix(".md").unwrap_or(name).to_lowercase()
}

/// Every agent name the current bundle ships, lower-cased, matched by stem.
///
/// Why: a file is an orphan only when NO source has it, so this set must err
/// large. Deployed file names follow the SOURCE file name (`BASE-AGENT.md`),
/// while [`bundled_roster_with`] keys by declared `name:` (`base-agent`); a set
/// built from either alone would call a live file an orphan.
/// What: [`bundled_roster_with`] (declared names from the on-disk source dir
/// and `roster`), plus every roster file stem and every `.md` stem in
/// [`FrameworkPaths::agent_source_dir`], all lower-cased.
/// Test: `live_names_cover_every_repo_roster_file`.
pub fn live_agent_names(paths: &FrameworkPaths, roster: &AgentRoster) -> BTreeSet<String> {
    let mut live: BTreeSet<String> = bundled_roster_with(paths, roster)
        .iter()
        .map(|name| stem_key(name))
        .collect();
    live.extend(roster.iter().map(|(file_name, _)| stem_key(file_name)));
    if let Ok(entries) = std::fs::read_dir(paths.agent_source_dir()) {
        live.extend(
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(stem_key)),
        );
    }
    live
}

/// Resolve this machine's live agent set, or say why it cannot be.
///
/// Why: a roster that cannot be resolved must refuse the sweep, never be read
/// as "nothing is bundled" — that would make every file an orphan.
/// What: [`content_source::agent_roster`] through [`live_agent_names`];
/// `Err` when the roster is unavailable or empty.
/// Test: `an_empty_live_set_refuses_the_sweep` (the empty arm the core holds).
pub fn resolve_live_agent_names(paths: &FrameworkPaths) -> Result<BTreeSet<String>, String> {
    let roster = content_source::agent_roster()
        .map_err(|e| format!("the bundled agent roster is unavailable ({e})"))?;
    if roster.is_empty() {
        return Err("the bundled agent roster is empty".to_string());
    }
    Ok(live_agent_names(paths, &roster))
}

/// Load the dir's ledger, failing closed.
///
/// Why: a missing or corrupt ledger answers "did tm write this?" with "no" for
/// every file — safe for deletion, but a report built on it would read as an
/// examined dir when nothing was established.
/// What: `Ok` with the parsed ledger; `Err` when the ledger is corrupt or
/// unreadable, or absent while the dir holds `.md` files.
/// Test: `a_missing_ledger_refuses_the_sweep`, `a_corrupt_ledger_refuses_the_sweep`.
fn load_ledger(dir: &Path) -> Result<AgentManifest, String> {
    match AgentManifest::load_checked(dir) {
        ManifestLoad::Corrupt(detail) => Err(format!(
            "the deploy ledger is unreadable ({detail}) — it is what proves which files tm \
             wrote, so nothing here can be removed until it is repaired"
        )),
        ManifestLoad::Ok(manifest) => {
            if !dir.join(MANIFEST_FILE).exists() && !md_files(dir)?.is_empty() {
                return Err(format!(
                    "{} holds agent files but no deploy ledger ({MANIFEST_FILE}) — nothing \
                     here can be proven tm-deployed",
                    dir.display()
                ));
            }
            Ok(manifest)
        }
    }
}

/// Every non-dot `.md` file name directly in `dir`, sorted.
fn md_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("{} could not be listed: {e}", dir.display()))?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| !n.starts_with('.') && n.ends_with(".md"))
        .collect();
    names.sort();
    Ok(names)
}

/// Why an untracked orphan is kept, by whether it carries the #4698 stamp.
fn untracked_reason(path: &Path) -> String {
    let stamped = std::fs::read_to_string(path).is_ok_and(|content| {
        agent_metadata_from_str(&content).provenance == Some(Provenance::FrameworkOwned)
    });
    if stamped {
        "stamped `provenance: framework-owned` but absent from the deploy ledger — with no \
         recorded checksum tm cannot prove it is unedited"
            .to_string()
    } else {
        "no tm provenance: absent from the deploy ledger and not stamped framework-owned"
            .to_string()
    }
}

/// Classify every orphan in `dir` against an already-loaded ledger.
///
/// What: ledger keys whose stem `live` lacks get [`agent_removal_verdict`]; a
/// non-live `.md` file the ledger lacks is always `Kept`. Sorted by file name.
fn classify(
    dir: &Path,
    manifest: &AgentManifest,
    live: &BTreeSet<String>,
) -> Result<Vec<OrphanAgent>, String> {
    let mut names: BTreeSet<String> = manifest.managed.keys().cloned().collect();
    names.extend(md_files(dir)?);
    Ok(names
        .into_iter()
        .filter(|name| !live.contains(&stem_key(name)))
        .map(|file_name| {
            let path = dir.join(&file_name);
            let verdict = if file_name.contains('/') {
                AgentRemoval::Kept("a ledger key that is not a top-level agent file".into())
            } else if manifest.managed.contains_key(&file_name) {
                agent_removal_verdict(manifest, dir, &file_name)
            } else {
                AgentRemoval::Kept(untracked_reason(&path))
            };
            OrphanAgent {
                file_name,
                path,
                verdict,
            }
        })
        .collect())
}

/// Read-only: the orphans in `dir`, or why they cannot be established.
///
/// Why: the doctor row and the sweep must agree file for file, so both run
/// [`classify`]; this half takes no lock and writes nothing.
/// What: `Ok(vec![])` when `dir` is absent; `Err` when `live` is empty or the
/// ledger is missing, corrupt, or the dir unlistable.
/// Test: `scan_reports_what_the_sweep_would_do`.
pub fn scan_orphan_agents(dir: &Path, live: &BTreeSet<String>) -> Result<Vec<OrphanAgent>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    if live.is_empty() {
        return Err("no bundled agent names are known — every file would read as an orphan".into());
    }
    let manifest = load_ledger(dir)?;
    classify(dir, &manifest, live)
}

/// Sweep this machine's canonical agent dir (`tm doctor --fix-agents`).
///
/// What: resolves the live set with [`resolve_live_agent_names`] and runs
/// [`sweep_orphan_agents_in`] on [`FrameworkPaths::agent_deploy_dir`]. An
/// unresolvable roster is one `Refused` step.
/// Test: `sweep_orphan_agents_in` carries the coverage.
pub fn sweep_orphan_agents(
    paths: &FrameworkPaths,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let dir = paths.agent_deploy_dir();
    match resolve_live_agent_names(paths) {
        Ok(live) => sweep_orphan_agents_in(&dir, &live, backup_root, mode),
        Err(why) => vec![refusal(dir, why)],
    }
}

/// Remove the removable orphans from `dir`, holding its ledger lock.
///
/// Why: the injectable core — taking `live` as a parameter is what lets the
/// tests drive it against a literal set instead of the real bundle.
/// What: one [`RepairStep`] per orphan: `Refused` for a kept file,
/// `Planned` in [`RepairMode::DryRun`], otherwise `Applied` after a backup
/// under `<backup_root>/canonical-agents/`, the removal, and dropping the
/// ledger entry. A ledger entry whose file is already gone is dropped with no
/// backup. A dir-wide failure (see [`scan_orphan_agents`]) is one `Refused` step.
/// Test: `a_pristine_orphan_is_removed`, `an_edited_orphan_is_kept`,
/// `an_untracked_file_is_kept`, `a_user_origin_orphan_is_kept`,
/// `a_current_bundled_agent_is_kept`, `a_missing_ledger_refuses_the_sweep`,
/// `a_corrupt_ledger_refuses_the_sweep`, `an_empty_live_set_refuses_the_sweep`,
/// `a_dry_run_writes_nothing`.
pub fn sweep_orphan_agents_in(
    dir: &Path,
    live: &BTreeSet<String>,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    if !dir.is_dir() {
        return Vec::new();
    }
    if live.is_empty() {
        return vec![refusal(
            dir.to_path_buf(),
            "no bundled agent names are known — every file would read as an orphan".into(),
        )];
    }
    let locked = with_agent_manifest_lock::<_, ManifestError, _>(dir, || {
        Ok(sweep_locked(dir, live, backup_root, mode))
    });
    locked.unwrap_or_else(|e| {
        vec![RepairStep {
            check: CHECK,
            path: dir.to_path_buf(),
            what: SWEEP_WHAT.to_string(),
            status: StepStatus::Failed(format!(
                "could not lock the deploy ledger: {e} — refusing to sweep unserialised"
            )),
        }]
    })
}

/// The body of [`sweep_orphan_agents_in`], run holding the ledger lock.
fn sweep_locked(
    dir: &Path,
    live: &BTreeSet<String>,
    backup_root: &Path,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let mut manifest = match load_ledger(dir) {
        Ok(m) => m,
        Err(why) => return vec![refusal(dir.to_path_buf(), why)],
    };
    let orphans = match classify(dir, &manifest, live) {
        Ok(orphans) => orphans,
        Err(why) => return vec![refusal(dir.to_path_buf(), why)],
    };

    let mut dirty = false;
    let mut steps = Vec::with_capacity(orphans.len());
    for orphan in orphans {
        let stem = orphan.file_name.trim_end_matches(".md");
        let what = format!("remove `{stem}`, an agent the current bundle no longer ships");
        let status = match orphan.verdict {
            AgentRemoval::Kept(why) => StepStatus::Refused(why),
            AgentRemoval::Removable if mode == RepairMode::DryRun => StepStatus::Planned,
            AgentRemoval::Removable => remove_one(&orphan.path, &orphan.file_name, backup_root),
        };
        if matches!(status, StepStatus::Applied { .. }) {
            manifest.managed.remove(&orphan.file_name);
            dirty = true;
        }
        steps.push(RepairStep {
            check: CHECK,
            path: orphan.path,
            what,
            status,
        });
    }

    if dirty && let Err(e) = manifest.save(dir) {
        steps.push(RepairStep {
            check: CHECK,
            path: dir.join(MANIFEST_FILE),
            what: "drop the removed orphans from the deploy ledger".to_string(),
            status: StepStatus::Failed(format!("could not save the deploy ledger: {e}")),
        });
    }
    steps
}

/// Back up and remove one removable orphan; an already-gone file needs neither.
fn remove_one(path: &Path, file_name: &str, backup_root: &Path) -> StepStatus {
    if !path.exists() {
        return StepStatus::Applied { backup: None };
    }
    match back_up_and_remove(path, file_name, backup_root, "canonical-agents") {
        Ok(backup) => StepStatus::Applied {
            backup: Some(backup),
        },
        Err(why) => StepStatus::Failed(why),
    }
}

/// One dir-wide [`StepStatus::Refused`] step.
fn refusal(dir: PathBuf, why: String) -> RepairStep {
    RepairStep {
        check: CHECK,
        path: dir,
        what: SWEEP_WHAT.to_string(),
        status: StepStatus::Refused(why),
    }
}

#[cfg(test)]
#[path = "agent_orphans_tests.rs"]
mod tests;
