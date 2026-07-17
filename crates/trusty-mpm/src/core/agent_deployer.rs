//! Agent deployment — writes composed agents into `~/.claude/agents/`.
//!
//! Why: Claude Code reads agent files from `~/.claude/agents/`. trusty-mpm must
//! keep that directory populated with up-to-date *composed* (inheritance-
//! flattened) agents, while never destroying files the user owns or has
//! hand-edited.
//! What: [`deploy_agents`] composes every source agent, consults the
//! [`AgentManifest`] to classify each target file, and writes only the files
//! it safely may. It uses atomic write-temp-then-rename for both content files
//! and the manifest. Corrupt manifests are detected and surfaced as errors
//! rather than silently reset to empty. Returns a [`DeployResult`] summarising
//! what happened.
//! Test: `cargo test -p trusty-mpm-core agent_deployer` covers a new deploy, a
//! skipped user-modified file, an unchanged file, a user-owned file, atomic
//! writes, and corrupt manifest detection.

use std::collections::HashMap;
use std::path::Path;

use crate::core::agent_builder::{AgentBuildError, compose_agent, source_chain};
use crate::core::agent_manifest::{
    AgentManifest, ManifestEntry, ManifestLoad, Origin, atomic_write, checksum,
};
use crate::core::agent_metadata::agent_metadata_from_str;

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

