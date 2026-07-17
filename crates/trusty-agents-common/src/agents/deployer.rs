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
//! it safely may. It uses atomic write-temp-then-rename for both content files
//! and the manifest. Corrupt manifests are detected and surfaced as errors
//! rather than silently reset to empty. Returns a [`DeployResult`] summarising
//! what happened.
//! Test: `cargo test -p trusty-agents-common agents::deployer` covers a new
//! deploy, a skipped user-modified file, an unchanged file, a user-owned file,
//! atomic writes, and corrupt manifest detection.

use std::collections::HashMap;
use std::path::Path;

use crate::agents::builder::{AgentBuildError, compose_agent, source_chain};
use crate::agents::manifest::{
    AgentManifest, ManifestEntry, ManifestError, ManifestLoad, Origin, atomic_write, checksum,
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
    /// Filenames skipped because the user modified them, or because they were
    /// untracked and differ from the fresh composition.
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
///   - In manifest, checksum differs → user-modified → warn + skip
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
    let mut result = DeployResult::default();

    // No source directory means nothing to deploy — an empty result, not an
    // error, so a fresh install with no agents still succeeds.
    if !source_dir.is_dir() {
        return Ok(result);
    }

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
            if manifest.checksum_matches(&filename, &current) {
                if checksum(&composed) == checksum(&current) {
                    // Deployed copy is already the latest composition.
                    result.unchanged.push(filename);
                    continue;
                }
                // Managed and unmodified by the user → safe to refresh.
            } else {
                // Managed but the user edited it → preserve their changes.
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

#[cfg(test)]
#[path = "deployer_tests.rs"]
mod tests;
