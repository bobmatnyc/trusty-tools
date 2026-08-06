//! Agent deployment — writes composed agents into `~/.claude/agents/`.
//!
//! Why: Claude Code reads agent files from `~/.claude/agents/`. The deploying
//! harness must keep that directory populated with up-to-date *composed*
//! (inheritance-flattened) agents, while never destroying files the user owns
//! or has hand-edited. Moved to trusty-agents-common (#2892) from
//! `trusty-mpm::core::agent_deployer` — both `source_dir` and `target_dir` were
//! already plain parameters, so the move required no path-coupling changes;
//! `trusty-mpm` re-exports every item here from `core::agent_deployer` for
//! source compatibility.
//! What: [`deploy_agents`] composes every source agent, consults the
//! [`AgentManifest`] to classify each target file, and writes only the files
//! it safely may. Every freshly composed agent is strict-YAML-validated via
//! [`crate::agents::frontmatter::validate_frontmatter`] (issue #3556) before
//! it is written — a composition trusty-mpm's own lenient reader accepts but
//! a strict consumer (e.g. `trusty-agents`' `serde_yaml`-based `.md` loader)
//! would reject is treated like a compose failure: logged loudly, recorded in
//! [`DeployResult::failed`], and skipped, never written to `target_dir`. It
//! uses atomic write-temp-then-rename for both content files and the
//! manifest. Corrupt manifests are detected and surfaced as errors rather
//! than silently reset to empty. Returns a [`DeployResult`] summarising what
//! happened.
//! Ownership follows the two-variant install policy (#4408): a manifest entry
//! whose origin is FRAMEWORK-owned (bundled, the `Overwrite` tier) is
//! re-deployed whenever its on-disk checksum drifts — a mismatch there means
//! corruption, not user ownership — while a user-owned entry (the `SeedOnce`
//! tier) and any untracked file keep the preserve-on-mismatch behavior.
//! [`retract_framework_agents`] is the inverse operation (#4409): it deletes
//! exactly the framework-owned entries this deployer wrote into a directory
//! that is no longer a deploy destination, using the same ownership predicate,
//! and never touches an untracked or user-owned file.
//! Both directions run their WHOLE load-modify-save cycle under
//! [`crate::agents::manifest::with_agent_manifest_lock`] (#4409): the agent
//! deploy target is now one machine-global directory shared by every concurrent
//! session launch, sync-assets run, and catalog apply, so an unlocked cycle
//! loses one writer's ledger entries and the files they described are then
//! treated as untracked and frozen — #4408's shape, reached by a race.
//! Test: `cargo test -p trusty-agents-common agents::deployer` covers a new
//! deploy, a preserved user-owned file, an unchanged file, atomic writes,
//! corrupt manifest detection, (#3556) a stale-broken-copy refresh plus a
//! strict-YAML-invalid composition being isolated and skipped, (#4408) a
//! corrupted bundled file being re-deployed while a modified user-owned entry
//! survives byte-identical, and (#4409) retraction removing only the
//! framework-owned tier.

use std::collections::HashMap;
use std::path::Path;

use crate::agents::builder::{AgentBuildError, compose_agent, source_chain};
use crate::agents::frontmatter::validate_frontmatter;
use crate::agents::manifest::{
    AgentManifest, MANIFEST_FILE, ManifestEntry, ManifestError, ManifestLoad, Origin, atomic_write,
    checksum, manifest_lock_path, with_agent_manifest_lock,
};
use crate::agents::metadata::agent_metadata_from_str;

