//! Ownership manifest for deployed skill files.
//!
//! Why: the skill deploy step writes skill `.md` files into `~/.claude/skills/`,
//! a directory the user may also drop their own skills into. trusty-mpm must
//! never clobber a user-owned or user-modified file, so it records exactly
//! which files it manages and the content it wrote — mirroring the agent
//! manifest but kept separate so the two ownership records never collide.
//! What: [`SkillManifest`] is a JSON document (`.trusty-mpm-skills-manifest.json`)
//! mapping each deployed filename to a [`SkillManifestEntry`] holding a sha256
//! checksum and the deploy timestamp.
//! Test: `cargo test -p trusty-mpm-core skill_manifest` covers load-of-missing,
//! round-trip save/load, and checksum matching.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::error::Result;

/// Filename of the skill manifest within a target directory.
pub const SKILL_MANIFEST_FILE: &str = ".trusty-mpm-skills-manifest.json";

/// Current on-disk skill manifest schema version.
const SKILL_MANIFEST_VERSION: u32 = 1;

/// One managed skill file's deployment record.
///
/// Why: deploy decisions ("safe to overwrite?", "user-modified?") need the
/// checksum of the content trusty-mpm last wrote.
/// What: the sha256 of the deployed content and the RFC3339 deploy time.
/// Test: `skill_manifest_round_trip`, `skill_manifest_checksum_matches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifestEntry {
    /// sha256 hex digest of the deployed file content.
    pub checksum: String,
    /// RFC3339 timestamp of the deployment.
    pub deployed_at: String,
}

/// The set of skill files trusty-mpm owns in a target directory.
///
/// Why: gives the skill deployer a single source of truth for which files it
/// may safely overwrite without destroying user work.
/// What: a schema version plus a `filename -> entry` map.
/// Test: `skill_manifest_load_missing_returns_empty`, `skill_manifest_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    /// On-disk schema version.
    pub version: u32,
    /// Managed skills keyed by skill name / stem (e.g. `tm-doctor`, not `tm-doctor.md`).
    pub managed: HashMap<String, SkillManifestEntry>,
}

impl Default for SkillManifest {
    fn default() -> Self {
        Self {
            version: SKILL_MANIFEST_VERSION,
            managed: HashMap::new(),
        }
    }
}

impl SkillManifest {
    /// Load the manifest from `target_dir`, defaulting to empty when absent.
    ///
    /// Why: a first-ever deploy has no manifest; treating a missing file as an
    /// empty manifest keeps the deployer's logic uniform.
    /// What: reads `<target_dir>/.trusty-mpm-skills-manifest.json`; a missing or
    /// unparseable file yields a fresh empty manifest.
    /// Test: `skill_manifest_load_missing_returns_empty`, `skill_manifest_round_trip`.
    pub fn load(target_dir: &Path) -> Self {
        let path = target_dir.join(SKILL_MANIFEST_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the manifest to `<target_dir>/.trusty-mpm-skills-manifest.json`
    /// using an atomic write-temp-then-rename strategy.
    ///
    /// Why: a crash within the manifest write must not leave a half-written
    /// manifest on disk — that would silently reclassify managed skills as
    /// user-owned on the next deploy. The temp-then-rename pattern eliminates
    /// this window (same-filesystem rename is atomic on POSIX).
    /// What: creates `target_dir` if needed, serializes to pretty JSON, writes
    /// to `<manifest>.tmp`, then atomically renames onto the final path.
    /// Test: `skill_manifest_round_trip`, `skill_manifest_save_is_atomic`.
    pub fn save(&self, target_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(target_dir)?;
        let path = target_dir.join(SKILL_MANIFEST_FILE);
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(&path, &json)?;
        Ok(())
    }

    /// Whether `filename` is a trusty-mpm-managed skill file.
    ///
    /// Why: files absent from the manifest are user-owned and must never be
    /// touched by the deployer.
    /// What: returns `true` iff the manifest has an entry for `filename`.
    /// Test: `skill_manifest_is_managed`.
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
    /// Test: `skill_manifest_checksum_matches`.
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

    fn sample_entry() -> SkillManifestEntry {
        SkillManifestEntry {
            checksum: checksum("hello world"),
            deployed_at: "2026-05-19T00:00:00Z".into(),
        }
    }

    #[test]
    fn skill_manifest_load_missing_returns_empty() {
        // A directory with no manifest file must yield an empty, valid
        // manifest rather than an error.
        let tmp = TempDir::new().unwrap();
        let manifest = SkillManifest::load(tmp.path());
        assert_eq!(manifest.version, SKILL_MANIFEST_VERSION);
        assert!(manifest.managed.is_empty());
    }

    #[test]
    fn skill_manifest_round_trip() {
        // A saved manifest must reload identically.
        let tmp = TempDir::new().unwrap();
        let mut manifest = SkillManifest::default();
        manifest
            .managed
            .insert("tm-doctor.md".into(), sample_entry());
        manifest.save(tmp.path()).unwrap();

        let loaded = SkillManifest::load(tmp.path());
        assert_eq!(loaded, manifest);
        assert!(tmp.path().join(SKILL_MANIFEST_FILE).exists());
    }

    #[test]
    fn skill_manifest_checksum_matches() {
        // Correct content matches; modified content does not.
        let mut manifest = SkillManifest::default();
        manifest
            .managed
            .insert("tm-doctor.md".into(), sample_entry());
        assert!(manifest.checksum_matches("tm-doctor.md", "hello world"));
        assert!(!manifest.checksum_matches("tm-doctor.md", "hello world!"));
        // An unmanaged file never matches.
        assert!(!manifest.checksum_matches("other.md", "hello world"));
    }

    #[test]
    fn skill_manifest_is_managed() {
        let mut manifest = SkillManifest::default();
        manifest
            .managed
            .insert("tm-doctor.md".into(), sample_entry());
        assert!(manifest.is_managed("tm-doctor.md"));
        assert!(!manifest.is_managed("user-skill.md"));
    }

    #[test]
    fn skill_manifest_file_name_differs_from_agent_manifest() {
        // The skill manifest must use a distinct filename so it never collides
        // with the agent manifest if both ever share a directory.
        assert_ne!(
            SKILL_MANIFEST_FILE,
            crate::core::agent_manifest::MANIFEST_FILE
        );
    }
}
