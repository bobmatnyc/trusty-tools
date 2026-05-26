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
//! round-trip save/load, and checksum matching.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::error::Result;

/// Filename of the manifest within a target directory.
pub const MANIFEST_FILE: &str = ".trusty-mpm-manifest.json";

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
    /// empty manifest keeps the deployer's logic uniform.
    /// What: reads `<target_dir>/.trusty-mpm-manifest.json`; a missing or
    /// unparseable file yields a fresh empty manifest.
    /// Test: `manifest_load_missing_returns_empty`, `manifest_round_trip`.
    pub fn load(target_dir: &Path) -> Self {
        let path = target_dir.join(MANIFEST_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the manifest to `<target_dir>/.trusty-mpm-manifest.json`.
    ///
    /// Why: after a deploy run the manifest must record the files written so
    /// the next run can make safe overwrite decisions.
    /// What: creates `target_dir` if needed and writes pretty-printed JSON.
    /// Test: `manifest_round_trip`.
    pub fn save(&self, target_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(target_dir)?;
        let path = target_dir.join(MANIFEST_FILE);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
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
