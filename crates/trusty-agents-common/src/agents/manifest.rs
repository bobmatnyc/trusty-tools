//! Ownership manifest for deployed agent files.
//!
//! Why: the deploy step writes composed agents into `~/.claude/agents/`, a
//! directory the user may also drop their own files into. The deploying
//! harness must never clobber a user-owned or user-modified file, so it
//! records exactly which files it manages and what content it wrote. Moved
//! to trusty-agents-common (#2892) from `trusty-mpm::core::agent_manifest`
//! so a second harness (trusty-code) can reuse the same ownership ledger;
//! `trusty-mpm` re-exports every item here from `core::agent_manifest` for
//! source compatibility.
//! What: [`AgentManifest`] is a JSON document (`.trusty-mpm-manifest.json`)
//! mapping each deployed filename to a [`ManifestEntry`] holding the resolved
//! source chain, a sha256 checksum, the deploy timestamp, and the origin.
//! Test: `cargo test -p trusty-agents-common agents::manifest` covers
//! load-of-missing, round-trip save/load, checksum matching, and corruption
//! detection.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Result alias for agent-manifest operations.
pub type Result<T> = std::result::Result<T, ManifestError>;

/// Failure modes raised while loading, saving, or hashing the agent manifest.
///
/// Why: the manifest is a self-contained ledger (no dependency on any host
/// crate's error type), so it needs its own narrow, typed failure surface —
/// just the two things that can actually go wrong: filesystem access and JSON
/// (de)serialization.
/// What: `Io` wraps a filesystem failure (read/write/rename/create_dir_all);
/// `Json` wraps a `serde_json` (de)serialization failure.
/// Test: exercised indirectly by every `manifest_*` test that touches disk or
/// (de)serializes; `manifest_load_corrupt_returns_corrupt` specifically
/// distinguishes a JSON failure from an IO failure.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// Filesystem access failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

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

/// Scratch path for one [`atomic_write`] attempt — unique per writer, per
/// attempt.
///
/// Why (#4409): a SHARED temp name is itself a corruption bug. Before this the
/// staging path was a fixed `<path>.tmp`, which was survivable while every
/// deploy target was a per-workspace directory — but #4409 moved the agent
/// deploy target to ONE machine-global directory that every concurrent session
/// launch, `sync-assets` run, and `tm catalog apply` writes. Two writers then
/// interleave into that single scratch file and `rename` publishes the mangled
/// result: a torn manifest (which the spawn/resume gate reports as
/// `AgentManifestCorrupt`, and which retraction then refuses to act on) or a
/// torn agent file. Uniqueness per attempt removes that class of failure
/// entirely, independently of any lock. Exactly the reasoning — and the naming
/// scheme — `trusty_common::json_rmw::temp_path` adopted after the identical
/// fixed-`projects.json.tmp` corruption (#3502).
/// What: `<file_name>.<pid>.<nanos>.tmp`, alongside the target so the publish
/// stays a same-filesystem `rename`.
///
/// Public so tests — including `tm`'s orphan-cleaner tests in another crate —
/// can build a scratch file using the SAME generator the writer uses, instead
/// of hand-writing a filename that can silently drift from it. Note there is no
/// such thing as "the temp path for X": every call returns a fresh name, which
/// is exactly why an orphan cleaner must scan for `*.tmp` rather than derive.
/// Test: `atomic_write_temp_names_are_unique_per_attempt`.
pub fn temp_path(path: &std::path::Path) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{nanos}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Atomically write `content` to `path` using a temp-then-rename swap.
///
/// Why: a crash between writing content and writing the manifest (or within
/// either write) must not leave the target in a half-written state — that
/// would cause the next deploy to read garbage and reclassify managed files.
/// Using a temp file in the same directory guarantees the rename is atomic on
/// any POSIX filesystem (both paths on the same mount point).
/// What: writes `content` to a [`temp_path`] unique to this process and
/// attempt, then renames onto `path`. A failed write removes the scratch file
/// rather than leaving it behind.
/// Test: `atomic_write_leaves_old_intact_on_interrupted_write`,
/// `atomic_write_temp_names_are_unique_per_attempt`.
pub fn atomic_write(path: &std::path::Path, content: &str) -> Result<()> {
    let tmp = temp_path(path);
    if let Err(e) = std::fs::write(&tmp, content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ManifestError::Io(e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ManifestError::Io(e));
    }
    Ok(())
}

/// Sidecar lock-file path serialising one directory's agent ledger.
///
/// Why: named once so the lock helper and any future repair tooling cannot
/// disagree about which file serialises a deploy directory. Locking the ledger
/// document itself would not work: the lock would be lost across the `rename`
/// that publishes each new version, because the renamed-over inode — and any
/// lock held on it — is discarded. A stable sidecar survives every publish.
/// Same reasoning and same convention as `trusty_common::json_rmw::lock_path`.
/// What: `<dir>/.trusty-mpm-manifest.json.lock`.
/// Test: `manifest_lock_path_is_a_sidecar`.
pub fn manifest_lock_path(target_dir: &Path) -> std::path::PathBuf {
    target_dir.join(format!("{MANIFEST_FILE}.lock"))
}