/// Summary of one [`deploy_agents`] run.
///
/// Why: the CLI prints per-file status; callers need the file lists split by
/// outcome to render that summary and to know whether any work was skipped.
/// What: filenames grouped into freshly written, skipped (user-modified),
/// unchanged (checksum already current), and silently adopted (untracked but
/// byte-identical to the fresh composition — see [`adopted`]).
/// [`untracked_modified`] is a subset of `skipped`: files that were absent
/// from the manifest AND differ from the fresh composition, which is the
/// specific case `--reset-agents` exists to resolve (issue #2504).
/// Test: every `deploy_*` test asserts on these vectors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployResult {
    /// Filenames successfully (re)written this run.
    pub deployed: Vec<String>,
    /// Filenames skipped because they are user-owned — either untracked by the
    /// manifest and differing from the fresh composition, or tracked with a
    /// user-owned origin (the seed-once tier) and edited. A tracked
    /// FRAMEWORK-owned file that differs is re-deployed, not skipped (#4408).
    pub skipped: Vec<String>,
    /// Filenames left untouched because their checksum already matched.
    pub unchanged: Vec<String>,
    /// Filenames that were untracked by the manifest but byte-identical to
    /// the fresh composition — registered into the manifest without a
    /// rewrite (issue #2504 adoption path).
    pub adopted: Vec<String>,
    /// Filenames that were untracked by the manifest AND differ from the
    /// fresh composition — skipped conservatively, but flagged so the
    /// operator knows `tm install --reset-agents` is available to reconcile
    /// them. Always a subset of `skipped`.
    pub untracked_modified: Vec<String>,
    /// Declared `skills:` per processed agent, keyed by agent name (stem, not
    /// filename) — DOC-42, issue #2889.
    ///
    /// Why: co-deployment (§SPEC-AGENTSKILLS-02) needs every selected agent's
    /// declared skill dependencies to fold into the skill deployer's `select`
    /// predicate, regardless of whether THIS run wrote/skipped/adopted the
    /// agent file — the declaration is a property of the agent's composed
    /// definition, not of this run's write outcome.
    /// What: populated for every agent name this run processed (i.e. every
    /// selected source agent), from the freshly composed content's `skills:`
    /// frontmatter (empty `Vec` when the agent declares none).
    /// Test: `declared_skills_populated_for_every_processed_agent`,
    /// `declared_skills_empty_when_agent_declares_none`.
    pub declared_skills: HashMap<String, Vec<String>>,
    /// Agents whose compose step failed, as `"<name>: <error>"` — DOC-42,
    /// issue #2906 review (CRITICAL finding).
    ///
    /// Why: a single malformed agent asset (e.g. unterminated frontmatter)
    /// must never abort the ENTIRE roster deploy — every other well-formed
    /// agent still needs to land, and the caller (`tm install`, session
    /// launch) needs to know WHICH agent(s) were skipped and why, rather than
    /// the whole operation failing with no roster deployed at all.
    /// What: one entry per source agent whose [`compose_agent`] call
    /// returned `Err`; that agent is neither composed nor written, and
    /// processing continues with the next agent.
    /// Test: `deploy_isolates_single_malformed_agent_failure`.
    pub failed: Vec<String>,
    /// Framework-owned files that had drifted from their recorded checksum and
    /// were rewritten from the bundle — the #4408 corruption-repair branch.
    ///
    /// Why: `tm reinstall` reports per destination what it deployed, preserved,
    /// repaired, and failed, and "repaired" is the outcome an operator most
    /// needs to see — it means a managed file had been corrupted (invalid
    /// frontmatter, a truncated stub) and was recovered. Until now that branch
    /// only emitted a `tracing::warn!` and the file landed in `deployed`,
    /// indistinguishable from an ordinary refresh.
    /// What: always a subset of [`deployed`](Self::deployed). Empty when no
    /// managed file had drifted.
    /// Test: `deploy_redeploys_corrupted_bundled_file`.
    pub repaired: Vec<String>,
}

/// Whether a source filename names an agent to compose.
///
/// Why: the source directory holds `.md` files; only those should be composed,
/// and the manifest file (if it ever appears there) must be ignored. Widened
/// from `pub(crate)` to `pub` in the #2892 move so `trusty-mpm`'s
/// `agent_reset` module (a different crate) can keep calling it through the
/// `core::agent_deployer` re-export.
/// What: returns `true` for `*.md` files other than the manifest.
/// Test: covered indirectly by `deploy_new_agent`.
pub fn is_agent_file(name: &str) -> bool {
    name.ends_with(".md")
}

