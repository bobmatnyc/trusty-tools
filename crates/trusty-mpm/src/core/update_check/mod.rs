//! Catalog update-check: detect when deployed content is stale (HR-3 / DOC-17).
//!
//! Why: the claude-mpm catalog evolves; a session deployed last week may run
//! agents/skills that no longer match the upstream catalog. DOC-17 §HR-3 requires
//! the runner to DETECT that drift autonomously and OFFER a rebuild — never to
//! silently force one. This module owns the pure detection: a SHA compare of the
//! catalog's current content against the deployed checksum manifests
//! (`agent_deployer`/`skill_deployer` write them, §2.2). It is intentionally
//! cheap and offline: it hashes an already-synced catalog checkout and the
//! already-deployed files; it NEVER pulls from the network (a network sync is the
//! TTL-gated [`crate::content::CatalogSync::sync`], a separate concern).
//! What: [`detect_staleness`] compares a catalog agent/skill source tree against
//! the deployed [`AgentManifest`]/[`SkillManifest`] under a selection predicate
//! and returns a [`StalenessReport`] — `stale`/`unknown` flags plus a small list
//! of WHAT changed. [`StalenessReport::summary_lines`] renders the change list.
//! Test: this module's `tests` cover stale-on-change, not-stale-on-identical,
//! unknown-when-never-synced, and selection filtering; the `/health` and `apply`
//! wiring is covered in the daemon and CLI suites.

use std::collections::HashMap;
use std::path::Path;

use crate::core::agent_builder::compose_agent;
use crate::core::agent_manifest::{AgentManifest, checksum};
use crate::core::skill_manifest::SkillManifest;

mod apply;
pub use apply::{ApplyError, ApplyReport, apply_catalog};

/// The classification of one catalog artifact relative to what was deployed.
///
/// Why: the rebuild offer needs to tell the operator WHAT changed, not just that
/// *something* did; a per-artifact verb makes the `catalog_changes` summary
/// actionable ("agent rust-engineer: changed", "skill foo: new").
/// What: a closed enum naming the three drift outcomes the SHA compare yields.
/// Test: asserted indirectly via [`StalenessReport::summary_lines`] in `tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Present in the catalog but never deployed (absent from the manifest).
    New,
    /// Deployed, but the catalog's current content hashes differently.
    Changed,
}

impl ChangeKind {
    /// The lowercase verb shown in a change summary line.
    ///
    /// Why: keeps the rendered wording single-sourced between the summary builder
    /// and any test asserting on it.
    /// What: `"new"` or `"changed"`.
    /// Test: `summary_lines_render_kind_and_name`.
    fn label(self) -> &'static str {
        match self {
            ChangeKind::New => "new",
            ChangeKind::Changed => "changed",
        }
    }
}

/// One catalog artifact that differs from the deployed copy.
///
/// Why: the report carries a bounded list of these so the operator (and the TUI)
/// can see the first handful of changes without the daemon doing string work on
/// the hot health path.
/// What: the artifact kind (agent/skill), its name, and how it drifted.
/// Test: `detect_flags_changed_agent`, `detect_flags_new_skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogChange {
    /// `"agent"` or `"skill"` — the artifact class.
    pub artifact: &'static str,
    /// The artifact's stem name (e.g. `rust-engineer`, `tm-doctor`).
    pub name: String,
    /// How the catalog copy drifted from the deployed copy.
    pub kind: ChangeKind,
}

/// The outcome of a catalog staleness check.
///
/// Why: `/health` surfaces `catalog_stale` and a small change summary; the TUI
/// reads the flag for its indicator; the `apply` command reports what it will
/// refresh. One struct serves all three so the detection logic stays in one
/// place.
/// What: `stale` is true when at least one selected catalog artifact is new or
/// changed; `unknown` is true when the catalog has never been synced (no source
/// tree to compare against) — distinct from "fresh" so the surface can say
/// "run `tm catalog sync` first" rather than implying currency. `changes` is the
/// bounded per-artifact drift list.
/// Test: `detect_*` tests assert each flag and the change list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StalenessReport {
    /// True when at least one selected catalog artifact differs from deployed.
    pub stale: bool,
    /// True when the catalog has never been synced (nothing to compare).
    pub unknown: bool,
    /// The drift list (bounded by [`MAX_CHANGES`]); empty when not stale.
    pub changes: Vec<CatalogChange>,
}