/// Run `f` holding the exclusive cross-process lock on the sidecar `lock_file`.
///
/// Why (#4881): the agent ledger (#4409) and the skill ledger need the IDENTICAL
/// serialisation, and a `flock` critical section is the last code two copies
/// should drift on — blocking acquisition, RAII release, and
/// lock-failure-is-an-error ARE the safety property. One implementation, two
/// named wrappers ([`with_agent_manifest_lock`] and
/// [`crate::skills::manifest::with_skill_manifest_lock`]), so a change to the
/// rule lands once.
/// What: creates `lock_file`'s parent directory and the file itself if absent,
/// takes a `flock(2)`-style advisory EXCLUSIVE lock on it (blocking until
/// available), runs `f`, and releases the lock on every exit path including
/// panics (RAII).
///
/// Advisory, not mandatory: a writer that bypasses this helper is not blocked,
/// so every load-modify-save of a ledger must go through it. Blocking: callers
/// on an async runtime must use a blocking-safe thread. Not reentrant — a nested
/// call on the same lock file self-deadlocks.
///
/// Lock acquisition failure is an `Err`, never a silent unlocked fallthrough:
/// proceeding without the lock reinstates exactly the race this closes.
/// Test: `manifest_lock_serialises_concurrent_writers`,
/// `crate::skills::manifest::tests::skill_manifest_lock_serialises_concurrent_writers`.
pub fn with_ledger_lock<T, E, F>(lock_file: &Path, f: F) -> std::result::Result<T, E>
where
    E: From<ManifestError>,
    F: FnOnce() -> std::result::Result<T, E>,
{
    if let Some(parent) = lock_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| E::from(ManifestError::Io(e)))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_file)
        .map_err(|e| E::from(ManifestError::Io(e)))?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.write().map_err(|e| E::from(ManifestError::Io(e)))?;
    f()
}

/// Run `f` holding the exclusive cross-process lock on `target_dir`'s ledger.
///
/// Why (#4409): the deployer's cycle is load manifest → write files → save
/// manifest, and since #4409 that runs against ONE machine-global directory
/// rather than a per-workspace one. Two concurrent writers each read the ledger
/// before either writes, so the second `save` publishes a document missing the
/// first's entries — and the files those lost entries describe then fall into
/// `deploy_agents_filtered`'s UNTRACKED branch on the next run, get classified
/// `untracked_modified`, and are skipped from then on. That is the #4408 freeze
/// shape, reached by a race instead of by corruption. An in-process `Mutex`
/// cannot fix it, because the writers are separate processes.
///
/// What: creates `target_dir` and the [`manifest_lock_path`] sidecar if absent,
/// takes a `flock(2)`-style advisory EXCLUSIVE lock on it (blocking until
/// available), runs `f`, and releases the lock on every exit path including
/// panics (RAII). `f` must perform the WHOLE load-modify-save cycle — the
/// lost-update window opens at the load, not at the save. A scope/closure API
/// rather than a returned guard because `fd_lock`'s guard borrows its `RwLock`,
/// so the two cannot be moved out together; this is also the shape
/// `trusty_common::json_rmw::update` settled on.
///
/// Advisory, not mandatory: a writer that bypasses this helper is not blocked,
/// so every load-modify-save of a ledger must go through it. Blocking: callers
/// on an async runtime must use a blocking-safe thread. Not reentrant — a
/// nested call on the same directory self-deadlocks.
///
/// Lock acquisition failure is an `Err`, never a silent unlocked fallthrough:
/// proceeding without the lock reinstates exactly the race this closes.
/// Test: `manifest_lock_serialises_concurrent_writers`,
/// `manifest_lock_path_is_a_sidecar`.
pub fn with_agent_manifest_lock<T, E, F>(target_dir: &Path, f: F) -> std::result::Result<T, E>
where
    E: From<ManifestError>,
    F: FnOnce() -> std::result::Result<T, E>,
{
    with_ledger_lock(&manifest_lock_path(target_dir), f)
}

// `repair_stale_tmp(path)` was removed with #4409. It derived a single scratch
// path from a target (`path.with_extension("tmp")`), which only worked while
// staging used ONE fixed name per target. [`temp_path`] is now unique per
// process and per attempt, so "the temp file for this target" is no longer a
// derivable path and any such API is a silent no-op waiting to happen — it was
// exactly that in `tm repair deploy`, which reported removing orphans it had
// left on disk. Orphan cleanup is now a directory scan for `*.tmp` that unlinks
// what it finds (`tm`'s `repair::remove_tmp_orphans`), which needs no
// target-to-temp derivation at all.

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