/// Deploy all agents from source_dir to target_dir.
///
/// Why: ensures ~/.claude/agents/ has up-to-date composed agent files
/// without clobbering user-owned or user-modified files.
///
/// Rules:
///   - Not in manifest → user-owned → skip silently
///   - In manifest, checksum matches → overwrite (safe)
///   - In manifest, checksum differs, entry is framework-owned
///     ([`Origin::is_framework_owned`], the Overwrite tier) → drift or
///     corruption → warn + re-deploy the bundled composition (issue #4408)
///   - In manifest, checksum differs, entry is user-owned (the seed-once
///     tier) → user-modified → skip, preserving their content byte-for-byte
///   - New trusty-mpm agent → compose + write (atomic) + add to manifest
///   - Corrupt manifest → error (never silently reset, which would reclassify
///     managed files as user-owned and skip re-deploying them)
///
/// Atomic safety: every content file is written via write-temp-then-rename
/// so a crash between writes leaves the old file intact. The manifest is also
/// written atomically via [`AgentManifest::save`].
///
/// Test: `deploy_new_agent`, `deploy_skips_user_modified`, `deploy_unchanged_no_write`,
///       `deploy_aborts_on_corrupt_manifest`, `deploy_content_file_is_atomic`.
pub fn deploy_agents(
    source_dir: &Path,
    target_dir: &Path,
) -> Result<DeployResult, AgentBuildError> {
    // Default policy: deploy every agent in the source directory.
    deploy_agents_filtered(source_dir, target_dir, |_name| true)
}