/// Cap on the number of per-artifact changes retained in a report.
///
/// Why: a first-ever sync against an empty deployment would flag ~40 agents +
/// ~25 skills as `new`; carrying every one onto the health response is needless.
/// A small cap keeps the payload (and the TUI hint) tidy while still proving
/// staleness. The `stale` flag itself is never capped.
/// What: the maximum length of [`StalenessReport::changes`].
/// Test: `detect_caps_change_list`.
pub const MAX_CHANGES: usize = 12;

impl StalenessReport {
    /// Render the change list as short human lines (e.g. `agent foo: changed`).
    ///
    /// Why: both the CLI `apply` output and the daemon's `catalog_changes` field
    /// want the same compact wording; centralizing it keeps them identical.
    /// What: one `"<artifact> <name>: <kind>"` string per change, in list order.
    /// Test: `summary_lines_render_kind_and_name`.
    pub fn summary_lines(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|c| format!("{} {}: {}", c.artifact, c.name, c.kind.label()))
            .collect()
    }
}

/// List the `.md` stems directly under `dir`, sorted; empty when `dir` is absent.
///
/// Why: both the agent and skill catalog comparisons iterate the source `.md`
/// files deterministically; a shared helper keeps ordering stable so the change
/// list (and its cap) are reproducible.
/// What: reads `dir`, keeps regular `*.md` files, returns their `.md`-stripped
/// stems sorted. A missing directory yields an empty vec (the caller treats that
/// as "never synced" for the whole tree).
/// Test: exercised via `detect_*` which seed catalog dirs.
fn md_stems(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            name.strip_suffix(".md").map(str::to_owned)
        })
        .collect();
    stems.sort_unstable();
    stems
}

/// Classify one artifact's drift against what the next `apply` would actually do.
///
/// Why (#1940): the old rule keyed drift on the checksum MANIFEST alone — an
/// artifact absent from the manifest was reported `new`, even when the file was
/// already deployed on disk. Real deployments landed files via paths that never
/// recorded a manifest entry (or another tool wrote an identical file), so those
/// on-disk-but-unmanifested files surfaced as permanent false "new" drift. This
/// classifier reconciles the manifest against the file ACTUALLY on disk, so the
/// report matches what `apply` would change rather than what the manifest happens
/// to record. `apply` only writes a selected artifact when it is absent or is a
/// managed, unmodified refresh; it skips user-owned and user-modified files.
/// What: given the catalog content hash, the manifest's recorded checksum (if
/// any), and the on-disk deployed content (if any), returns:
/// - `None` when the artifact is already deployed & current (manifest OR on-disk
///   content matches the catalog hash) — including the reconcile case where the
///   file is present and correct but was never manifested;
/// - `None` when the file exists but is user-owned (not managed) or user-modified
///   (managed but on-disk differs from the recorded checksum) — `apply` skips it,
///   so it is not actionable drift;
/// - `Some(Changed)` when the artifact is managed, its catalog content changed,
///   and the on-disk copy is still what tm wrote (or cannot be read to prove
///   otherwise) — `apply` would refresh it;
/// - `Some(New)` when nothing is deployed on disk and the manifest has no entry —
///   `apply` would deploy it.
///
/// Test: `classify_drift_reconciles_on_disk`, `classify_drift_new_when_absent`,
/// and `detect_reconciles_deployed_but_unmanifested`.
fn classify_drift(
    catalog_hash: &str,
    manifest_checksum: Option<&str>,
    on_disk: Option<&str>,
) -> Option<ChangeKind> {
    let on_disk_hash = on_disk.map(checksum);
    // Deployed & current — the manifest records this exact content, or the file
    // already on disk holds it (reconcile: deployed-but-unmanifested). Not drift.
    if manifest_checksum == Some(catalog_hash) || on_disk_hash.as_deref() == Some(catalog_hash) {
        return None;
    }
    match (manifest_checksum, on_disk_hash.as_deref()) {
        // Managed, catalog changed since deploy. `apply` refreshes it only if the
        // on-disk copy is still what tm wrote (unmodified); a user-edited copy is
        // skipped, so it is not actionable drift.
        (Some(recorded), Some(disk_hash)) => (disk_hash == recorded).then_some(ChangeKind::Changed),
        // Managed, catalog changed, on-disk unreadable/absent: cannot prove the
        // user edited it, so report Changed (apply would attempt a refresh).
        (Some(_), None) => Some(ChangeKind::Changed),
        // Not managed and the on-disk content differs from the catalog: a
        // user-owned file `apply` never touches → not actionable drift.
        (None, Some(_)) => None,
        // Not managed and nothing on disk → genuinely new; `apply` deploys it.
        (None, None) => Some(ChangeKind::New),
    }
}

