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

/// Test-only counter of DEPLOYED-side file reads (issue #4322).
///
/// Why: cold `tm ls` latency is governed by how many files the probe opens,
/// not by CPU — the issue measured a constant 0.128 s of daemon CPU against a
/// 5–9 s wall time, and proved causality by warming exactly the probe's file
/// set from an unrelated process. That makes the read COUNT the honest,
/// machine-independent metric for this optimization: it does not move with
/// machine load, page-cache state, or how busy the laptop was when the
/// benchmark ran. It is also the enforcement handle for the sharing invariant
/// — a fleet of N sessions must read the ONE machine-global deployed-agent
/// directory once, not N times.
/// What: a LOG of the exact path of every deployed-file read attempt (hit or
/// miss), appended by [`read_deployed`]. A log, not a bare counter, for the
/// same reason [`COMPUTE_CALL_LOG`] is one (#4326 review): the counter is
/// process-global and `cargo test` runs many unrelated tests concurrently in
/// the same binary — NONE of the 21 tests in this module's `tests.rs` are
/// `#[serial]`, and every one of them reaches `read_deployed`. A global count
/// therefore attributes their reads to whichever test happens to be asserting
/// at that moment: a filtered `cargo test -- detect` run reproducibly observed
/// 25 where the fleet pin expected 5. Recording paths lets each test count
/// ONLY the reads made under its own `fake_home()`-scoped temp directory, so
/// concurrent unrelated reads through unrelated paths cannot pollute it.
/// [`reset_deployed_read_log`] clears it; [`deployed_reads_under`] counts one
/// prefix.
/// Test: `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet`
/// in `daemon::managed_routes::tests`; reported by
/// `daemon::managed_routes::staleness_bench_tests::bench_stale_assets_for_many`.
#[cfg(test)]
pub(crate) static DEPLOYED_READ_LOG: std::sync::LazyLock<
    std::sync::Mutex<Vec<std::path::PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Clear [`DEPLOYED_READ_LOG`] before the code under measurement runs.
///
/// Note this does NOT provide isolation on its own — concurrent unrelated
/// tests keep appending. Isolation comes from counting with
/// [`deployed_reads_under`], which filters to the caller's own path prefix.
#[cfg(test)]
pub(crate) fn reset_deployed_read_log() {
    DEPLOYED_READ_LOG.lock().unwrap().clear();
}

/// Count logged deployed-file reads whose path lies under `prefix`.
///
/// Why: the isolation primitive. A test passes a prefix unique to its own
/// fixture (its `fake_home()` temp dir, or a specific deploy directory under
/// it), so reads performed concurrently by unrelated tests — which live under
/// their own distinct temp dirs — are excluded by construction rather than by
/// hoping no other test runs at the same time.
#[cfg(test)]
pub(crate) fn deployed_reads_under(prefix: &Path) -> usize {
    DEPLOYED_READ_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(prefix))
        .count()
}