/// Deploy agents from `source_dir`, restricting to those `select` accepts.
///
/// Why: HR-2 manifests describe an agent *set* (include/exclude globs), so the
/// session-launch path must be able to deploy a subset of the source agents
/// without copying the whole directory. Factoring the selection into a predicate
/// keeps the manifest logic in `session_launch` while reusing the identical
/// compose/ownership/atomic-write machinery here.
/// What: identical to [`deploy_agents`] except a source agent whose stem (the
/// `.md`-stripped name) the `select` predicate rejects is skipped entirely — it
/// is neither composed nor written, and an already-deployed managed copy is left
/// in place (deselecting an agent does not remove a previously deployed file;
/// that is an HR-3 concern). [`deploy_agents`] delegates here with an accept-all
/// predicate, so existing behavior is unchanged.
/// Test: `deploy_filtered_respects_predicate`.
pub fn deploy_agents_filtered(
    source_dir: &Path,
    target_dir: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<DeployResult, AgentBuildError> {
    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no agents still succeeds. Checked BEFORE
    // taking the ledger lock so a no-op deploy neither blocks on a concurrent
    // writer nor creates a lock sidecar in a directory it will not touch.
    if !source_dir.is_dir() {
        return Ok(DeployResult::default());
    }

    // #4409: the ENTIRE load-modify-save cycle runs under the directory's
    // exclusive ledger lock. `target_dir` is now one machine-global directory
    // shared by every concurrent session launch, sync-assets run, and
    // `tm catalog apply`; without this, two writers that both load before
    // either saves silently drop each other's entries, and the files those
    // entries describe are then treated as untracked and frozen (the #4408
    // shape, via a race). See `manifest::with_agent_manifest_lock`.
    with_agent_manifest_lock(target_dir, || {
        deploy_agents_locked(source_dir, target_dir, select)
    })
}

/// The body of [`deploy_agents_filtered`], run while holding the ledger lock.
///
/// Why: split out so the critical section is a single expression the lock
/// helper can wrap, and so the lock's scope is impossible to misread — every
/// manifest load, file write, and manifest save in this function happens with
/// the lock held.
/// What: the compose/classify/write/save pipeline documented on
/// [`deploy_agents_filtered`]. Never call it directly; it is unsafe against
/// concurrent writers by construction.
/// Test: covered by every `deploy_*` test through the public wrapper.
fn deploy_agents_locked(
    source_dir: &Path,
    target_dir: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<DeployResult, AgentBuildError> {
    let mut result = DeployResult::default();

    // Detect manifest corruption before touching any file. A corrupt manifest
    // must surface as an error — resetting to empty would reclassify all
    // managed files as user-owned and silently skip the entire deploy.
    let mut manifest = match AgentManifest::load_checked(target_dir) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => {
            return Err(AgentBuildError::FrontmatterParse(format!(
                "agent manifest is corrupt and cannot be safely loaded; \
                 run `tm repair deploy` to recover. Detail: {detail}"
            )));
        }
    };
    let now = chrono::Utc::now().to_rfc3339();

    // Collect agent names deterministically so output and tests are stable.
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file() && is_agent_file(name) {
            let stem = name.trim_end_matches(".md").to_string();
            // Honour the manifest's agent-set selection (HR-2). A deselected
            // agent is skipped before compose, so it is never written.
            if select(&stem) {
                names.push(stem);
            }
        }
    }
    names.sort_unstable();

    for name in names {
        let filename = format!("{name}.md");
        // DOC-42 issue #2906 review (CRITICAL): isolate a per-agent compose
        // failure instead of propagating it with `?` — one malformed asset
        // (e.g. unterminated frontmatter) must not abort the entire roster
        // deploy. Log loudly, record it, and move on to the next agent.
        let composed = match compose_agent(&name, source_dir) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(
                    agent = %name,
                    "agent compose FAILED — skipping this agent, roster deploy \
                     continues for the rest of the roster: {err}"
                );
                result.failed.push(format!("{name}: {err}"));
                continue;
            }
        };

        // Issue #3556: `compose_agent`'s success only means trusty-mpm's own
        // LENIENT frontmatter reader could parse the result — it tolerates a
        // colon anywhere in a scalar value, which a strict YAML consumer
        // (`trusty-agents`' `serde_yaml`-based `.md` agent loader) rejects.
        // Validate every freshly composed agent with the SAME strict parser
        // real consumers use before it is ever written to `target_dir`, so a
        // malformed composition is caught loudly here — naming the offending
        // agent and the exact YAML problem — instead of silently landing in
        // `.claude/agents/` and only failing at `tagent` runtime.
        if let Err(detail) = validate_frontmatter(&composed) {
            tracing::error!(
                agent = %name,
                file = %filename,
                "composed agent frontmatter FAILED strict YAML validation — \
                 skipping this agent, roster deploy continues for the rest of \
                 the roster: {detail}"
            );
            result
                .failed
                .push(format!("{name}: invalid frontmatter YAML: {detail}"));
            continue;
        }
        let target_path = target_dir.join(&filename);

        // DOC-42: record declared skills for EVERY processed agent, before
        // any of the branches below `continue` — the declaration is a
        // property of the composed definition, independent of whether this
        // run actually rewrites the target file.
        result
            .declared_skills
            .insert(name.clone(), agent_metadata_from_str(&composed).skills);

        // Classify the existing target file, if any.
        if target_path.exists() {
            if !manifest.is_managed(&filename) {
                // Untracked (issue #2504): the manifest predates per-file
                // tracking for this file, or it was never registered. Rather
                // than permanently orphaning it, compare against the fresh
                // composition — if byte-identical, silently adopt it (the
                // content already matches what trusty-mpm would have written,
                // so registering it is safe and reconciles the manifest gap).
                // Otherwise keep the conservative skip (it may be genuinely
                // user-owned) but flag it for the `--reset-agents` warning.
                let current = std::fs::read_to_string(&target_path)?;
                if checksum(&current) == checksum(&composed) {
                    manifest.managed.insert(
                        filename.clone(),
                        ManifestEntry {
                            source_chain: source_chain(&name, source_dir)?,
                            checksum: checksum(&composed),
                            deployed_at: now.clone(),
                            origin: Origin::Bundled,
                        },
                    );
                    result.adopted.push(filename);
                } else {
                    result.skipped.push(filename.clone());
                    result.untracked_modified.push(filename);
                }
                continue;
            }
            let current = std::fs::read_to_string(&target_path)?;
            // #4408: snapshot the recorded checksum + ownership tier before the
            // branches below take a mutable borrow of `manifest` to re-register
            // the file.
            let recorded_owner = manifest
                .managed
                .get(&filename)
                .map(|e| (e.checksum.clone(), e.origin.is_framework_owned()));
            if manifest.checksum_matches(&filename, &current) {
                if checksum(&composed) == checksum(&current) {
                    // Deployed copy is already the latest composition.
                    result.unchanged.push(filename);
                    continue;
                }
                // Managed and unmodified by the user → safe to refresh.
            } else if let Some((expected, true)) = recorded_owner {
                // #4408: a framework-owned (bundled / Overwrite-tier) file that
                // no longer matches its recorded checksum is DRIFT or
                // CORRUPTION, not user ownership — freezing it left a corrupted
                // agent (a 32-byte `v1` stub in the reported incident)
                // unrecoverable forever, because corrupt content can never
                // checksum-match again. Re-deploy it, loudly, so the corruption
                // event is visible instead of silently skipped.
                tracing::warn!(
                    file = %filename,
                    expected_checksum = %expected,
                    found_checksum = %checksum(&current),
                    found_bytes = current.len(),
                    "bundled agent file drifted from its manifest checksum — \
                     re-deploying the bundled composition over it (framework-owned \
                     files are refreshed, not preserved; issue #4408)"
                );
                // The write below records this in `deployed`; also record it
                // here so a caller can distinguish a corruption REPAIR from an
                // ordinary refresh (`tm reinstall`'s per-destination report).
                result.repaired.push(filename.clone());
            } else {
                // Managed but user-owned (Origin::User / Registry — the
                // seed-once tier) and edited → preserve their changes.
                result.skipped.push(filename);
                continue;
            }
        }

        // Write (new file, or safe refresh of a managed file) atomically.
        // Using write-temp-then-rename guarantees that a crash between the
        // content write and the subsequent manifest save leaves the old content
        // file intact — never a half-written one.
        std::fs::create_dir_all(target_dir)?;
        atomic_write(&target_path, &composed).map_err(|e| match e {
            ManifestError::Io(io) => AgentBuildError::Io(io),
            other => AgentBuildError::FrontmatterParse(other.to_string()),
        })?;
        manifest.managed.insert(
            filename.clone(),
            ManifestEntry {
                source_chain: source_chain(&name, source_dir)?,
                checksum: checksum(&composed),
                deployed_at: now.clone(),
                origin: Origin::Bundled,
            },
        );
        result.deployed.push(filename);
    }

    // Issue #2504: warn ONCE with a count + short preview rather than one
    // line per untracked-modified file (this set can be dozens of files wide
    // on a machine that predates per-file manifest tracking) — actionable
    // without spamming the operator's terminal.
    if !result.untracked_modified.is_empty() {
        let count = result.untracked_modified.len();
        let preview = result
            .untracked_modified
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let ellipsis = if count > 5 { ", …" } else { "" };
        tracing::warn!(
            count,
            "{count} agent file(s) untracked by the deploy manifest differ from the \
             bundled composition and were skipped: {preview}{ellipsis}. Run \
             `tm install --reset-agents` to review and reconcile them."
        );
    }

    manifest.save(target_dir).map_err(|e| match e {
        ManifestError::Io(io) => AgentBuildError::Io(io),
        other => AgentBuildError::FrontmatterParse(other.to_string()),
    })?;

    Ok(result)
}