/// Compose+hash every deployable agent under `catalog_agents`, keyed by stem.
///
/// Why (issue #2444 review, MEDIUM finding): [`agent_changes`] used to
/// recompose EVERY catalog agent (walking its `extends:` chain) on every
/// single call — cheap for the ORIGINAL one-shot `/health`/`apply` call
/// sites, but `tm sessions ls`'s per-session staleness marker calls the
/// staleness comparison once per managed session, and every session's
/// default plan resolves to the SAME catalog agent source. Recomposing
/// ~40+ agents once per session (rather than once per `ls` request) was the
/// dominant cost. Splitting the compose step out into this standalone,
/// reusable hash map lets a caller comparing MANY targets against the SAME
/// catalog source (see [`CatalogHashes`]) compute it exactly once and share
/// it, while [`detect_staleness`] (the original single-target entry point)
/// still calls it fresh each time — identical behavior, no regression.
/// What: composes each `.md` stem in `catalog_agents` and records
/// `checksum(composed)`. A stem whose compose fails is simply absent from the
/// map (mirrors [`agent_changes`]'s pre-existing skip-on-compose-failure —
/// an agent that cannot be composed cannot be deployed either, so it was
/// never actionable drift).
/// Test: `compute_agent_catalog_hashes_skips_compose_failures`,
/// `detect_flags_changed_agent` (exercises this transitively via
/// [`detect_staleness`]).
pub fn compute_agent_catalog_hashes(catalog_agents: &Path) -> HashMap<String, String> {
    md_stems(catalog_agents)
        .into_iter()
        .filter_map(|stem| {
            let composed = compose_agent(&stem, catalog_agents).ok()?;
            let hash = checksum(&composed);
            Some((stem, hash))
        })
        .collect()
}

/// Hash every deployable skill body under `catalog_skills`, keyed by stem —
/// the skill-side sibling of [`compute_agent_catalog_hashes`].
///
/// Why: skills carry no `extends:` chain, so hashing them is already cheap
/// (one file read each) — but sharing the map across many targets still
/// saves the redundant reads for the common `tm sessions ls` case, and keeps
/// the agent/skill halves of [`CatalogHashes`] symmetric.
/// What: reads each `.md` stem's body under `catalog_skills` and records
/// `checksum(body)`. An unreadable file is simply absent from the map
/// (mirrors [`skill_changes`]'s pre-existing skip-on-read-failure).
/// Test: `compute_skill_catalog_hashes_reads_bodies`.
pub fn compute_skill_catalog_hashes(catalog_skills: &Path) -> HashMap<String, String> {
    md_stems(catalog_skills)
        .into_iter()
        .filter_map(|stem| {
            let filename = format!("{stem}.md");
            let body = std::fs::read_to_string(catalog_skills.join(&filename)).ok()?;
            let hash = checksum(&body);
            Some((stem, hash))
        })
        .collect()
}