/// Whether a source filename names a trusty-mpm agent to compose.
///
/// Why: the source directory holds `.md` files; only those should be composed,
/// and the manifest file (if it ever appears there) must be ignored.
/// What: returns `true` for `*.md` files other than the manifest.
/// Test: covered indirectly by `deploy_new_agent`.
pub(crate) fn is_agent_file(name: &str) -> bool {
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
            crate::core::error::Error::Io(io) => AgentBuildError::Io(io),
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
        crate::core::error::Error::Io(io) => AgentBuildError::Io(io),
        other => AgentBuildError::FrontmatterParse(other.to_string()),
    })?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A two-file source set: a base agent and a leaf that extends it.
    fn write_sources(dir: &Path) {
        fs::write(
            dir.join("base-agent.md"),
            "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBase content.\n",
        )
        .unwrap();
        fs::write(
            dir.join("engineer.md"),
            "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nEngineer content.\n",
        )
        .unwrap();
    }

    #[test]
    fn deploy_new_agent() {
        // A first-ever deploy must write every composed agent and record it
        // in the manifest.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert_eq!(result.deployed.len(), 2);
        assert!(result.deployed.contains(&"engineer.md".to_string()));
        assert!(result.skipped.is_empty());
        assert!(result.unchanged.is_empty());

        // Files exist and the composed engineer carries inherited content.
        let engineer = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(engineer.contains("Base content."));
        assert!(engineer.contains("Engineer content."));

        // The manifest records the resolved chain.
        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));
        assert_eq!(
            manifest.managed["engineer.md"].source_chain,
            vec!["base-agent", "engineer"]
        );
    }

    #[test]
    fn deploy_skips_user_modified() {
        // A managed file the user edited must be skipped, not overwritten.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // First deploy establishes the manifest.
        deploy_agents(src.path(), tgt.path()).unwrap();

        // User edits the deployed engineer file.
        fs::write(
            tgt.path().join("engineer.md"),
            "---\nname: engineer\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        // Second deploy must preserve the user's edit.
        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"engineer.md".to_string()));
        assert!(!result.deployed.contains(&"engineer.md".to_string()));

        let still = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(still.contains("USER HAND-EDIT"));
    }

    #[test]
    fn deploy_adopts_untracked_byte_identical_file() {
        // Issue #2504: a target file present on disk but absent from the
        // manifest (predates per-file tracking) must be silently adopted
        // when its content already equals the fresh composition — no
        // rewrite, just registration — so a later bundle change can reach it.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Pre-create the target with exactly what compose_agent would
        // produce, simulating a file deployed before manifest tracking
        // existed for it.
        let composed = crate::core::agent_builder::compose_agent("engineer", src.path()).unwrap();
        fs::write(tgt.path().join("engineer.md"), &composed).unwrap();
        let before = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.adopted.contains(&"engineer.md".to_string()));
        assert!(!result.skipped.contains(&"engineer.md".to_string()));
        assert!(!result.deployed.contains(&"engineer.md".to_string()));

        // Not rewritten — adoption is registration-only.
        let after = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "adopted file must not be rewritten");

        // Now registered — a subsequent bundle change must be able to reach
        // it (the whole point of adoption).
        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));

        fs::write(
            src.path().join("engineer.md"),
            "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nUPDATED.\n",
        )
        .unwrap();
        let second = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(second.deployed.contains(&"engineer.md".to_string()));
        let updated = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(updated.contains("UPDATED."));
    }

    #[test]
    fn deploy_flags_untracked_modified_file_for_reset() {
        // An untracked file whose content differs from the fresh composition
        // must be skipped AND recorded in `untracked_modified` so the CLI can
        // point the operator at `tm install --reset-agents`.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());
        fs::write(
            tgt.path().join("engineer.md"),
            "PRE-EXISTING, NOT TRUSTY-MPM'S CURRENT COMPOSITION\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"engineer.md".to_string()));
        assert!(
            result
                .untracked_modified
                .contains(&"engineer.md".to_string())
        );
        assert!(!AgentManifest::load(tgt.path()).is_managed("engineer.md"));
    }

    #[test]
    fn deploy_unchanged_no_write() {
        // A second deploy with no source changes must report files unchanged
        // and not rewrite them.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_agents(src.path(), tgt.path()).unwrap();
        let before = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.unchanged.contains(&"engineer.md".to_string()));
        assert!(result.deployed.is_empty());

        let after = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "unchanged file must not be rewritten");
    }

    #[test]
    fn deploy_user_owned_skipped() {
        // A file in the target that trusty-mpm never deployed (absent from the
        // manifest) must be left completely untouched.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // User pre-creates a file matching a source agent's name.
        fs::write(
            tgt.path().join("engineer.md"),
            "USER OWNED — not trusty-mpm's\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"engineer.md".to_string()));

        // The user's content survives untouched.
        let content = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

        // base-agent.md had no conflict, so it deploys normally.
        assert!(result.deployed.contains(&"base-agent.md".to_string()));
    }

    #[test]
    fn deploy_missing_source_dir_is_empty_result() {
        // Deploying from a non-existent source directory is a no-op success.
        let tgt = TempDir::new().unwrap();
        let result =
            deploy_agents(Path::new("/nonexistent/trusty-mpm/agents"), tgt.path()).unwrap();
        assert_eq!(result, DeployResult::default());
    }

    #[test]
    fn deploy_aborts_on_corrupt_manifest() {
        // A corrupt manifest file must cause deploy_agents to return an error
        // instead of silently resetting to empty and reclassifying managed
        // files as user-owned.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Write a malformed manifest to the target directory.
        fs::write(
            tgt.path().join(crate::core::agent_manifest::MANIFEST_FILE),
            b"not valid json{{{",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path());
        assert!(
            result.is_err(),
            "corrupt manifest must cause an error, not a silent reset to empty"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("corrupt") || err_msg.contains("repair"),
            "error message must mention corruption and repair: {err_msg}"
        );
    }

    #[test]
    fn deploy_injects_initial_prompt_and_tier_model() {
        // A deployed agent that declares a `resource_tier` but no `model`, and
        // an engineer `role` but no `initialPrompt`, must land on disk with both
        // deploy-time enrichments applied (HR-1).
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        fs::write(
            src.path().join("base-engineer.md"),
            "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE ENG CONTENT\n",
        )
        .unwrap();
        fs::write(
            src.path().join("heavy-eng.md"),
            "---\nname: heavy-eng\nrole: engineer\nextends: base-engineer\nresource_tier: intensive\n---\n\n# Heavy\n\nLEAF CONTENT\n",
        )
        .unwrap();

        deploy_agents(src.path(), tgt.path()).unwrap();

        let deployed = fs::read_to_string(tgt.path().join("heavy-eng.md")).unwrap();
        assert!(
            deployed.contains("model: opus"),
            "intensive tier must inject opus on deploy; got:\n{deployed}"
        );
        assert!(
            deployed.contains(r#"initialPrompt: "Begin implementation."#),
            "engineer role must inject implementation initialPrompt on deploy; got:\n{deployed}"
        );
        // Inherited base content survives composition + deploy.
        assert!(deployed.contains("BASE ENG CONTENT"));
        assert!(deployed.contains("LEAF CONTENT"));
    }

    #[test]
    fn deploy_preserves_explicit_model_and_prompt() {
        // Explicit `model` and explicit `initialPrompt` in the source must
        // survive deploy unchanged (explicit always wins over enrichment).
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        fs::write(
            src.path().join("pinned.md"),
            "---\nname: pinned\nrole: engineer\nresource_tier: intensive\nmodel: haiku\ninitialPrompt: Custom start.\n---\n\n# Pinned\n",
        )
        .unwrap();

        deploy_agents(src.path(), tgt.path()).unwrap();

        let deployed = fs::read_to_string(tgt.path().join("pinned.md")).unwrap();
        assert!(
            deployed.contains("model: haiku"),
            "explicit model wins:\n{deployed}"
        );
        assert!(!deployed.contains("model: opus"));
        assert!(
            deployed.contains(r#"initialPrompt: "Custom start.""#),
            "explicit prompt wins:\n{deployed}"
        );
        assert!(!deployed.contains("Begin implementation."));
    }

    #[test]
    fn deploy_filtered_respects_predicate() {
        // HR-2: a selection predicate must restrict which source agents deploy.
        // Only the accepted agent lands; the rejected one is never written.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // base-agent.md + engineer.md

        let result =
            deploy_agents_filtered(src.path(), tgt.path(), |name| name == "engineer").unwrap();

        // engineer.md deployed; base-agent.md filtered out and not written.
        assert!(result.deployed.contains(&"engineer.md".to_string()));
        assert!(!result.deployed.contains(&"base-agent.md".to_string()));
        assert!(tgt.path().join("engineer.md").exists());
        assert!(!tgt.path().join("base-agent.md").exists());
    }

    #[test]
    fn declared_skills_populated_for_every_processed_agent() {
        // DOC-42: an agent declaring `skills:` must have that list recorded
        // in `declared_skills`, keyed by agent name (stem).
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        fs::write(
            src.path().join("code-critic.md"),
            "---\nname: code-critic\nrole: qa\nskills: [code-review-standards, systematic-debugging]\n---\n\n# Critic\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert_eq!(
            result.declared_skills.get("code-critic"),
            Some(&vec![
                "code-review-standards".to_string(),
                "systematic-debugging".to_string()
            ])
        );
    }

    #[test]
    fn declared_skills_empty_when_agent_declares_none() {
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path()); // base-agent.md + engineer.md, neither declares skills

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert_eq!(result.declared_skills.get("engineer"), Some(&Vec::new()));
    }

    #[test]
    fn declared_skills_populated_even_when_deploy_is_skipped() {
        // The declaration is a property of the SOURCE composition, not of
        // this run's write outcome — even a user-modified (skipped) agent's
        // declared skills must still surface for co-deployment purposes.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        fs::write(
            src.path().join("code-critic.md"),
            "---\nname: code-critic\nrole: qa\nskills: [code-review-standards]\n---\n\n# Critic\n",
        )
        .unwrap();
        deploy_agents(src.path(), tgt.path()).unwrap();
        fs::write(
            tgt.path().join("code-critic.md"),
            "---\nname: code-critic\n---\n\nUSER HAND-EDIT\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(result.skipped.contains(&"code-critic.md".to_string()));
        assert_eq!(
            result.declared_skills.get("code-critic"),
            Some(&vec!["code-review-standards".to_string()])
        );
    }

    #[test]
    fn deploy_isolates_single_malformed_agent_failure() {
        // Issue #2906 review (CRITICAL finding): one malformed agent asset
        // (here, unterminated frontmatter — missing closing `---`) must NOT
        // abort the entire roster deploy. The well-formed sibling agent must
        // still deploy, and the failure must be recorded (not silently
        // dropped) rather than propagated as an `Err` from `deploy_agents`.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        fs::write(
            src.path().join("broken.md"),
            "---\nname: broken\n\n# No closing fence\n",
        )
        .unwrap();
        fs::write(
            src.path().join("good.md"),
            "---\nname: good\nrole: engineer\n---\n\n# Good\n\nGOOD BODY\n",
        )
        .unwrap();

        let result = deploy_agents(src.path(), tgt.path())
            .expect("a single malformed agent must not abort the whole deploy");

        assert!(result.deployed.contains(&"good.md".to_string()));
        assert!(tgt.path().join("good.md").is_file());
        assert!(!tgt.path().join("broken.md").exists());
        assert_eq!(result.failed.len(), 1);
        assert!(result.failed[0].starts_with("broken:"));
    }

    #[test]
    fn deploy_content_file_is_atomic() {
        // After a successful deploy no stale .tmp file should remain in the
        // target directory — the atomic rename must have completed.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        deploy_agents(src.path(), tgt.path()).unwrap();

        for entry in fs::read_dir(tgt.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tmp"),
                "stale .tmp file found after deploy: {name_str}"
            );
        }
    }
}