/// Summary of one [`retract_framework_agents`] run.
///
/// Why: the caller reports what a workspace retraction actually removed, and
/// tests need to assert that user-owned entries and untracked files survived.
/// What: filenames removed (framework-owned, manifest-tracked) and filenames
/// deliberately preserved (manifest-tracked but user-owned — the seed-once
/// tier). Untracked files never appear in either list: they are invisible to
/// this function by construction, which is exactly the guarantee.
/// Test: `retract_removes_framework_owned_only`,
/// `retract_preserves_untracked_hand_placed_file`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetractResult {
    /// Framework-owned filenames deleted from `target_dir` this run.
    pub removed: Vec<String>,
    /// Manifest-tracked but user-owned filenames left in place.
    pub preserved: Vec<String>,
}

/// Remove the FRAMEWORK-owned agents this deployer previously wrote into
/// `target_dir`, leaving everything else byte-identical (issue #4409).
///
/// Why: bundled agents moved from per-workspace `.claude/agents/` to the
/// tm-managed user tier. Stopping the writes is not enough — a workspace
/// provisioned by an older binary still holds a full copy of the bundled
/// roster, and the project tier OUTRANKS the user tier in Claude Code's agent
/// resolution. Left in place those copies would shadow the new canonical tier
/// forever and would no longer be refreshed by any deploy, which is strictly
/// worse than the pre-#4409 behavior (it is #4408's shadowing incident made
/// permanent). Retraction is the other half of the flip, not cleanup polish.
/// What: reads the ownership manifest in `target_dir` and deletes exactly the
/// entries whose [`Origin::is_framework_owned`] is `true` — the same
/// `Overwrite`-tier predicate the deployer uses to decide a file is tm's to
/// rewrite (#4408) — then saves the pruned manifest. Checksum is deliberately
/// NOT consulted: a framework-owned file that drifted is corruption, not user
/// ownership, and corrupt content can never checksum-match again. A file
/// absent from the manifest is NEVER touched (hand-placed agents), and a
/// tracked entry with a user-owned origin (the seed-once tier) is kept and
/// reported in [`RetractResult::preserved`]. A missing `target_dir` or missing
/// manifest is an empty no-op. A CORRUPT manifest is an error and removes
/// nothing — deleting files on the strength of an unreadable ledger is exactly
/// the failure mode the manifest exists to prevent. When no managed entries
/// remain, the manifest file itself is removed (and `target_dir` too, if it is
/// then empty) so a retracted workspace returns to pristine rather than
/// carrying an empty ledger forever.
/// Test: `retract_removes_framework_owned_only`,
/// `retract_preserves_untracked_hand_placed_file`,
/// `retract_is_idempotent`, `retract_missing_dir_is_a_noop`,
/// `retract_refuses_on_corrupt_manifest`,
/// `retract_removes_drifted_framework_file`.
pub fn retract_framework_agents(target_dir: &Path) -> Result<RetractResult, AgentBuildError> {
    // Default policy: retract every framework-owned entry.
    retract_framework_agents_filtered(target_dir, |_name| true)
}