/// Compare precomputed catalog agent hashes against the deployed agents
/// (manifest + on-disk files), reconciling the two so unmanifested-but-
/// deployed files are not false "new" drift.
///
/// Why: the agent half of staleness — for each selected catalog agent, the
/// PRECOMPUTED catalog hash (see [`compute_agent_catalog_hashes`]) is
/// classified via [`classify_drift`] against BOTH the checksum manifest and
/// the file actually on disk under `deployed_dir`. Keying only on the
/// manifest caused #1940's false positives; reconciling against the on-disk
/// file fixes them.
/// What: pushes a [`CatalogChange`] only when [`classify_drift`] reports the
/// agent as `new`/`changed`. Agents the predicate rejects are skipped (not
/// part of this harness's set).
/// Test: `detect_flags_changed_agent`, `detect_not_stale_when_identical`,
/// `detect_respects_selection`, `detect_reconciles_deployed_but_unmanifested`,
/// `detect_respects_ignore_staleness`.
fn agent_changes(
    catalog_hashes: &HashMap<String, String>,
    deployed: &AgentManifest,
    deployed_dir: &Path,
    select: &impl Fn(&str) -> bool,
    ignore_staleness: &impl Fn(&str) -> bool,
    out: &mut Vec<CatalogChange>,
) {
    for (stem, catalog_hash) in catalog_hashes {
        if !select(stem) || ignore_staleness(stem) {
            continue;
        }
        let filename = format!("{stem}.md");
        let on_disk = std::fs::read_to_string(deployed_dir.join(&filename)).ok();
        if let Some(kind) = classify_drift(
            catalog_hash,
            deployed.managed.get(&filename).map(|e| e.checksum.as_str()),
            on_disk.as_deref(),
        ) {
            out.push(CatalogChange {
                artifact: "agent",
                name: stem.clone(),
                kind,
            });
        }
    }
}

/// Compare precomputed catalog skill hashes against the deployed skill
/// manifest.
///
/// Why: the skill half of staleness — the PRECOMPUTED catalog hash (see
/// [`compute_skill_catalog_hashes`]) is compared against the deployed skill
/// manifest's checksum.
/// What: pushes a [`CatalogChange`] only when [`classify_drift`] reports the
/// skill as `new`/`changed`, reconciling the skill manifest against the file
/// actually on disk (deployed skills live at `<deployed_dir>/<stem>/SKILL.md`).
/// The [`SkillManifest`] is keyed by the bare STEM (no `.md`) — so the lookup
/// uses `stem`, matching the deployer.
/// Test: `detect_flags_new_skill`, `detect_not_stale_when_identical`,
/// `detect_reconciles_deployed_but_unmanifested`.
fn skill_changes(
    catalog_hashes: &HashMap<String, String>,
    deployed: &SkillManifest,
    deployed_dir: &Path,
    select: &impl Fn(&str) -> bool,
    out: &mut Vec<CatalogChange>,
) {
    for (stem, catalog_hash) in catalog_hashes {
        if !select(stem) {
            continue;
        }
        // Deployed skills land as `<deployed_dir>/<stem>/SKILL.md` (per the
        // skill deployer); the SkillManifest is keyed by stem (no `.md`).
        let on_disk = std::fs::read_to_string(deployed_dir.join(stem).join("SKILL.md")).ok();
        if let Some(kind) = classify_drift(
            catalog_hash,
            deployed.managed.get(stem).map(|e| e.checksum.as_str()),
            on_disk.as_deref(),
        ) {
            out.push(CatalogChange {
                artifact: "skill",
                name: stem.clone(),
                kind,
            });
        }
    }
}

/// Precomputed, reusable catalog-side hashes for ONE `(agent_source,
/// skill_source)` pair — the expensive half of a staleness comparison,
/// shareable across every DEPLOYED-side comparison against that same catalog
/// within one caller's batch (issue #2444 review MEDIUM finding).
///
/// Why: a staleness comparison has two independent halves — hashing the
/// CATALOG side (composing agents, reading skill bodies: expensive, and
/// IDENTICAL for every target sharing the same source paths) and comparing
/// against the DEPLOYED side (reading one target's manifest + on-disk files:
/// cheap, and unique per target). `detect_staleness` previously redid the
/// catalog-side work on every call; `tm sessions ls` calling it once per
/// managed session (issue #2444) turned that redundant recompose into the
/// dominant per-request cost. Splitting the two halves lets a batch caller
/// (`daemon::managed_routes::summary::checked_summaries`) call
/// [`CatalogHashes::compute`] ONCE per distinct `(agent_source, skill_source)`
/// pair it encounters — which collapses to a SINGLE compute for the common
/// case where every session resolves the same default bundled/catalog
/// source — then call [`CatalogHashes::detect`] per target using the shared
/// result. Selection predicates are NOT part of the cache key: they are cheap
/// glob/string matching applied only in `detect`, so two sessions sharing a
/// catalog source but selecting different subsets still share this cache
/// correctly.
/// What: `unknown` mirrors [`StalenessReport::unknown`] (neither source tree
/// exists); the two hash maps are the [`compute_agent_catalog_hashes`] /
/// [`compute_skill_catalog_hashes`] outputs.
/// Test: `catalog_hashes_compute_is_unknown_without_either_source`,
/// `catalog_hashes_shared_across_two_deployed_targets`.
#[derive(Debug, Default)]
pub struct CatalogHashes {
    unknown: bool,
    agent_hashes: HashMap<String, String>,
    skill_hashes: HashMap<String, String>,
}