/// Read one deployed artifact file, logging the attempt under `cfg(test)`.
///
/// Why: routing every deployed-side read through one helper is what makes the
/// read log above meaningful — a read added later that bypassed it would be
/// invisible to both the benchmark and the sharing invariant test.
/// What: `std::fs::read_to_string(path).ok()`, plus a log append in test
/// builds. Identical behavior to the direct call it replaces (a missing or
/// unreadable file yields `None`).
/// Test: exercised by every `detect_*` test; counted by
/// `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet`.
fn read_deployed(path: &Path) -> Option<String> {
    #[cfg(test)]
    DEPLOYED_READ_LOG.lock().unwrap().push(path.to_path_buf());
    std::fs::read_to_string(path).ok()
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
/// Takes the on-disk CHECKSUM rather than the on-disk CONTENT (issue #4322):
/// the deployed AGENT tree is one machine-global directory shared by every
/// managed session, so a fleet-wide caller hashes it once and reuses the
/// digests across sessions ([`DeployedAgentHashes`]) instead of re-reading and
/// re-hashing the same bytes per session. Hashing at the call site is exactly
/// what this function used to do internally, so the classification is
/// unchanged.
///
/// Test: `classify_drift_reconciles_on_disk`, `classify_drift_new_when_absent`,
/// and `detect_reconciles_deployed_but_unmanifested`.
fn classify_drift(
    catalog_hash: &str,
    manifest_checksum: Option<&str>,
    on_disk_hash: Option<&str>,
) -> Option<ChangeKind> {
    // Deployed & current — the manifest records this exact content, or the file
    // already on disk holds it (reconcile: deployed-but-unmanifested). Not drift.
    if manifest_checksum == Some(catalog_hash) || on_disk_hash == Some(catalog_hash) {
        return None;
    }
    match (manifest_checksum, on_disk_hash) {
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
/// Issue #4322: the deployed side is supplied PRE-READ as a
/// [`DeployedAgentHashes`] rather than as a directory this function walks
/// itself. The selection and ignore predicates are applied here, AFTER the
/// read — they are pure string matching over the stem, so they cannot affect
/// which bytes the disk yields, which is exactly what makes the pre-read
/// snapshot shareable across callers with different predicates.
///
/// Test: `detect_flags_changed_agent`, `detect_not_stale_when_identical`,
/// `detect_respects_selection`, `detect_reconciles_deployed_but_unmanifested`,
/// `detect_respects_ignore_staleness`.
fn agent_changes(
    catalog_hashes: &HashMap<String, String>,
    deployed: &DeployedAgentHashes,
    select: &impl Fn(&str) -> bool,
    ignore_staleness: &impl Fn(&str) -> bool,
    out: &mut Vec<CatalogChange>,
) {
    for (stem, catalog_hash) in catalog_hashes {
        if !select(stem) || ignore_staleness(stem) {
            continue;
        }
        let filename = format!("{stem}.md");
        let on_disk_hash = deployed.on_disk_hash(stem);
        if let Some(kind) = classify_drift(
            catalog_hash,
            deployed
                .manifest
                .managed
                .get(&filename)
                .map(|e| e.checksum.as_str()),
            on_disk_hash.as_deref(),
        ) {
            out.push(CatalogChange {
                artifact: "agent",
                name: stem.clone(),
                kind,
            });
        }
    }
}

/// The deployed AGENT tree, read and hashed ONCE for one directory + catalog
/// pair (issue #4322).
///
/// Why: bundled agents deploy into a single machine-global directory —
/// `FrameworkPaths::agent_deploy_dir()`, which #4409 deliberately exempted
/// from `for_managed_project`'s per-workspace rewrite — so EVERY managed
/// session's staleness probe reads the very same files. `tm ls` was opening
/// and hashing that one directory once per session: on the reported 32-session
/// fleet, 42 agent files re-read 32 times, 30.2 MiB of the 41.9 MiB the listing
/// moved, for 32 byte-identical answers. Reading it once per LISTING instead
/// of once per SESSION removes that fan-out without touching what is actually
/// per-session (skills, which still deploy per workspace).
///
/// This is deduplication WITHIN one request, NOT a cache across requests:
/// nothing is retained after the listing returns, so the next `tm ls` re-reads
/// the tree from disk and observes any change immediately. There is no
/// invalidation window to get wrong.
/// What: the deployed [`AgentManifest`] plus, in the EAGER form, `stem ->
/// checksum(on-disk body)` for every stem in the catalog. The eager map is
/// deliberately keyed on the CATALOG's stems (not on a selected subset) so the
/// snapshot is predicate-independent and one snapshot serves sessions selecting
/// different agent sets. A stem with no readable file is simply absent, which
/// [`classify_drift`] reads as `None` — exactly what the per-call
/// `read_to_string(...).ok()` produced before.
///
/// Two forms, because eagerness is only a win when the result is SHARED
/// (#4619 review, MEDIUM):
/// - [`Self::read`] — EAGER. Hashes every catalog stem up front. Correct for
///   the fleet path, where one snapshot serves N sessions.
/// - [`Self::lazy`] — LAZY. Hashes a stem only when [`agent_changes`] actually
///   asks for it, i.e. only for stems the caller's predicates select. Correct
///   for every SINGLE-target caller (`/health`, `tm catalog apply`, the
///   single-session `GET …/managed/{id}`), which has nothing to share the work
///   with. Using the eager form there would read every catalog stem where the
///   old code read only the selected ones — a real read INCREASE for a
///   manifest-filtered harness. The lazy form makes single-target read
///   behavior byte-for-byte identical to before this change.
///
/// Test: `session_asset_staleness_with_shared_matches_unshared`,
/// `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet`,
/// `single_target_detect_reads_only_selected_agents`.
#[derive(Debug, Clone, Default)]
pub struct DeployedAgentHashes {
    manifest: AgentManifest,
    /// Directory the deployed agent files live in — needed by the lazy form,
    /// and retained by the eager form so both share one lookup path.
    dir: std::path::PathBuf,
    /// `Some` = eager, pre-hashed snapshot. `None` = hash on demand.
    on_disk: Option<HashMap<String, String>>,
}

impl DeployedAgentHashes {
    /// EAGER: load the manifest and hash every catalog-named agent under
    /// `deployed_dir`.
    ///
    /// Why: the fleet-wide entry point — one call per distinct deployed
    /// directory per listing, shared across every session in that listing.
    /// What: [`AgentManifest::load`] plus one [`read_deployed`] per catalog
    /// agent stem, hashed with [`checksum`].
    /// Test: `session_asset_staleness_with_shared_matches_unshared`.
    pub fn read(deployed_dir: &Path, catalog: &CatalogHashes) -> Self {
        Self::with_manifest(AgentManifest::load(deployed_dir), deployed_dir, catalog)
    }

    /// [`Self::read`] with an ALREADY-LOADED manifest.
    ///
    /// Why: callers that load the manifest themselves must not have it
    /// silently re-loaded — that would change behavior when the on-disk
    /// manifest differs from what they hold.
    /// What: hashes the on-disk bodies and pairs them with `manifest`.
    /// Test: `session_asset_staleness_with_shared_matches_unshared`.
    pub fn with_manifest(
        manifest: AgentManifest,
        deployed_dir: &Path,
        catalog: &CatalogHashes,
    ) -> Self {
        let on_disk = catalog
            .agent_hashes
            .keys()
            .filter_map(|stem| {
                let body = read_deployed(&deployed_dir.join(format!("{stem}.md")))?;
                Some((stem.clone(), checksum(&body)))
            })
            .collect();
        Self {
            manifest,
            dir: deployed_dir.to_path_buf(),
            on_disk: Some(on_disk),
        }
    }

    /// LAZY: hash a deployed agent only when the comparison actually asks for
    /// it (#4619 review, MEDIUM).
    ///
    /// Why: a single-target caller shares its read with nobody, so paying to
    /// hash every catalog stem up front is pure loss when its manifest selects
    /// only a subset — the pre-#4322 code read exactly the selected stems.
    /// This form restores that exactly, so no call shape regresses.
    /// What: records the manifest and directory; [`Self::on_disk_hash`] reads
    /// and hashes per stem on demand.
    /// Test: `single_target_detect_reads_only_selected_agents`.
    pub fn lazy(manifest: AgentManifest, deployed_dir: &Path) -> Self {
        Self {
            manifest,
            dir: deployed_dir.to_path_buf(),
            on_disk: None,
        }
    }

    /// The on-disk checksum for one stem — from the eager snapshot when present,
    /// otherwise read and hashed now.
    ///
    /// Both branches yield the identical value for identical on-disk bytes; the
    /// only difference is WHEN the read happens, and therefore how many reads a
    /// caller that inspects a subset of stems performs.
    fn on_disk_hash(&self, stem: &str) -> Option<String> {
        match &self.on_disk {
            Some(map) => map.get(stem).cloned(),
            None => read_deployed(&self.dir.join(format!("{stem}.md"))).map(|b| checksum(&b)),
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
        // Skills stay a per-session read: unlike agents (#4409), they deploy
        // into `<workspace>/.claude/skills`, so two sessions genuinely can
        // disagree and there is nothing to share.
        let on_disk = read_deployed(&deployed_dir.join(stem).join("SKILL.md"));
        if let Some(kind) = classify_drift(
            catalog_hash,
            deployed.managed.get(stem).map(|e| e.checksum.as_str()),
            on_disk.as_deref().map(checksum).as_deref(),
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

/// Test-only call log for [`CatalogHashes::compute`], keyed by the exact
/// `(catalog_agents, catalog_skills)` pair each invocation was called with.
///
/// Why (issue #4326 review, HIGH — empirically proven): the anti-regression
/// pin for the shared-catalog invariant
/// (`staleness_inputs_computes_one_catalog_per_source_pair_shared_by_arc`)
/// only asserted the `Arc`-sharing structure of `staleness_inputs` directly —
/// it never exercised `stale_assets_for_many` (the actual hot path `tm ls`
/// calls), so moving `compute` inside that function's per-session
/// `JoinSet::spawn_blocking` fan-out left every existing test green. A LOG
/// (rather than a bare atomic counter) records the exact paths each call
/// used, so a test can filter to just the source pair IT constructed —
/// keyed by that test's own unique `fake_home()`-scoped temp directory — and
/// get a reliable exact-count assertion even though `cargo test` runs many
/// unrelated tests that also reach `compute` (directly or via
/// `detect_staleness`) concurrently in the same binary.
/// What: a process-wide `Mutex<Vec<(PathBuf, PathBuf)>>`, appended to by
/// every real `compute()` call; [`reset_compute_call_log`] clears it and
/// [`compute_calls_for`] counts entries matching one specific pair.
/// Test: `stale_assets_for_many_computes_catalog_exactly_once_per_source_pair`
/// in `daemon::managed_routes::tests`.
#[cfg(test)]
pub(crate) static COMPUTE_CALL_LOG: std::sync::LazyLock<
    std::sync::Mutex<Vec<(std::path::PathBuf, std::path::PathBuf)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Clear [`COMPUTE_CALL_LOG`] — call before the fan-out under test so earlier
/// tests' entries can never be mistaken for this test's calls.
#[cfg(test)]
pub(crate) fn reset_compute_call_log() {
    COMPUTE_CALL_LOG.lock().unwrap().clear();
}

/// Count logged [`CatalogHashes::compute`] calls for exactly one
/// `(catalog_agents, catalog_skills)` pair.
#[cfg(test)]
pub(crate) fn compute_calls_for(catalog_agents: &Path, catalog_skills: &Path) -> usize {
    COMPUTE_CALL_LOG
        .lock()
        .unwrap()
        .iter()
        .filter(|(a, s)| a == catalog_agents && s == catalog_skills)
        .count()
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
    /// never-synced check; otherwise computes both maps. Under `cfg(test)`,
    /// every call (regardless of outcome) is appended to
    /// `COMPUTE_CALL_LOG` before either branch runs.
    /// Test: `catalog_hashes_compute_is_unknown_without_either_source`,
    /// `stale_assets_for_many_computes_catalog_exactly_once_per_source_pair`.
    pub fn compute(catalog_agents: &Path, catalog_skills: &Path) -> Self {
        #[cfg(test)]
        COMPUTE_CALL_LOG
            .lock()
            .unwrap()
            .push((catalog_agents.to_path_buf(), catalog_skills.to_path_buf()));
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
    ///
    /// Issue #4322: the deployed AGENT side arrives pre-read as a
    /// [`DeployedAgentHashes`] so a fleet-wide caller can share ONE read of the
    /// machine-global agent deploy directory across every session. The deployed
    /// SKILL side is still read here, per call — skills deploy per workspace, so
    /// there is nothing to share.
    /// Test: `catalog_hashes_shared_across_two_deployed_targets`.
    pub fn detect(
        &self,
        deployed_agents: &DeployedAgentHashes,
        deployed_skills: &SkillManifest,
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
    let catalog = CatalogHashes::compute(catalog_agents, catalog_skills);
    // #4322 + #4619 review: single-target callers use the LAZY form, so they
    // read exactly the stems their predicates select — byte-for-byte the same
    // reads as before this change. Only the fleet-wide caller pre-hashes, and
    // only because it shares the result across every session in the listing.
    let deployed_agents = DeployedAgentHashes::lazy(deployed_agents.clone(), deployed_agents_dir);
    catalog.detect(
        &deployed_agents,
        deployed_skills,
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
/// What: resolves the effective [`HarnessManifest`](crate::core::manifest::HarnessManifest) for `project_dir` (the
/// project override layer; pass the framework root for the daemon-wide baseline),
/// materializes the [`HarnessPlan`](crate::core::manifest::HarnessPlan) to learn the catalog source dirs + selection
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
    let sources = crate::core::manifest::ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    // #4409: agent staleness is measured against the tier agents deploy into
    // (the tm-managed config dir); skills still deploy per-workspace.
    let agents_dir = fw.agent_deploy_dir();
    let skills_dir = fw.claude_skills_dir();
    let deployed_agents = AgentManifest::load(&agents_dir);
    // #5626: same rule as `session_assets` — a ledger this read could not
    // establish leaves staleness `unknown`, never fresh.
    let deployed_skills = match SkillManifest::load(&skills_dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                dir = %skills_dir.display(),
                error = %e,
                "skill ownership ledger unreadable — catalog staleness is undetermined"
            );
            return StalenessReport {
                unknown: true,
                ..StalenessReport::default()
            };
        }
    };

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