/// Retract the framework-owned agents in `target_dir` whose stem `select`
/// accepts.
///
/// Why: `tm install --reset-agents <names> --reset-agents-workspaces` names a
/// specific agent scope, and the workspace half of that sweep is a RETRACTION
/// since #4409 (there is nothing legitimate to recompose into a workspace any
/// more). Honouring the operator's stated scope there needs the same
/// stem-predicate seam [`deploy_agents_filtered`] already provides for the
/// deploy direction, so the two mirror each other rather than diverging.
/// What: identical to [`retract_framework_agents`] except a framework-owned
/// entry whose `.md`-stripped stem `select` rejects is left on disk AND left in
/// the manifest — it is neither removed nor reported, exactly as if it were not
/// framework-owned. [`retract_framework_agents`] delegates here with an
/// accept-all predicate, so the unfiltered behavior is unchanged.
/// Test: `retract_filtered_respects_predicate`.
pub fn retract_framework_agents_filtered(
    target_dir: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<RetractResult, AgentBuildError> {
    // A directory that was never deployed into has nothing to retract. Checked
    // before the lock so the common no-op neither blocks nor CREATES the
    // directory (which `with_agent_manifest_lock` would, to place its sidecar)
    // — materialising an empty `.claude/agents/` in every workspace on every
    // launch would be a visible regression of its own.
    if !target_dir.is_dir() {
        return Ok(RetractResult::default());
    }

    // #4409: same exclusive ledger lock the deploy path takes — retraction is a
    // load-modify-save of the very same document.
    with_agent_manifest_lock(target_dir, || retract_locked(target_dir, select))
}

/// The body of [`retract_framework_agents_filtered`], run holding the lock.
///
/// Why/What: mirrors [`deploy_agents_locked`] — the critical section is one
/// expression so the lock's scope cannot be misread. Never call it directly.
/// Test: covered by every `retract_*` test through the public wrapper.
fn retract_locked(
    target_dir: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<RetractResult, AgentBuildError> {
    let mut result = RetractResult::default();

    let mut manifest = match AgentManifest::load_checked(target_dir) {
        ManifestLoad::Ok(m) => m,
        ManifestLoad::Corrupt(detail) => {
            return Err(AgentBuildError::FrontmatterParse(format!(
                "agent manifest is corrupt; refusing to retract any file on the strength \
                 of an unreadable ownership ledger. `tm repair deploy` cannot fix this — \
                 it only repairs the user-tier deploy, not this workspace's ledger. \
                 Delete `.claude/agents/.trusty-mpm-manifest.json` in this workspace by \
                 hand (and any stale framework-owned agent files alongside it) to clear \
                 it; the next session launch will not redeploy here since bundled agents \
                 now come from the user tier. Detail: {detail}"
            )));
        }
    };

    // Deterministic order so logs and tests are stable.
    let mut tracked: Vec<String> = manifest.managed.keys().cloned().collect();
    tracked.sort_unstable();

    for filename in tracked {
        let framework_owned = manifest
            .managed
            .get(&filename)
            .is_some_and(|entry| entry.origin.is_framework_owned());
        if !framework_owned {
            result.preserved.push(filename);
            continue;
        }
        if !select(filename.trim_end_matches(".md")) {
            continue;
        }
        match std::fs::remove_file(target_dir.join(&filename)) {
            Ok(()) => {}
            // Already gone (operator deleted it, or a previous partial run):
            // still drop the ledger entry so the manifest stops claiming it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // Persist what we HAVE removed before surfacing the failure.
                // Returning early with the pruned entries only in memory would
                // leave the on-disk ledger claiming files that are already
                // gone — a window the next run self-heals (`NotFound` above is
                // tolerated), but an avoidable one.
                save_pruned(&manifest, target_dir, &result);
                return Err(AgentBuildError::Io(e));
            }
        }
        manifest.managed.remove(&filename);
        result.removed.push(filename);
    }

    if result.removed.is_empty() {
        return Ok(result);
    }

    tracing::info!(
        removed = result.removed.len(),
        preserved = result.preserved.len(),
        dir = %target_dir.display(),
        "retracted per-workspace bundled agents — the canonical tier is now the \
         tm-managed CLAUDE_CONFIG_DIR (issue #4409)"
    );

    if manifest.managed.is_empty() {
        // Nothing left to track: drop the ledger, and the directory too when
        // the retraction emptied it, so the workspace looks untouched.
        match std::fs::remove_file(target_dir.join(MANIFEST_FILE)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AgentBuildError::Io(e)),
        }
        // The lock sidecar we are holding is the one file guaranteed to be
        // here, so "empty" means "nothing but the sidecar". Removing it while
        // still holding the lock is safe on Unix — `unlink` drops the NAME, not
        // the open file description the lock lives on — and the directory is
        // being abandoned, so a writer blocked on the now-orphaned inode simply
        // proceeds against a directory with nothing left to race over. Only
        // reached when the retraction removed every tracked file AND no
        // hand-placed file remains; anything else leaves the sidecar in place.
        if only_the_lock_sidecar_remains(target_dir)? {
            let _ = std::fs::remove_file(manifest_lock_path(target_dir));
        }
        if std::fs::read_dir(target_dir)?.next().is_none() {
            // Best-effort: a concurrent writer racing us here is harmless.
            let _ = std::fs::remove_dir(target_dir);
        }
        return Ok(result);
    }

    manifest.save(target_dir).map_err(|e| match e {
        ManifestError::Io(io) => AgentBuildError::Io(io),
        other => AgentBuildError::FrontmatterParse(other.to_string()),
    })?;
    Ok(result)
}