impl CatalogHashes {
    /// Compute the catalog-side hashes for one `(catalog_agents,
    /// catalog_skills)` pair.
    ///
    /// Why: the one-time expensive step ([`compute_agent_catalog_hashes`]/
    /// [`compute_skill_catalog_hashes`]) a batch caller runs ONCE per distinct
    /// source pair and reuses via [`Self::detect`].
    /// What: `unknown: true` (empty maps) when NEITHER source tree exists —
    /// identical short-circuit to [`detect_staleness`]'s original
    /// never-synced check; otherwise computes both maps.
    /// Test: `catalog_hashes_compute_is_unknown_without_either_source`.
    pub fn compute(catalog_agents: &Path, catalog_skills: &Path) -> Self {
        if !catalog_agents.is_dir() && !catalog_skills.is_dir() {
            return Self {
                unknown: true,
                ..Default::default()
            };
        }
        Self {
            unknown: false,
            agent_hashes: compute_agent_catalog_hashes(catalog_agents),
            skill_hashes: compute_skill_catalog_hashes(catalog_skills),
        }
    }

    /// Compare ONE deployed target against these precomputed catalog hashes.
    ///
    /// Why: the cheap, per-target half of the comparison — reads only this
    /// target's manifest + on-disk files; does zero catalog I/O (that already
    /// happened in [`Self::compute`]).
    /// What: identical semantics to [`detect_staleness`] for a non-`unknown`
    /// cache: compares the selected catalog agents/skills against the
    /// deployed manifests + on-disk files via [`classify_drift`], caps the
    /// change list at [`MAX_CHANGES`]. When `self.unknown`, returns the same
    /// `unknown` report [`detect_staleness`] would.
    /// Test: `catalog_hashes_shared_across_two_deployed_targets`.
    #[allow(clippy::too_many_arguments)]
    pub fn detect(
        &self,
        deployed_agents: &AgentManifest,
        deployed_skills: &SkillManifest,
        deployed_agents_dir: &Path,
        deployed_skills_dir: &Path,
        agent_select: impl Fn(&str) -> bool,
        skill_select: impl Fn(&str) -> bool,
        agent_ignore_staleness: impl Fn(&str) -> bool,
    ) -> StalenessReport {
        if self.unknown {
            return StalenessReport {
                stale: false,
                unknown: true,
                changes: Vec::new(),
            };
        }

        let mut changes = Vec::new();
        agent_changes(
            &self.agent_hashes,
            deployed_agents,
            deployed_agents_dir,
            &agent_select,
            &agent_ignore_staleness,
            &mut changes,
        );
        skill_changes(
            &self.skill_hashes,
            deployed_skills,
            deployed_skills_dir,
            &skill_select,
            &mut changes,
        );

        let stale = !changes.is_empty();
        changes.truncate(MAX_CHANGES);
        StalenessReport {
            stale,
            unknown: false,
            changes,
        }
    }
}

