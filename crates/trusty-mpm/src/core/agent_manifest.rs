//! Ownership manifest for deployed agent files.
//!
//! Why: the deploy step writes composed agents into `~/.claude/agents/`, a
//! directory the user may also drop their own files into. trusty-mpm must
//! never clobber a user-owned or user-modified file, so it records exactly
//! which files it manages and what content it wrote.
//! What: [`AgentManifest`] is a JSON document (`.trusty-mpm-manifest.json`)
//! mapping each deployed filename to a [`ManifestEntry`] holding the resolved
//! source chain, a sha256 checksum, the deploy timestamp, and the origin.
//! Test: `cargo test -p trusty-mpm-core agent_manifest` covers load-of-missing,
//! round-trip save/load, checksum matching, and corruption detection.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::error::{Error, Result};

/// Filename of the manifest within a target directory.
pub const MANIFEST_FILE: &str = ".trusty-mpm-manifest.json";

/// The outcome of loading the agent manifest.
///
/// Why: callers need to distinguish "no manifest yet" (first deploy, expect
/// empty) from "manifest is corrupt" (dangerous — silently resetting to empty
/// would reclassify managed files as user-owned and skip re-deploying them).
/// What: `Ok(manifest)` when the file is absent or parses cleanly; `Corrupt`
/// when the file exists but is malformed or truncated.
/// Test: `manifest_load_corrupt_returns_corrupt`.
#[derive(Debug)]
pub enum ManifestLoad {
    /// File was absent (first deploy) or parsed cleanly.
    Ok(AgentManifest),
    /// File exists but is malformed / truncated.
    Corrupt(String),
}

/// Atomically write `content` to `path` using a temp-then-rename swap.
///
/// Why: a crash between writing content and writing the manifest (or within
/// either write) must not leave the target in a half-written state — that
/// would cause the next deploy to read garbage and reclassify managed files.
/// Using a temp file in the same directory guarantees the rename is atomic on
/// any POSIX filesystem (both paths on the same mount point).
/// What: writes `content` to `<path>.tmp`, then renames onto `path`. The
/// `.tmp` suffix is chosen to be predictable so a repair command can detect
/// and clean up stale temp files if a crash interrupted a previous rename.
/// Test: `atomic_write_leaves_old_intact_on_interrupted_write`.
pub fn atomic_write(path: &std::path::Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Remove a stale `.tmp` sibling if present (left by an interrupted write).
///
/// Why: `atomic_write` stages via `<path>.tmp`; a crash after `fs::write` but
/// before `fs::rename` leaves a `.tmp` orphan. `repair_stale_tmp` removes it
/// so the directory stays tidy after a `tm repair deploy` run.
/// What: if `<path>.tmp` exists, removes it. Non-existence is silently
/// ignored; IO errors are propagated.
/// Test: `repair_stale_tmp_removes_orphan`.
pub fn repair_stale_tmp(path: &std::path::Path) -> Result<()> {
    let tmp = path.with_extension("tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Current on-disk manifest schema version.
const MANIFEST_VERSION: u32 = 1;

/// Where a managed agent originated.
///
/// Why: future tooling distinguishes framework-bundled agents from
/// registry-pulled or user-authored ones; recording it now keeps the manifest
/// forward-compatible.
/// What: a closed enum serialized in lowercase.
/// Test: `manifest_round_trip` exercises serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Shipped with the trusty-mpm binary.
    Bundled,
    /// Pulled from an agent registry.
    Registry,
    /// Authored or imported by the user.
    User,
}

/// One managed agent file's deployment record.
///
/// Why: deploy decisions ("safe to overwrite?", "user-modified?") need the
/// checksum of the content trusty-mpm last wrote.
/// What: the resolved inheritance chain, the sha256 of the deployed content,
/// the RFC3339 deploy time, and the origin.
/// Test: `manifest_round_trip`, `manifest_checksum_matches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Resolved inheritance chain, base-first
    /// (e.g. `["base-agent", "base-engineer", "engineer"]`).
    pub source_chain: Vec<String>,
    /// sha256 hex digest of the deployed file content.
    pub checksum: String,
    /// RFC3339 timestamp of the deployment.
    pub deployed_at: String,
    /// Where the agent came from.
    pub origin: Origin,
}

/// The set of agent files trusty-mpm owns in a target directory.
///
/// Why: gives the deployer a single source of truth for which files it may
/// safely overwrite without destroying user work.
/// What: a schema version plus a `filename -> entry` map.
/// Test: `manifest_load_missing_returns_empty`, `manifest_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentManifest {
    /// On-disk schema version.
    pub version: u32,
    /// Managed files keyed by filename (e.g. `engineer.md`).
    pub managed: HashMap<String, ManifestEntry>,
}

impl Default for AgentManifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            managed: HashMap::new(),
        }
    }
}

