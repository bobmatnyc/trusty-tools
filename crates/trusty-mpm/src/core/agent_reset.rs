//! `tm install --reset-agents` — force-recompose managed agent files.
//!
//! Why: [`crate::core::agent_deployer::deploy_agents_filtered`] is
//! deliberately conservative — a target file absent from the manifest is
//! always treated as potentially user-owned and skipped, with no path back to
//! a managed state (issue #2504). That conservatism is correct as a default,
//! but it means a fleet of composed agent files deployed before per-file
//! manifest tracking existed can never receive bundle updates again. This
//! module provides the explicit, operator-invoked escape hatch: force
//! every targeted agent back to the current bundle's composition, preserving
//! any content that cannot be proven to already be a fresh copy by backing it
//! up first.
//! What: [`reset_agents`] recomposes every requested source agent (default:
//! all of them) and writes it into `target_dir`, registering the result in
//! the deploy manifest. A target file whose content already matches the
//! fresh composition is adopted without a rewrite; a target file whose
//! content differs from BOTH the manifest's last-known checksum (if any) AND
//! the fresh composition is backed up to `<file>.bak-<unix_nanos>` before
//! being overwritten, so no content is ever silently discarded.
//! Test: `reset_writes_all_by_default`, `reset_filters_by_name`,
//! `reset_adopts_matching_untracked_file`, `reset_backs_up_diverged_file`,
//! `reset_recomposes_without_backup_when_matching_manifest_checksum`,
//! `reset_reports_unknown_names`.

use std::path::{Path, PathBuf};

use crate::core::agent_builder::{AgentBuildError, compose_agent, source_chain};
use crate::core::agent_deployer::is_agent_file;
use crate::core::agent_manifest::{
    AgentManifest, ManifestEntry, ManifestLoad, Origin, atomic_write, checksum,
};

/// Summary of one [`reset_agents`] run.
///
/// Why: the CLI prints a per-file table plus totals; splitting outcomes into
/// named buckets keeps that report mechanical rather than string-sniffed.
/// What: `recomposed` is every filename actually (re)written (a superset of
/// `backed_up`, since a backed-up file is also rewritten); `adopted` is every
/// filename whose content already matched the fresh composition and so was
/// only (re-)registered in the manifest; `backed_up` is the subset of
/// `recomposed` that required preserving prior content first; `not_found` is
/// any requested name with no matching source agent.
/// Test: every `reset_*` test in this module asserts on these vectors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResetResult {
    /// Filenames rewritten from the fresh composition.
    pub recomposed: Vec<String>,
    /// Filenames already matching the fresh composition — registered without
    /// a rewrite.
    pub adopted: Vec<String>,
    /// Filenames backed up (subset of `recomposed`) before being overwritten.
    pub backed_up: Vec<String>,
    /// Requested names with no matching source agent file.
    pub not_found: Vec<String>,
}

/// Suffix template for reset backups: `<file>.bak-<unix_nanos>`.
///
/// Why: naming it once keeps [`backup_path`] and any operator-facing docs in
/// agreement; mirrors the `.old-layout-backup-<unix_nanos>` convention already
/// used for clone-directory migrations
/// ([`crate::daemon::managed_routes::inproject::OLD_LAYOUT_BACKUP_SUFFIX`]) so
/// backup naming stays consistent across the codebase.
/// What: `".bak-"` — the timestamp is appended by [`backup_path`].
/// Test: `reset_backs_up_diverged_file` asserts a sibling matching this
/// pattern is created.
pub const RESET_BACKUP_SUFFIX: &str = ".bak-";

/// Build the backup path for `target_path`, e.g. `engineer.md.bak-<nanos>`.
///
/// Why: factored out so the timestamp source is exercised identically in
/// tests and production.
/// What: appends [`RESET_BACKUP_SUFFIX`] plus nanoseconds-since-epoch to the
/// target's file name, in the same directory.
/// Test: `reset_backs_up_diverged_file`.
fn backup_path(target_path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = target_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent.md".to_string());
    target_path.with_file_name(format!("{name}{RESET_BACKUP_SUFFIX}{ts}"))
}