/// Detect whether the deployed harness content is stale vs the synced catalog.
///
/// Why: this is the single autonomous check DOC-17 §HR-3 mandates — surfaced on
/// `/health`, shown in the TUI, and the precondition for the rebuild offer. It is
/// deliberately a pure, offline SHA compare so it can run on the health hot path
/// and in deterministic tests without a network or a live `claude`. This is a
/// thin wrapper over [`CatalogHashes::compute`] + [`CatalogHashes::detect`] for
/// the common single-target call site; a caller comparing MANY targets against
/// the SAME catalog source (issue #2444's `tm sessions ls`) should call
/// [`CatalogHashes`] directly and share the `compute` result instead of calling
/// this once per target.
/// What: when neither catalog source tree exists (the catalog was never synced)
/// returns `unknown` (and NOT stale) so the surface can prompt a sync rather than
/// imply currency — DOC-17's "catalog unreachable → treat as not-stale, never
/// block". Otherwise it compares the selected catalog agents (composed-hash) and
/// skills (raw-hash) against BOTH the deployed checksum manifests
/// ([`AgentManifest`]/[`SkillManifest`]) AND the files actually on disk under
/// `deployed_agents_dir`/`deployed_skills_dir` (via [`classify_drift`]), so a
/// deployed-but-unmanifested file is reconciled rather than reported as a false
/// `new` (#1940). `stale` is true iff any selected artifact would be
/// deployed/refreshed by `apply`. The change list is capped at [`MAX_CHANGES`].
/// `agent_ignore_staleness` (issue #2462) additionally exempts a selected agent
/// from drift reporting WITHOUT removing it from `agent_select`'s deploy roster
/// — see [`crate::core::manifest::AgentSet::ignore_staleness`] for why this is a
/// separate predicate from selection.
/// Test: `detect_unknown_when_never_synced`, `detect_flags_changed_agent`,
/// `detect_flags_new_skill`, `detect_not_stale_when_identical`,
/// `detect_respects_selection`, `detect_caps_change_list`,
/// `detect_reconciles_deployed_but_unmanifested`, `detect_respects_ignore_staleness`.
#[allow(clippy::too_many_arguments)]
pub fn detect_staleness(
    catalog_agents: &Path,
    catalog_skills: &Path,
    deployed_agents: &AgentManifest,
    deployed_skills: &SkillManifest,
    deployed_agents_dir: &Path,
    deployed_skills_dir: &Path,
    agent_select: impl Fn(&str) -> bool,
    skill_select: impl Fn(&str) -> bool,
    agent_ignore_staleness: impl Fn(&str) -> bool,
) -> StalenessReport {
    CatalogHashes::compute(catalog_agents, catalog_skills).detect(
        deployed_agents,
        deployed_skills,
        deployed_agents_dir,
        deployed_skills_dir,
        agent_select,
        skill_select,
        agent_ignore_staleness,
    )
}

/// Detect staleness for a framework root, resolving the harness manifest itself.
///
/// Why: both `GET /health` and `tm catalog apply` need staleness against the SAME
/// inputs the launcher uses — the catalog checkout `CatalogSync` populates and the
/// manifest-selected agent/skill set. Centralizing the resolve→plan→compare here
/// means the daemon and the CLI cannot drift on WHAT counts as the harness set.
/// It performs NO network sync: it compares the already-synced catalog checkout
/// (gated upstream by the TTL) against the already-deployed manifests, so it is
/// cheap enough for the health hot path.
/// What: resolves the effective [`HarnessManifest`] for `project_dir` (the
/// project override layer; pass the framework root for the daemon-wide baseline),
/// materializes the [`HarnessPlan`] to learn the catalog source dirs + selection
/// predicates, loads the deployed [`AgentManifest`]/[`SkillManifest`] from
/// `~/.claude/`, and runs [`detect_staleness`]. When the plan's content sources
/// are *bundled* (the default manifest) the catalog dirs do not exist, so the
/// result is `unknown` — staleness is only meaningful once a manifest opts an
/// artifact class onto the catalog source.
/// Test: `detect_for_framework_unknown_without_catalog` (bundled default →
/// unknown); the catalog-source path is covered by the daemon/apply integration
/// tests which seed a catalog checkout.
pub fn detect_for_framework(
    fw: &crate::core::paths::FrameworkPaths,
    project_dir: &Path,
) -> StalenessReport {
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    let agents_dir = fw.claude_agents_dir();
    let skills_dir = fw.claude_skills_dir();
    let deployed_agents = AgentManifest::load(&agents_dir);
    let deployed_skills = SkillManifest::load(&skills_dir);

    detect_staleness(
        &plan.agent_source,
        &plan.skill_source,
        &deployed_agents,
        &deployed_skills,
        &agents_dir,
        &skills_dir,
        |name| plan.agent_selected(name),
        |name| plan.skill_selected(name),
        |name| plan.agent_staleness_ignored(name),
    )
}

#[cfg(test)]
mod tests;