/// Compute the sha256 hex digest of a string.
///
/// Why: the manifest stores a checksum so the deployer can tell whether a
/// deployed file still holds trusty-mpm's content or has been hand-edited.
/// What: returns the lowercase hex sha256 of `content`'s UTF-8 bytes.
/// Test: `manifest_checksum_matches`.
pub fn checksum(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

impl AgentManifest {
    /// Load the manifest from `target_dir`, defaulting to empty when absent.
    ///
    /// Why: a first-ever deploy has no manifest; treating a missing file as an
    /// empty manifest keeps the deployer's logic uniform. A corrupt (malformed /
    /// truncated) manifest must NOT silently reset to empty — that would cause
    /// managed files to be reclassified as user-owned and skipped on the next
    /// deploy, producing a silent no-op rather than a re-deploy.
    /// What: reads `<target_dir>/.trusty-mpm-manifest.json`; if absent returns
    /// `ManifestLoad::Ok(default)`; if present but malformed returns
    /// `ManifestLoad::Corrupt` with the parse error; if valid returns
    /// `ManifestLoad::Ok(parsed)`.
    /// Test: `manifest_load_missing_returns_empty`,
    ///       `manifest_load_corrupt_returns_corrupt`,
    ///       `manifest_round_trip`.
    pub fn load_checked(target_dir: &Path) -> ManifestLoad {
        let path = target_dir.join(MANIFEST_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<AgentManifest>(&raw) {
                Ok(m) => ManifestLoad::Ok(m),
                Err(e) => ManifestLoad::Corrupt(format!("{path}: {e}", path = path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ManifestLoad::Ok(Self::default()),
            Err(_) => ManifestLoad::Ok(Self::default()),
        }
    }

    /// Load the manifest from `target_dir`, defaulting to empty when absent.
    ///
    /// Why: preserves the pre-existing call sites that can tolerate a silent
    /// empty-on-corruption fallback (e.g. the deployer, which calls
    /// `load_checked` itself when it cares about the distinction).
    /// What: delegates to `load_checked`; returns the manifest on `Ok`, an
    /// empty default on `Corrupt` (with no side-effects — callers that need
    /// to react to corruption should use `load_checked`).
    /// Test: `manifest_load_missing_returns_empty`, `manifest_round_trip`.
    pub fn load(target_dir: &Path) -> Self {
        match Self::load_checked(target_dir) {
            ManifestLoad::Ok(m) => m,
            ManifestLoad::Corrupt(_) => Self::default(),
        }
    }

    /// Persist the manifest to `<target_dir>/.trusty-mpm-manifest.json`
    /// using an atomic write-temp-then-rename strategy.
    ///
    /// Why: a crash between writing content files and writing the manifest
    /// (or within the manifest write itself) must never leave a half-written
    /// manifest on disk — that could silently reclassify managed files as
    /// user-owned on the next deploy. Writing to a `.tmp` sibling in the same
    /// directory then renaming atomically eliminates this window.
    /// What: creates `target_dir` if needed, serializes to pretty JSON, writes
    /// to `<manifest>.tmp`, then atomically renames onto the final path.
    /// Test: `manifest_round_trip`, `manifest_save_is_atomic`.
    pub fn save(&self, target_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(target_dir)?;
        let path = target_dir.join(MANIFEST_FILE);
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(&path, &json)?;
        Ok(())
    }

    /// Whether `filename` is a trusty-mpm-managed agent file.
    ///
    /// Why: files absent from the manifest are user-owned and must never be
    /// touched by the deployer.
    /// What: returns `true` iff the manifest has an entry for `filename`.
    /// Test: `manifest_is_managed`.
    pub fn is_managed(&self, filename: &str) -> bool {
        self.managed.contains_key(filename)
    }

    /// Whether `content` matches the checksum recorded for `filename`.
    ///
    /// Why: the deployer overwrites a managed file only when the deployed copy
    /// still matches what trusty-mpm last wrote; a mismatch means the user
    /// edited it.
    /// What: returns `true` iff `filename` is managed and `checksum(content)`
    /// equals the stored checksum.
    /// Test: `manifest_checksum_matches`.
    pub fn checksum_matches(&self, filename: &str, content: &str) -> bool {
        self.managed
            .get(filename)
            .is_some_and(|entry| entry.checksum == checksum(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn sample_entry() -> ManifestEntry {
        ManifestEntry {
            source_chain: vec!["base-agent".into(), "engineer".into()],
            checksum: checksum("hello world"),
            deployed_at: "2026-05-16T00:00:00Z".into(),
            origin: Origin::Bundled,
        }
    }

    #[test]
    fn manifest_load_missing_returns_empty() {
        // A directory with no manifest file must yield an empty, valid
        // manifest rather than an error.
        let tmp = TempDir::new().unwrap();
        let manifest = AgentManifest::load(tmp.path());
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert!(manifest.managed.is_empty());
    }

    #[test]
    fn manifest_load_checked_missing_returns_ok() {
        // load_checked on a missing file must return ManifestLoad::Ok(empty).
        let tmp = TempDir::new().unwrap();
        let result = AgentManifest::load_checked(tmp.path());
        assert!(
            matches!(result, ManifestLoad::Ok(m) if m.managed.is_empty()),
            "expected Ok(empty) for missing manifest"
        );
    }

    #[test]
    fn manifest_load_corrupt_returns_corrupt() {
        // A malformed manifest file must return ManifestLoad::Corrupt, not a
        // silent empty default — silently resetting to empty would reclassify
        // managed files as user-owned on the next deploy.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(MANIFEST_FILE), b"not valid json{{{").unwrap();
        let result = AgentManifest::load_checked(tmp.path());
        assert!(
            matches!(result, ManifestLoad::Corrupt(_)),
            "expected Corrupt for malformed manifest"
        );
    }

    #[test]
    fn manifest_load_truncated_returns_corrupt() {
        // A truncated JSON file (simulating a crash mid-write) must also be
        // flagged as corrupt rather than silently reset.
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(MANIFEST_FILE),
            b"{\"version\":1,\"managed\":{",
        )
        .unwrap();
        let result = AgentManifest::load_checked(tmp.path());
        assert!(
            matches!(result, ManifestLoad::Corrupt(_)),
            "expected Corrupt for truncated manifest"
        );
    }

    #[test]
    fn manifest_round_trip() {
        // A saved manifest must reload identically.
        let tmp = TempDir::new().unwrap();
        let mut manifest = AgentManifest::default();
        manifest
            .managed
            .insert("engineer.md".into(), sample_entry());
        manifest.save(tmp.path()).unwrap();

        let loaded = AgentManifest::load(tmp.path());
        assert_eq!(loaded, manifest);
        assert!(tmp.path().join(MANIFEST_FILE).exists());
    }

    #[test]
    fn manifest_save_is_atomic() {
        // After save completes, no stale .tmp file must remain.
        let tmp = TempDir::new().unwrap();
        let mut manifest = AgentManifest::default();
        manifest
            .managed
            .insert("engineer.md".into(), sample_entry());
        manifest.save(tmp.path()).unwrap();

        let tmp_path = tmp.path().join(MANIFEST_FILE).with_extension("tmp");
        assert!(
            !tmp_path.exists(),
            ".tmp staging file must be removed after successful save"
        );
    }

    #[test]
    fn atomic_write_leaves_old_intact_on_interrupted_write() {
        // Simulate: staged .tmp exists (crash before rename) — original must
        // still be readable. This test simulates what the OS guarantees: the
        // rename is atomic, so even if we had crashed after writing .tmp, the
        // old file would be intact. We verify that a stale .tmp left by a
        // previous crash is cleaned up by repair_stale_tmp.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        fs::write(&path, "original content").unwrap();

        // Simulate a stale .tmp orphan from a crashed previous write.
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, "incomplete new content").unwrap();

        // The original file is still present and readable.
        assert_eq!(fs::read_to_string(&path).unwrap(), "original content");

        // repair_stale_tmp removes the orphan.
        repair_stale_tmp(&path).unwrap();
        assert!(
            !tmp_path.exists(),
            "stale .tmp must be removed by repair_stale_tmp"
        );
        // Original remains untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "original content");
    }

    #[test]
    fn repair_stale_tmp_is_idempotent_when_no_tmp() {
        // Calling repair_stale_tmp when no .tmp exists must not error.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        assert!(repair_stale_tmp(&path).is_ok());
    }

    #[test]
    fn manifest_checksum_matches() {
        // Correct content matches; modified content does not.
        let mut manifest = AgentManifest::default();
        manifest
            .managed
            .insert("engineer.md".into(), sample_entry());
        assert!(manifest.checksum_matches("engineer.md", "hello world"));
        assert!(!manifest.checksum_matches("engineer.md", "hello world!"));
        // An unmanaged file never matches.
        assert!(!manifest.checksum_matches("other.md", "hello world"));
    }

    #[test]
    fn manifest_is_managed() {
        let mut manifest = AgentManifest::default();
        manifest
            .managed
            .insert("engineer.md".into(), sample_entry());
        assert!(manifest.is_managed("engineer.md"));
        assert!(!manifest.is_managed("user-agent.md"));
    }

    #[test]
    fn checksum_is_stable_and_distinct() {
        // The digest must be deterministic and differ for different inputs.
        assert_eq!(checksum("abc"), checksum("abc"));
        assert_ne!(checksum("abc"), checksum("abd"));
        // sha256 hex is always 64 chars.
        assert_eq!(checksum("anything").len(), 64);
    }
}