/// Whether `target_dir` holds nothing but the ledger lock sidecar.
///
/// Why: the "return the workspace to pristine" step wants to know whether the
/// retraction emptied the directory, but the caller is HOLDING a lock whose
/// sidecar lives in that very directory — so a naive `read_dir().next().is_none()`
/// is never true and the cleanup silently stops working the moment locking is
/// introduced. Naming the exception keeps the emptiness test honest.
/// What: `true` when every entry is [`manifest_lock_path`]'s file name; `false`
/// when any other entry (a hand-placed agent, a user-owned file) remains.
/// Test: `retract_clears_manifest_and_dir_when_nothing_remains`,
/// `retract_preserves_untracked_hand_placed_file`.
fn only_the_lock_sidecar_remains(target_dir: &Path) -> Result<bool, AgentBuildError> {
    let sidecar = manifest_lock_path(target_dir);
    let sidecar_name = sidecar.file_name();
    for entry in std::fs::read_dir(target_dir)? {
        if Some(entry?.file_name().as_os_str()) != sidecar_name {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Best-effort persist of a partially-pruned manifest on the retraction error
/// path.
///
/// Why: a mid-loop `remove_file` failure (e.g. `EACCES` on file 4 of 6) must
/// not leave the ledger claiming the files already deleted. The subsequent
/// `Err` return is what the caller acts on; this write only narrows the
/// inconsistency window, so its own failure is deliberately swallowed — there
/// is nothing useful to do about it and it must not mask the original error.
/// What: saves `manifest` when this run removed anything; a no-op otherwise.
/// Test: covered via `retract_removes_framework_owned_only`'s ledger
/// assertions (the success path uses the same `save`); the failure path is
/// filesystem-permission-dependent and not simulated.
fn save_pruned(manifest: &AgentManifest, target_dir: &Path, result: &RetractResult) {
    if result.removed.is_empty() {
        return;
    }
    if let Err(e) = manifest.save(target_dir) {
        tracing::warn!(
            dir = %target_dir.display(),
            "could not persist the partially-pruned agent manifest after a retraction \
             failure; the next run self-heals: {e}"
        );
    }
}

#[cfg(test)]
#[path = "deployer_tests.rs"]
mod tests;
