//! The record of pull requests this harness opened and has yet to clean up
//! (#7275).
//!
//! Why: the supervisor's periodic trigger has to know WHICH pull requests to
//! ask about. Listing every open PR on the repository would clean up work this
//! harness never opened, and asking GitHub on every sweep for PRs it has
//! already cleaned would re-run a destructive sequence indefinitely. One small
//! append-only file, written by `tm pr open` and stamped by cleanup, answers
//! both: the entries are the harness's own PRs, and a stamped entry is never
//! looked at again.
//!
//! What: [`OpenedPr`] (one entry) and [`CleanupRegistry`] (the file), with
//! [`CleanupRegistry::record_open`] and [`CleanupRegistry::mark_cleaned`] as
//! the only two mutations. Both rewrite the whole file atomically through the
//! crate's shared [`atomic_write`](crate::core::agent_manifest::atomic_write),
//! so a crash mid-write cannot leave a truncated registry that would strand
//! every pending entry.
//!
//! FAIL DIRECTION: toward doing nothing. A registry that cannot be read yields
//! an EMPTY list, so the sweep cleans nothing rather than acting on a guess;
//! `tm pr cleanup <n>` by hand is unaffected, because it takes the PR number
//! from the operator and never consults this file.
//!
//! Test: the sibling `tests.rs` — `registry_*`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Basename of the registry under the framework root.
pub const REGISTRY_FILENAME: &str = "pr-cleanup.json";

/// One pull request `tm pr open` created.
///
/// Test: `registry_round_trips_an_entry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedPr {
    /// The pull-request number.
    pub pr: u64,
    /// `owner/repo` the PR lives in.
    pub repo: String,
    /// The checkout the PR was opened from — where cleanup's git commands run.
    pub repo_root: PathBuf,
    /// When `tm pr open` recorded it.
    pub opened_at: DateTime<Utc>,
    /// When cleanup last ran to completion for it; `None` while pending.
    #[serde(default)]
    pub cleaned_at: Option<DateTime<Utc>>,
}

impl OpenedPr {
    /// Whether the periodic sweep should still consider this entry.
    ///
    /// Test: `registry_pending_excludes_a_cleaned_entry`.
    pub fn pending(&self) -> bool {
        self.cleaned_at.is_none()
    }
}

/// The registry file.
///
/// Test: `registry_round_trips_an_entry`, `registry_record_open_is_idempotent`.
#[derive(Debug, Clone)]
pub struct CleanupRegistry {
    /// Where the JSON lives.
    path: PathBuf,
}

/// The on-disk shape — a versioned wrapper, so a future field addition does not
/// have to guess at a bare array's provenance.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    /// Every PR recorded, newest last.
    #[serde(default)]
    entries: Vec<OpenedPr>,
}

impl CleanupRegistry {
    /// Point at an explicit registry file.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The registry under a framework root (`~/.trusty-mpm` by default).
    pub fn under_root(root: impl AsRef<Path>) -> Self {
        Self::at(root.as_ref().join(REGISTRY_FILENAME))
    }

    /// The production registry, under the real framework root.
    ///
    /// Why: `tm pr open`, `tm pr cleanup` and the supervisor's sweep must all
    /// resolve the SAME file, and each computing `~/.trusty-mpm` for itself is
    /// how they would drift apart.
    /// Test: `sweep_registry_path_is_under_the_framework_root` pins the shape.
    pub fn production() -> Self {
        Self::under_root(crate::core::paths::FrameworkPaths::default().root)
    }

    /// Where this registry reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every recorded entry, newest last.
    ///
    /// Why: see the module's FAIL DIRECTION note — an unreadable or malformed
    /// registry answers EMPTY, so a corrupt file makes the sweep idle rather
    /// than making it act on a partial parse.
    /// Test: `registry_unreadable_file_reads_as_empty`.
    pub fn entries(&self) -> Vec<OpenedPr> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        serde_json::from_str::<RegistryFile>(&raw)
            .map(|f| f.entries)
            .unwrap_or_default()
    }

    /// The entries the periodic sweep should still ask GitHub about.
    ///
    /// Test: `registry_pending_excludes_a_cleaned_entry`.
    pub fn pending(&self) -> Vec<OpenedPr> {
        self.entries()
            .into_iter()
            .filter(OpenedPr::pending)
            .collect()
    }

    /// Record a PR `tm pr open` just created.
    ///
    /// Why: recording the same PR twice would make the sweep clean it twice,
    /// and the second run's `git branch -D` would fail on a branch the first
    /// already deleted — a spurious red for work that succeeded. Keying on the
    /// (repo, pr) pair makes the write idempotent.
    /// What: replaces an existing entry for the same repo and number, else
    /// appends; then rewrites the file atomically.
    /// Test: `registry_record_open_is_idempotent`.
    pub fn record_open(&self, entry: OpenedPr) -> anyhow::Result<()> {
        let mut entries = self.entries();
        entries.retain(|e| !(e.pr == entry.pr && e.repo == entry.repo));
        entries.push(entry);
        self.write(entries)
    }

    /// Stamp a PR as cleaned so no later sweep re-runs it.
    ///
    /// Why: this stamp IS the idempotence of the periodic trigger. Without it
    /// every sweep would see the same merged PR and re-issue the whole
    /// destructive sequence.
    /// What: sets `cleaned_at` on the matching entry and rewrites the file. A
    /// PR that is not in the registry (a hand-run `tm pr cleanup`) is recorded
    /// as already-cleaned, so a later `tm pr open` collision cannot resurrect
    /// it, and no entry is invented for a repo the registry never knew.
    /// Test: `registry_mark_cleaned_stamps_the_entry`,
    /// `registry_mark_cleaned_ignores_an_unknown_pr`.
    pub fn mark_cleaned(&self, repo: &str, pr: u64, at: DateTime<Utc>) -> anyhow::Result<()> {
        let mut entries = self.entries();
        let mut hit = false;
        for e in &mut entries {
            if e.pr == pr && e.repo == repo {
                e.cleaned_at = Some(at);
                hit = true;
            }
        }
        if !hit {
            return Ok(());
        }
        self.write(entries)
    }

    /// Rewrite the whole file atomically.
    fn write(&self, entries: Vec<OpenedPr>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(&RegistryFile { entries })?;
        crate::core::agent_manifest::atomic_write(&self.path, &body)
            .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", self.path.display()))?;
        Ok(())
    }
}
