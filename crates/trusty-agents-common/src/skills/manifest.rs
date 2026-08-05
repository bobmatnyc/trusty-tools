//! Ownership manifest for deployed skill files.
//!
//! Why: the skill deploy step writes skill `.md` files into `~/.claude/skills/`,
//! a directory the user may also drop their own skills into. trusty-mpm must
//! never clobber a user-owned or user-modified file, so it records exactly
//! which files it manages and the content it wrote — mirroring the agent
//! manifest but kept separate so the two ownership records never collide.
//! Moved to trusty-agents-common (#2892, #2818) from
//! `trusty-mpm::core::skill_manifest` so a second harness (trusty-code) can
//! reuse the same ownership ledger; `trusty-mpm` re-exports every item here
//! from `core::skill_manifest` for source compatibility. Reuses
//! [`crate::agents::manifest::ManifestError`]/`atomic_write`/`checksum` rather
//! than defining a second self-contained error type, since the skill manifest
//! needs exactly the same two failure modes (IO + JSON) the agent manifest
//! already carries.
//! What: [`SkillManifest`] is a JSON document (`.trusty-mpm-skills-manifest.json`)
//! mapping each deployed filename to a [`SkillManifestEntry`] holding a sha256
//! checksum and the deploy timestamp.
//! Test: `cargo test -p trusty-agents-common skills::manifest` covers
//! load-of-missing, round-trip save/load, and checksum matching.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::manifest::{ManifestError, Result, atomic_write, checksum, with_ledger_lock};

/// Filename of the skill manifest within a target directory.
pub const SKILL_MANIFEST_FILE: &str = ".trusty-mpm-skills-manifest.json";

/// Sidecar lock-file path serialising one directory's skill ledger.
///
/// Why (#4881): named once so the lock helper and any future repair tooling
/// cannot disagree about which file serialises a skill deploy directory. Locking
/// the ledger document itself would not work — the lock would be lost across the
/// `rename` that publishes each new version, because the renamed-over inode (and
/// any lock held on it) is discarded. A stable sidecar survives every publish.
/// Same reasoning and convention as [`crate::agents::manifest::manifest_lock_path`].
/// What: `<dir>/.trusty-mpm-skills-manifest.json.lock`.
/// Test: `skill_manifest_lock_path_is_a_sidecar`.
pub fn skill_manifest_lock_path(target_dir: &Path) -> std::path::PathBuf {
    target_dir.join(format!("{SKILL_MANIFEST_FILE}.lock"))
}

/// Run `f` holding the exclusive cross-process lock on `target_dir`'s skill ledger.
///
/// Why (#4881): the skill deploy cycle is load manifest → write skill files →
/// save manifest, and four separate writers run it against the SAME directory —
/// a session launch's deploy, `sync-assets`, `tm catalog apply`'s deploy+prune,
/// and (since #4882) the project-tier deploy triggered by a version change. Two
/// concurrent writers each read the ledger before either writes, so the second
/// `save` publishes a document missing the first's entries. The file a lost
/// entry described still holds the newer bytes, so its recorded checksum now
/// names an OLDER revision than what is on disk, and
/// [`crate::skills::deployer`]'s `deploy_one_file` reads that mismatch as "the
/// user hand-edited this" and skips the file from then on — a skill nobody
/// touched, frozen against every future update. Three such skills were measured
/// frozen on 2026-08-05 with zero hand-authored content in any of them. An
/// in-process `Mutex` cannot fix it, because the writers are separate processes.
///
/// What: delegates to [`with_ledger_lock`] on [`skill_manifest_lock_path`]. `f`
/// must perform the WHOLE load-modify-save cycle — the lost-update window opens
/// at the load, not at the save.
///
/// Advisory and not reentrant; see [`with_ledger_lock`] for the full contract.
/// Test: `skill_manifest_lock_serialises_concurrent_writers`,
/// `skill_manifest_lock_releases_so_a_second_acquisition_succeeds`.
pub fn with_skill_manifest_lock<T, E, F>(target_dir: &Path, f: F) -> std::result::Result<T, E>
where
    E: From<ManifestError>,
    F: FnOnce() -> std::result::Result<T, E>,
{
    with_ledger_lock(&skill_manifest_lock_path(target_dir), f)
}

/// Outcome of a compare-and-swap manifest save.
///
/// Why (#4881): a refused save is not a failure the caller may ignore — it means
/// the ledger moved under an unlocked writer, which is the exact condition that
/// freezes skills. `#[must_use]` makes dropping the answer a compile error.
/// What: `Written` when the snapshot was still current and the document was
/// published; `RefusedStale` when it was not and NOTHING was written.
/// Test: `skill_manifest_save_if_current_refuses_a_stale_snapshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum SkillManifestSave {
    /// The snapshot was current; the manifest was written.
    Written,
    /// On-disk state had moved on since the snapshot; nothing was written.
    RefusedStale,
}

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

    /// Save only if the on-disk manifest is still the one `base` was loaded from.
    ///
    /// Why (#4881): defence in depth behind [`with_skill_manifest_lock`]. The
    /// lock is ADVISORY, so it protects only writers that take it; a future
    /// caller that reaches for plain [`SkillManifest::save`] silently reinstates
    /// the lost-update race. Under the owner's lifecycle ruling this ledger is
    /// the durable record of what the user customized, so a stale write no
    /// longer merely freezes a file — it corrupts that record. Comparing against
    /// disk turns the clobber into a refusal the caller must handle.
    /// What: re-reads the manifest at `target_dir` and writes `self` only when
    /// it equals `base` (the snapshot this manifest was derived from), returning
    /// [`SkillManifestSave::Written`]. Otherwise nothing is written and
    /// [`SkillManifestSave::RefusedStale`] is returned. An absent file loads as
    /// the empty default, so a first-ever deploy compares equal and proceeds.
    ///
    /// This is a CAS, not a merge: it refuses, it never reconciles. Deploy is
    /// idempotent, so the correct response to a refusal is to surface it and let
    /// the next run redo the cycle.
    /// Test: `skill_manifest_save_if_current_refuses_a_stale_snapshot`,
    /// `skill_manifest_save_if_current_writes_when_unchanged`.
    pub fn save_if_current(
        &self,
        target_dir: &Path,
        base: &SkillManifest,
    ) -> Result<SkillManifestSave> {
        if &Self::load(target_dir) != base {
            return Ok(SkillManifestSave::RefusedStale);
        }
        self.save(target_dir)?;
        Ok(SkillManifestSave::Written)
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
#[path = "manifest_tests.rs"]
mod tests;