/// Force-recompose agent files from the current bundle into `target_dir`.
///
/// Why: the explicit, operator-invoked reconciliation path for issue #2504 —
/// bundled BASE-AGENT.md guidance updates are inert for any deployed agent
/// file that predates per-file manifest tracking. This is the only way to
/// bring such a file back under management without manually deleting it.
/// What: `names` restricts the reset to those source-agent stems (e.g.
/// `["engineer", "qa"]`); `None` resets every source agent. For each targeted
/// name: composes fresh content; if no target file exists, writes it and
/// registers it (counted as `recomposed`); if a target file exists and
/// already matches the fresh composition, registers it without a rewrite
/// (`adopted`); if it differs, backs it up first ONLY when its content also
/// differs from the manifest's last-known checksum for that file (or there is
/// no such record), then overwrites it (`recomposed`, plus `backed_up` when a
/// backup was written). The manifest is saved atomically once at the end.
/// Test: `reset_writes_all_by_default`, `reset_filters_by_name`,
/// `reset_adopts_matching_untracked_file`, `reset_backs_up_diverged_file`,
/// `reset_recomposes_without_backup_when_matching_manifest_checksum`,
/// `reset_reports_unknown_names`.
pub fn reset_agents(
    source_dir: &Path,
    target_dir: &Path,
    names: Option<&[String]>,
) -> Result<ResetResult, AgentBuildError> {
    let mut result = ResetResult::default();

    if !source_dir.is_dir() {
        return Ok(result);
    }

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

    let mut available: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file() && is_agent_file(name) {
            available.push(name.trim_end_matches(".md").to_string());
        }
    }
    available.sort_unstable();

    let targets: Vec<String> = match names {
        None => available.clone(),
        Some(requested) => {
            for n in requested {
                if !available.contains(n) {
                    result.not_found.push(n.clone());
                }
            }
            let mut filtered: Vec<String> = available
                .iter()
                .filter(|a| requested.contains(a))
                .cloned()
                .collect();
            filtered.sort_unstable();
            filtered
        }
    };

    for name in targets {
        let filename = format!("{name}.md");
        let composed = compose_agent(&name, source_dir)?;
        let target_path = target_dir.join(&filename);
        let fresh_checksum = checksum(&composed);

        let needs_backup = if target_path.exists() {
            let current = std::fs::read_to_string(&target_path)?;
            let current_checksum = checksum(&current);
            if current_checksum == fresh_checksum {
                // Already the latest composition — adopt without a rewrite.
                manifest.managed.insert(
                    filename.clone(),
                    ManifestEntry {
                        source_chain: source_chain(&name, source_dir)?,
                        checksum: fresh_checksum,
                        deployed_at: now.clone(),
                        origin: Origin::Bundled,
                    },
                );
                result.adopted.push(filename);
                continue;
            }
            let old_checksum = manifest.managed.get(&filename).map(|e| e.checksum.clone());
            old_checksum.as_deref() != Some(current_checksum.as_str())
        } else {
            false
        };

        if needs_backup {
            let dest = backup_path(&target_path);
            std::fs::copy(&target_path, &dest)?;
            tracing::warn!(
                original = %target_path.display(),
                backup = %dest.display(),
                "tm install --reset-agents: preserved diverged content before recompose"
            );
            result.backed_up.push(filename.clone());
        }

        std::fs::create_dir_all(target_dir)?;
        atomic_write(&target_path, &composed).map_err(|e| match e {
            crate::core::error::Error::Io(io) => AgentBuildError::Io(io),
            other => AgentBuildError::FrontmatterParse(other.to_string()),
        })?;
        manifest.managed.insert(
            filename.clone(),
            ManifestEntry {
                source_chain: source_chain(&name, source_dir)?,
                checksum: fresh_checksum,
                deployed_at: now.clone(),
                origin: Origin::Bundled,
            },
        );
        result.recomposed.push(filename);
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
    fn reset_writes_all_by_default() {
        // With no `names` filter, every source agent is (re)composed and
        // registered — this is the "nuke and pave" path for a manifest gap.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let result = reset_agents(src.path(), tgt.path(), None).unwrap();
        assert_eq!(result.recomposed.len(), 2);
        assert!(result.adopted.is_empty());
        assert!(result.backed_up.is_empty());
        assert!(result.not_found.is_empty());

        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));
        assert!(manifest.is_managed("base-agent.md"));
    }

    #[test]
    fn reset_filters_by_name() {
        // Passing a name filter must restrict the reset to that agent only.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let result = reset_agents(src.path(), tgt.path(), Some(&["engineer".to_string()])).unwrap();
        assert_eq!(result.recomposed, vec!["engineer.md".to_string()]);
        assert!(!tgt.path().join("base-agent.md").exists());
    }

    #[test]
    fn reset_reports_unknown_names() {
        // A requested name with no matching source file is reported, not
        // silently dropped, and does not abort the rest of the reset.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        let result = reset_agents(
            src.path(),
            tgt.path(),
            Some(&["engineer".to_string(), "nonexistent".to_string()]),
        )
        .unwrap();
        assert_eq!(result.recomposed, vec!["engineer.md".to_string()]);
        assert_eq!(result.not_found, vec!["nonexistent".to_string()]);
    }

    #[test]
    fn reset_adopts_matching_untracked_file() {
        // An untracked file whose content already equals the fresh
        // composition must be adopted (registered) without a rewrite or
        // backup — this is the silent-adoption half of issue #2504.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());
        let composed = compose_agent("engineer", src.path()).unwrap();
        fs::write(tgt.path().join("engineer.md"), &composed).unwrap();
        let before = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();

        let result = reset_agents(src.path(), tgt.path(), Some(&["engineer".to_string()])).unwrap();
        assert_eq!(result.adopted, vec!["engineer.md".to_string()]);
        assert!(result.recomposed.is_empty());
        assert!(result.backed_up.is_empty());

        let after = fs::metadata(tgt.path().join("engineer.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "adopted file must not be rewritten");

        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));
    }

    #[test]
    fn reset_backs_up_diverged_file() {
        // An untracked file whose content differs from the fresh composition
        // must be backed up before being overwritten — no content loss.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());
        fs::write(
            tgt.path().join("engineer.md"),
            "USER OR STALE CONTENT — not the current bundle\n",
        )
        .unwrap();

        let result = reset_agents(src.path(), tgt.path(), Some(&["engineer".to_string()])).unwrap();
        assert_eq!(result.recomposed, vec!["engineer.md".to_string()]);
        assert_eq!(result.backed_up, vec!["engineer.md".to_string()]);

        // Fresh content lands at the original path.
        let fresh = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(fresh.contains("Engineer content."));

        // A backup sibling preserves the prior content.
        let backups: Vec<_> = fs::read_dir(tgt.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("engineer.md.bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "expected exactly one backup file");
        let backup_content = fs::read_to_string(tgt.path().join(&backups[0])).unwrap();
        assert_eq!(
            backup_content,
            "USER OR STALE CONTENT — not the current bundle\n"
        );

        let manifest = AgentManifest::load(tgt.path());
        assert!(manifest.is_managed("engineer.md"));
    }

    #[test]
    fn reset_recomposes_without_backup_when_matching_manifest_checksum() {
        // A managed file whose content matches the LAST deploy (manifest
        // checksum) but differs from the FRESH composition (the bundle
        // changed) is a routine bundle update — no backup needed, since the
        // content is known to be trusty-mpm's own prior output, not a user
        // edit.
        let src = TempDir::new().unwrap();
        let tgt = TempDir::new().unwrap();
        write_sources(src.path());

        // Establish a normal managed deploy first.
        crate::core::agent_deployer::deploy_agents(src.path(), tgt.path()).unwrap();

        // Bundle changes: engineer.md source now has different body content.
        fs::write(
            src.path().join("engineer.md"),
            "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nUPDATED engineer content.\n",
        )
        .unwrap();

        let result = reset_agents(src.path(), tgt.path(), Some(&["engineer".to_string()])).unwrap();
        assert_eq!(result.recomposed, vec!["engineer.md".to_string()]);
        assert!(
            result.backed_up.is_empty(),
            "no backup expected for a routine bundle update"
        );

        let fresh = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
        assert!(fresh.contains("UPDATED engineer content."));
    }

    #[test]
    fn reset_missing_source_dir_is_empty_result() {
        let tgt = TempDir::new().unwrap();
        let result = reset_agents(
            Path::new("/nonexistent/trusty-mpm/agents"),
            tgt.path(),
            None,
        )
        .unwrap();
        assert_eq!(result, ResetResult::default());
    }
}