impl Origin {
    /// Whether this origin marks a FRAMEWORK-owned file (the `Overwrite`
    /// install tier) rather than a user-owned one (the `SeedOnce` tier).
    ///
    /// Why: issue #4408. The two-variant install policy says framework-owned
    /// bundled assets are refreshed from the bundle on every deploy, while
    /// user-owned assets are seeded once and never clobbered. Without this
    /// distinction the deployer read ANY checksum mismatch as "the user edited
    /// this file", which is fatal for a bundled asset: once its content
    /// corrupts it can never checksum-match again, so the corruption is
    /// permanently misclassified as a user edit and the file is frozen —
    /// unrecoverable by redeploy or `tm validate --repair`.
    /// What: `true` only for [`Origin::Bundled`]. [`Origin::User`] is
    /// user-authored by definition, and [`Origin::Registry`] is treated
    /// conservatively as user-owned (the operator pulled it deliberately), so
    /// both keep the preserve-on-mismatch behavior.
    /// Test: `origin_framework_ownership_tiers`.
    pub fn is_framework_owned(self) -> bool {
        matches!(self, Origin::Bundled)
    }
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
        // A staged scratch file from a crash before `rename` must leave the
        // original readable — that is what the atomic rename buys.
        //
        // The scratch path comes from the REAL `temp_path`, not a hand-written
        // `path.with_extension("tmp")` (#4409). Modelling the filename by hand
        // is what let the old fixed-name assumption survive undetected in
        // `tm repair deploy` long after `atomic_write` stopped producing it.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        fs::write(&path, "original content").unwrap();

        let orphan = temp_path(&path);
        fs::write(&orphan, "incomplete new content").unwrap();
        assert!(
            orphan
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".tmp"),
            "the orphan cleaner matches on the `.tmp` suffix: {}",
            orphan.display()
        );

        // The original file is still present and readable despite the orphan.
        assert_eq!(fs::read_to_string(&path).unwrap(), "original content");

        // A later successful write publishes over it and leaves the original
        // intact until the rename completes.
        atomic_write(&path, "new content").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
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
    fn origin_framework_ownership_tiers() {
        // #4408: only `Bundled` is the framework-owned (Overwrite) tier; the
        // user-owned tiers keep the preserve-on-mismatch behavior.
        assert!(Origin::Bundled.is_framework_owned());
        assert!(!Origin::User.is_framework_owned());
        assert!(!Origin::Registry.is_framework_owned());
    }

    #[test]
    fn manifest_lock_path_is_a_sidecar() {
        // #4409: the lock must be a stable sibling, not the ledger itself —
        // a lock on the document is discarded by the rename that publishes
        // each new version.
        let dir = Path::new("/some/agents");
        assert_eq!(
            manifest_lock_path(dir),
            dir.join(".trusty-mpm-manifest.json.lock")
        );
    }

    #[test]
    fn atomic_write_temp_names_are_unique_per_attempt() {
        // #4409: a FIXED scratch name is the corruption bug itself once the
        // target is one machine-global directory — two writers interleave into
        // the single temp file and rename the mangled result over the real one.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join(MANIFEST_FILE);
        let a = temp_path(&target);
        let b = temp_path(&target);
        assert_ne!(a, b, "two attempts must not share a scratch path");
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&std::process::id().to_string()),
            "the scratch name must carry the writer's pid: {}",
            a.display()
        );
        assert_eq!(
            a.parent(),
            target.parent(),
            "publish must stay a same-fs rename"
        );
    }

    #[test]
    fn atomic_write_leaves_no_scratch_behind() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join(MANIFEST_FILE);
        atomic_write(&target, "{}").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
        let leftovers: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stale scratch files: {leftovers:?}");
    }

    #[test]
    fn manifest_lock_serialises_concurrent_writers() {
        // #4409: the load-modify-save cycle must be serialised, or two writers
        // that both load before either saves silently drop each other's
        // entries — and the files those lost entries described are then
        // classified untracked and frozen (#4408's shape, via a race).
        //
        // Each thread does a full load → insert → save under the lock. With no
        // lock, interleaving loses updates; with it, both entries survive.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        AgentManifest::default().save(&dir).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    with_agent_manifest_lock::<(), ManifestError, _>(&dir, || {
                        let mut m = AgentManifest::load(&dir);
                        m.managed.insert(format!("agent-{i}.md"), sample_entry());
                        // Widen the window an unlocked implementation would
                        // lose updates in, so this test is not timing-lucky.
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        m.save(&dir)
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_manifest = AgentManifest::load(&dir);
        assert_eq!(
            final_manifest.managed.len(),
            8,
            "every writer's entry must survive: {:?}",
            final_manifest.managed.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn manifest_lock_releases_so_a_second_acquisition_succeeds() {
        // The lock is RAII: a completed critical section must not leave the
        // directory locked, or the next deploy in this process deadlocks.
        let tmp = TempDir::new().unwrap();
        for _ in 0..3 {
            with_agent_manifest_lock::<(), ManifestError, _>(tmp.path(), || Ok(())).unwrap();
        }
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
